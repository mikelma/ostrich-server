use ostrich_core::*;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};

use std::io;
use std::sync::Arc;
use std::net::SocketAddr;
use std::process;

use tokio::stream::{StreamExt};
use ostrich_server::{
    SharedConn, Message, Peer, 
    // NOTE: Renamed to avoid conflic with simplelog::Config
    DataBase, config::Config as ServerConfig 
};

#[macro_use] extern crate log;
extern crate simplelog;

use simplelog::*;

use std::fs::File;

#[tokio::main]
async fn main() -> Result<(), io::Error> {

    let server_config = ServerConfig::new("config.toml")
        .unwrap_or_else(|err| {
            eprintln!("Fatal error reading ostrich-server config file: {}",
                err);
            process::exit(-1);
        });
        
    // Initialize server logger
    if let Err(err) = CombinedLogger::init(vec![
            TermLogger::new(LevelFilter::Trace, Config::default(), 
                TerminalMode::Mixed).unwrap(),
            WriteLogger::new(LevelFilter::Info, Config::default(), 
                File::create(server_config.logger_file.clone()).unwrap())]) {

        eprintln!("Fatal: Could not initialize the logger: {}", err);
        process::exit(-1);
    }

    info!("Ostrich server initialized!");
    info!("logger's output file path: {}", server_config.logger_file);

    // Load the DataBase 
    let db = match DataBase::new(&server_config.database_file) {
        Ok(db) => {
            info!("Database loaded");
            db
        },
        Err(err) => {
            error!("Database loading error: {}", err);
            process::exit(1);
        },
    };
    let db = Arc::new(Mutex::new(db));

    let shared_conn = Arc::new(Mutex::new(SharedConn::new()));

    let addr = format!("{}:{}", 
        server_config.ip_address,
        server_config.port);

    // Bind a TCP listener to the socket address
    let mut listener = TcpListener::bind(&addr).await?;
    info!("server running on {}", addr);

    loop {
        // Asynchronously wait for an inbound TcpStream.
        let (stream, addr) = listener.accept().await?;

        // Clone a handle to the `ConnectedUsers` state for the new connection.
        let world = Arc::clone(&shared_conn);
        let data = Arc::clone(&db);

        // Spawn our handler to be run asynchronously.
        tokio::spawn(async move {
            if let Err(e) = process(world, data, stream, addr).await {
                error!("User dropped with error, ERROR: {:?}", e);
            }
        });
    }
}

