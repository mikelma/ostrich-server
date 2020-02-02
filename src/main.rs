use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};

use std::io;
use std::sync::Arc;
use std::net::SocketAddr;

use tokio::stream::{StreamExt};
use ostrich_server::{Shared, Message, Peer, DataBase, ServerCommand};

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
    let login_command = match user.read().await {
        Ok(Some(login)) => login.trim().to_string(),
        Ok(None) => {
            println!("Connection losed!");
            return Ok(());
        },
        Err(e) => {
            eprintln!("User error: {}", e);
            return Ok(());
        },
    };

    // Check if the log in command is correct
    let name = match db.lock().await.check_log_in(login_command) {
            Ok(name) => {
                // Notify the user for successful log in
                user.send_command(&ServerCommand::Ok).await?;
                name
            },
            Err(err) => {
                let _ = user.send_command(&ServerCommand::Err(err)).await;
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
            let _ = user.send_command(&ServerCommand::Err(err)).await;
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                                      "A user with the same credentials is already loged in"));
        }
    }

    while let Some(request) = user.next().await {
        match request {

            Ok(Message::Received(mesg)) => {
                // Log the message to the pending list of the user
                // NOTE UNUSED
                db.lock().await.store(&name, mesg)?;
            },

            Ok(Message::Sended(mesg)) => {
                // Split the message
                let input = mesg.clone();
                let mut input = input.trim().split('~');

                // Determine if the user requests a GET
                match input.next() {
                    Some("GET")  => {
                        println!("A get was requested!");
                        // Get pending messages
                        while let Some(mesg) = db.lock().await.get_next(&name) {
                            println!("Sending: {}", mesg);
                            // Send the pending messages to the user
                            if let Err(err) = user.send_command(&ServerCommand::Mesg(mesg)).await {
                                eprintln!("Send error: {}", err);
                            }
                        }
                        user.send_command(&ServerCommand::End).await?;
                    },

                    Some("MSG") => {
                        // Verify sender
                        match input.next() {
                            Some(sender) if sender != name => {
                                eprintln!("Username {} and sender {} does not match", name, sender);
                                continue;
                            },
                            Some(_) => (), // Sender is correct
                            None => eprintln!("Incorrect MSG request by  {}", name),
                        }
                        // Get target name 
                        match input.next() {
                            Some(target) => {
                                // Remove the command part of the message
                                let (_command, mesg) = mesg.split_at(4);
                                db.lock().await.store(&target, mesg.to_string())?
                            },
                            None => eprintln!("Incorrect MSG request by  {}", name),
                        }
                    },

                    Some(_) | None => eprintln!("Unknown request from {}", name),
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
