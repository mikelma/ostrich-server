use ostrich_core::Command;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};

use std::io;
use std::sync::Arc;
use std::net::SocketAddr;

use tokio::stream::{StreamExt};
use ostrich_server::{SharedConn, Message, Peer, DataBase};

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    
    // Load the Data Base 
    let db = DataBase::new("db.json")?;
    let db = Arc::new(Mutex::new(db));

    let shared_conn = Arc::new(Mutex::new(SharedConn::new()));

    let addr = "127.0.0.1:9999";

    // Bind a TCP listener to the socket address
    let mut listener = TcpListener::bind(&addr).await?;
    println!("server running on {}", addr);

    loop {
        // Asynchronously wait for an inbound TcpStream.
        let (stream, addr) = listener.accept().await?;

        // Clone a handle to the `ConnectedUsers` state for the new connection.
        let world = Arc::clone(&shared_conn);
        let data = Arc::clone(&db);

        // Spawn our handler to be run asynchronously.
        tokio::spawn(async move {
            if let Err(e) = process(world, data, stream, addr).await {
                println!("User dropped with error, ERROR: {:?}", e);
            }
        });
    }
}

async fn process(shared_conn: Arc<Mutex<SharedConn>>,
                 db: Arc<Mutex<DataBase>>,
                 stream: TcpStream,
                 addr: SocketAddr) -> Result<(), io::Error> {
    
    println!("New connection from : {}", addr);

    // Create a channel
    let (tx, rx) = mpsc::unbounded_channel(); 

    let mut user = Peer::new(stream, rx);

    // Read the log in command from the user and parse to Command
    let login_command = match user.read_command().await {
        Ok(Some(login)) => login,
        Ok(None) => {
            println!("Connection losed!");
            return Ok(());
        },
        Err(e) => {
            eprintln!("User login error: {}", e);
            return Ok(());
        },
    };
    // Check if the log in command is correct.
    // If the username is registered, check password.
    // Else, log in the user as anonymous user.
    let name = match db.lock().await.check_log_in(login_command) {
            Ok(name) => {
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
    // Check if a client with the same user is 
    // already loged in and register the user
    {
        if let Err(err) = shared_conn.lock().await.add(name.clone(), tx) {
            // The user is already loged in... so suspicious
            eprintln!("User {}, error: {}", name, err.to_string()); 
            let _ = user.send_command(&Command::Err(err.to_string())).await?;
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                                      "A user with the same credentials is already loged in"));
        }
    }
    println!("User {} loged in", name);

    while let Some(request) = user.next().await {
        match request {

            Ok(Message::Received(mesg)) => {
                // Send the received message to the user 
                if let Err(err) = user.send_command(&mesg).await {
                    eprintln!("User {} error sending message: {}", name, err);
                }
            },

            Ok(Message::ToSend(mesg)) => {
                // The user wants to send a message
                // The only type of command that the user is allowed to send is MSG
                match mesg {
                    // Send the message to the target 
                    Command::Msg(_,_,_) => {
                        if let Err(err) = shared_conn.lock().await.send(mesg).await {
                            eprintln!("[ERR]: User {} when trying to send data: {}", name, err);
                            // Send error message to the user
                            let command = Command::Err(
                                format!("unable to send message: {}", err));
                            if let Err(err) = user.send_command(&command).await {
                                eprintln!("[ERR]: Cannot send Err command to user {}: {}",
                                          name, err);
                            }
                        }

                    },
                    // Notify that a non valid command is sent
                    _ => {
                        eprintln!("User {} invaid command to send", name);
                        user.send_command(
                            &Command::Err(
                                "Unable to send non MSG command".to_string()
                            )
                        ).await?;
                    },
                }
            },

            Err(err) => {
                eprintln!("Error. User {}: {}", name, err);
            },
        }
    }

    // Delete the user from Shared
    {
        if let Err(err) = shared_conn.lock().await.remove(&name) {
            eprintln!("Error. User {}: {}", name, err); 
        }
        println!("User {} loged out", name);
    }
    
    Ok(())
}