async fn process(shared_conn: Arc<Mutex<SharedConn>>,
                 db: Arc<Mutex<DataBase>>,
                 stream: TcpStream,
                 addr: SocketAddr) -> Result<(), io::Error> {
    
    debug!("New connection from : {}", addr);

    // Create a channel
    let (tx, rx) = mpsc::unbounded_channel(); 

    let mut user = Peer::new(stream, rx);

    // Read the log in command from the user and parse to Command
    let login_command = match user.read_command().await {
        Ok(Some(login)) => login,
        Ok(None) => {
            debug!("Connection losed!");
            return Ok(());
        },
        Err(e) => {
            debug!("User login error: {}", e);
            return Ok(());
        },
    };
    // Check if the log in command is correct.
    // If the username is registered, check password.
    // Else, log in the user as anonymous user.
    let name = match db.lock().await.check_log_in_credentials(login_command) {
            Ok(name) => {
                // The crediantials where ok.
                // Check if a client with the same user is 
                // already loged in or register the user
                if let Err(err) = shared_conn.lock().await.add(name.clone(), tx) {
                    // The user is already loged in... so suspicious
                    debug!("User {}, error: {}", name, err.to_string()); 
                    let _ = user.send_command(&Command::Err(err.to_string())).await?;
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                                              "A user with the same credentials is already loged in"));
                }
                // Notify the user for successful log in
                user.send_command(&Command::Ok).await?;
                name
            },
            Err(err) => {
                let _ = user.send_command(&Command::Err(err.to_string())).await;
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, 
                                          format!("Login error: {}", err)));
            },
    };

    debug!("User {} loged in", name);

    while let Some(request) = user.next().await {
        match request {
            Ok(Message::Received(mesg)) => {
                // Send the received message to the target user 
                if let Err(err) = user.send_command(&mesg).await {
                    debug!("User {} error sending message: {}", name, err);
                }
            },
            Ok(Message::ToSend(mesg)) => {
                // The server has received a message from the user,
                // normally its a message to forward to another user or group (MSG commad).
                // If the message is not a MSG command, process the command.
                match mesg {
                    Command::Msg(_,_,_) => {
                        // Send the message to the target 
                        if let Err(err) = shared_conn.lock().await.send(mesg).await {
                            trace!("Error user {} when trying to send data: {}", name, err);
                            // Crate an error command
                            let command = Command::Err(
                                format!("unable to send message: {}", err));
                            // Send error message to the user
                            if let Err(err) = user.send_command(&command).await {
                                debug!("Cannot send Err command to user {}: {}",
                                          name, err);
                            }
                        }
                    },
                    Command::Join(join_name) => {
                        // Determine if the user wants to join another user or a group
                        if join_name.starts_with('#') {
                            trace!("User {} wants to join group: {}", name, join_name);
                            
                            // If the group exists, join the group, else, create it
                            if let Err(err) = shared_conn.lock().await.join_group(&join_name, &name).await {
                                debug!("User {} cannot join {}: {}", name, join_name, err);

                                // Send error to the user
                                let command = Command::Err(
                                    format!("unable to send message: {}", err));
                                if let Err(err) = user.send_command(&command).await {
                                    debug!("Cannot send Err command to user {}: {}",
                                              name, err);
                                }
                            } else {
                                // The user successully joined the group, add the group name to the
                                // list of groups of the user
                                user.groups.push(join_name.clone());
                            }
                        } else {
                            trace!("User {} wants to join user {}", name, join_name);
                        }
                    },
                    Command::Leave(target) => {
                        // The user wants to leave a chat            
                        if target.starts_with('#') {
                            if let Err(err) = shared_conn.lock().await.leave_group(&name, &target).await {
                                warn!("Could not remove user {} from group {}: {}", name, target, err);
                            } else {
                                trace!("User {} left group {}", name, target);
                            }
                        }
                    },
                    Command::ListUsr(gname, _, _) => {
                        // Check if the gname is really a group name (groups starts with #)
                        if !gname.starts_with('#') {
                            debug!("User {} trying to list a non group chat: '{}'", name, gname);
                            // Send error to the user
                            let cmd = Command::Err(
                                format!("Trying to list a non group chat: '{}'", gname));
                            if let Err(err) = user.send_command(&cmd).await {
                                debug!("Cannot send Err command to user {}: {}",
                                          name, err);
                            }
                        }
    
                        trace!("User {} requests listing group: {}", name, gname);
                        if let Ok(usrs_list) = shared_conn.lock().await.list_group(&gname) {
                            for set in usrs_list {
                                let cmd = Command::ListUsr(gname.clone(), ListUsrOperation::Add, set);
                                if let Err(err) = user.send_command(&cmd).await {
                                    debug!("Cannot send MSG command to user {}: {}",
                                              name, err);
                                }
                            }

                        } else {
                            debug!("cannot list group {}", gname);
                        }
                    },
                    // Notify that a non valid command is sent
                    _ => {
                        trace!("User {} invaid command received", name);
                        user.send_command(
                            &Command::Err(
                                "Unable to send non MSG command".to_string()
                            )
                        ).await?;
                    },
                }
            },

            Err(err) => {
                debug!("Error, user {}: {}", name, err);
            },
        }
    }

    // Delete the user from Shared and for every group it's member of
    debug!("User {} loged out", name);

    // Delete user for all the groups is in
    for group in user.groups {
        if let Err(err) = shared_conn.lock().await.leave_group(&name, &group).await {
            warn!("Could not remove user {} from group {}: {}", name, group, err);
        } else {
            trace!("User {} left group {}", name, group);
        }
    }
    // Delete user from shared 
    if let Err(err) = shared_conn.lock().await.remove(&name) {
        debug!("Error, user {}: {}", name, err); 
    }

    
    Ok(())
}
