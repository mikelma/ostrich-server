use ostrich_core::{Command, RawMessage};

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};

use std::io;
use std::sync::Arc;
use std::net::SocketAddr;

use tokio::stream::{StreamExt};
use ostrich_server::{Shared, Message, Peer, DataBase};

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    
    // Load the Data Base 
    let db = DataBase::new("db.json")?;
    let db = Arc::new(Mutex::new(db));

    let shared = Arc::new(Mutex::new(Shared::new()));

    let addr = "127.0.0.1:9999";

    // Bind a TCP listener to the socket address
    let mut listener = TcpListener::bind(&addr).await?;
    println!("server running on {}", addr);

    loop {
        // Asynchronously wait for an inbound TcpStream.
        let (stream, addr) = listener.accept().await?;

        // Clone a handle to the `Shared` state for the new connection.
        let world = Arc::clone(&shared);
        let data = Arc::clone(&db);

        // Spawn our handler to be run asynchronously.
        tokio::spawn(async move {
            if let Err(e) = process(world, data, stream, addr).await {
                println!("an error occured; error = {:?}", e);
            }
        });
    }
}

async fn process(shared: Arc<Mutex<Shared>>,
                 db: Arc<Mutex<DataBase>>,
                 stream: TcpStream,
                 addr: SocketAddr) -> Result<(), io::Error> {
    
    println!("New connection from : {}", addr);

    // Create a channel
    let (tx, rx) = mpsc::unbounded_channel(); 

    let mut user = Peer::new(stream, rx);

    // let _ = user.write("Welcome to ostrich2\n".to_string()).await?;

    // Read the log in command from the user
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

    // Check if the log in command is correct
    let name = match db.lock().await.check_log_in(login_command) {
            Ok(name) => {
                // Notify the user for successful log in
                user.send_command(&Command::Ok).await?;
                name
            },
            Err(err) => {
                let _ = user.send_command(&Command::Err(err.to_string())).await;
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, 
                                          "Login error: Incorrect username or password"));
            },
    };

    println!("User {} loged in", name);
    
    // Check if a client with the same user is 
    // already loged in and register the user
    {
        if let Err(err) = shared.lock().await.add(name.clone(), tx) {
            // The user is already loged in... so suspicious
            eprintln!("User {}, error: {}", name, err.to_string()); 
            let _ = user.send_command(&Command::Err(err.to_string())).await?;
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                                      "A user with the same credentials is already loged in"));
        }
    }

    while let Some(request) = user.next().await {
        match request {

            Ok(Message::Received(mesg)) => {
                // Send the receiver message to the user 
                let _n = user.send_command(&mesg).await?;
            },

            Ok(Message::ToSend(mesg)) => {
                // The user wants to send a message
                // The only type of command that the user is allowed to send is MSG
                match mesg {
                    // Send the message to the target 
                    Command::Msg(_,_,_) => shared.lock().await.send(mesg).await?,
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
        if let Err(err) = shared.lock().await.remove(&name) {
            eprintln!("Error. User {}: {}", name, err); 
        }
        println!("User {} loged out", name);
    }
    
    Ok(())
}
