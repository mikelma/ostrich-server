use tokio::sync::mpsc;
use tokio::net::{TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncRead};
use tokio::stream::{Stream};

use json::{JsonValue, 
    iterators::Entries,
    object::Iter,
};

use std::collections::HashMap;
use std::io::{self, BufReader, prelude::*};
use std::fmt;
use std::fs::File;

use core::task::{Poll, Context};
use core::pin::Pin;

pub type Tx = mpsc::UnboundedSender<String>;
pub type Rx = mpsc::UnboundedReceiver<String>;

pub struct Shared {
    shared: HashMap<String, Tx>,
}

impl Shared {

    pub fn new() -> Shared {
        Shared { shared: HashMap::new()}
    }

    pub fn add(&mut self, name: String, tx: Tx) -> Result<(), io::Error> {
        if self.shared.contains_key(&name) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                                      "A user with the same credentials is already loged in"));
        }
        self.shared.insert(name, tx);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<(), io::Error> {
        match self.shared.remove(name) {
            Some(_) => Ok(()),
            None => Err(io::Error::new(io::ErrorKind::NotFound, 
                                       "User not found")),
        }
    }
    
    /// mesg structure: MSG~sender~target~text
    pub async fn send(&mut self, 
                      command: String, 
                      exclude: &str) -> Result<(), io::Error>{

        let mut mesg = command.split('~'); 

        // Check the MSG command
        match mesg.next() {
            Some(c) if c == "MSG" => (),
            Some(_) | None => return Err(io::Error::new(io::ErrorKind::InvalidInput, 
                                              "Incorrect MSG command")),
        };

        // Ignore sender part of the MSG command
        let _ = mesg.next();

        // Get target's name
        let target = match mesg.next() {
            Some(t) => t,
            None => return Err(io::Error::new(io::ErrorKind::InvalidInput, 
                                              "Wrong target")),
        }; 
        // Get the target's user
        let target = match self.shared.get_mut(&target.to_string()) {
            Some(t) => t,
            None => return Err(io::Error::new(io::ErrorKind::InvalidInput, 
                                              "Wrong target")),
        };
        
        // Get the actual message
        // NOTE: Do not accept empty messages
        let mesg = match mesg.next() {
            Some(mesg) => mesg,
            None => return Err(io::Error::new(io::ErrorKind::InvalidInput, 
                                              "Cannot get the message from command")),
        }; 

        // Send the message, MSG~sender~message
        if let Err(_) = target.send(command) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, 
                                      "Cannot transmit data to users"));
        }

        /*
        for (username, tx) in &self.shared {
            if username != exclude {
                if let Err(_) = tx.send(mesg.clone()) {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, 
                                              "Cannot transmit data to users"));
                }
            }
        }
        */
        Ok(())
    }
}

pub enum Message {
    Received(String), // The user has received something
    Sended(String), // The user has something to say
}

pub enum ServerCommand {
    Ok,
    Err(io::Error),
    Mesg(String),
    End,
}

impl fmt::Display for ServerCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerCommand::Ok => write!(f, "OK"),
            ServerCommand::Err(err) => write!(f, "ERR~{}", err),
            ServerCommand::Mesg(m) => write!(f, "MSG~{}", m),
            ServerCommand::End => write!(f, "END"),
        }
    }
}

pub struct Peer {
    socket: TcpStream,
    rx: Rx,
}

impl Peer {

    pub fn new(socket: TcpStream, rx: Rx) -> Peer {
        Peer{ socket, rx }
    }

    pub async fn write(&mut self, mesg: String) -> Result<usize, io::Error> {
        self.socket.write(mesg.as_bytes()).await
    }

    pub async fn send_command(&mut self, command: &ServerCommand) -> Result<usize, io::Error> {
        self.socket.write(command.to_string().as_bytes()).await
    }

    pub async fn read(&mut self) -> Result<Option<String>, io::Error> {
        
        let mut buffer = [0u8;1024];
        let n = self.socket.read(&mut buffer).await?;

        if n == 0 {
            return Ok(None);
        }

        let data = String::from_utf8_lossy(&buffer[..n]);
            
        Ok(Some(data.to_string()))
    }
    
}

impl Stream for Peer {

    type Item = Result<Message, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, 
                 cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        
        // Check if we have received something
        //if let Poll::Ready(Some(v)) = Pin::new(&mut self.rx).poll_next(cx) {
        //    return Poll::Ready(Some(Ok(Message::Received(v))));
        //}

        // Check if we have received something
        let mut data = [0u8; 1024];
        let n = match Pin::new(&mut self.socket).poll_read(cx, &mut data) {
            Poll::Ready(Ok(n)) => n,
            Poll::Ready(Err(err)) => return Poll::Ready(Some(Err(err))),
            Poll::Pending => return Poll::Pending,
        };
        
        if n > 0 {
            let data = String::from_utf8_lossy(&data[..n]).to_string();
            return Poll::Ready(Some(Ok(Message::Sended(data))));

        } else {
            return Poll::Ready(None);
        }
    }

} 

#[derive(Debug)]
#[derive(PartialEq)]
struct User {
    pub name: String,
    password : String,
}

pub struct DataBase {
    db: Vec<User>,
    pending: HashMap<String, Vec<String>>, // username, vector of pending messages
}

impl DataBase {

    pub fn new(db_path: &str) -> Result<DataBase, io::Error> {
        // Read the db file
        let f = File::open(db_path)?;
        let mut buff = BufReader::new(f);
        let mut contents = String::new();
        buff.read_to_string(&mut contents)?;
        
        // Parse the file to JsonValue
        let parsed = match json::parse(&contents) {
            Ok(db) => db,
            Err(e) => return Err(io::Error::new(io::ErrorKind::Other, e)),
        };

        // Generate db
        let mut db = Vec::new();
        
        for user in parsed["users"].members() {
            let name = match user["name"].as_str() {
                Some(n) => n.to_string(),
                None => continue,
            };

            println!("Init user: {}", name);

            let password = match user["password"].as_str() {
                Some(s) => s.to_string(),
                None => continue,
            };

            // Create the user
            db.push(User {name, password});
        }
        
        let pending = HashMap::new();
        
        Ok(DataBase {db, pending})
    }

    fn get(&self, name: &str) -> Option<&User> {
        self.db.iter()
            .find(|user| user.name == name)
    }

    fn exists(&self, name: &str) -> bool {
        self.db.iter()
            .find(|user| user.name == name)
            .is_some()
    }

    // Returns the username and password from the user input 
    pub fn check_log_in(&self, data: String) -> Result<String, io::Error> {
        // Split the command in sections
        let mut data = data.split('~');

        // Check if the command is USR login command
        match data.next() {
            Some(c) if c == "USR" => (),
            Some(_) | None => return Err(io::Error::new(io::ErrorKind::InvalidInput, 
                                              "Incorrect log in command")),
        };

        let name = match data.next() {
            Some(n) => n,
            None => return Err(io::Error::new(io::ErrorKind::InvalidInput, 
                                              "No username found")),
        };

        let password = match data.next() {
            Some(p) => p,
            None => return Err(io::Error::new(io::ErrorKind::InvalidInput, 
                                              "No password found")),
        };

        // Create a user with the given username and password
        let usr = User {name: name.clone().to_string(), 
            password: password.to_string()};

        // Try to find the requested user in the db
        if !self.db.contains(&usr) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, 
                                      "Wrong username or password"))
        }

        Ok(name.to_string())
    }

    pub fn store(&mut self, name: &str, mesg: String) -> Result<(), io::Error> {
        match self.pending.get_mut(name) {
            Some(p) => {
                p.push(mesg);
                println!("User {}'s pending {} messages", name, p.len());
                Ok(())
            },
            None => {
                // Verify the target existance
                if !self.exists(name) {
                    return Err(io::Error::new(io::ErrorKind::NotFound, 
                                              format!("Target user {} not found", name)))
                    
                }
                // The user is not initialized in the pending list
                // So, init the users's pending list
                let _ = self.pending.insert(name.to_string(), vec![mesg]);
                println!("Loged {} in the pending list", name);
                Ok(())
            },
        }
    }
    
    pub fn get_next(&mut self, name: &str) -> Option<String> {
        match self.pending.get_mut(name) {
            Some(p) if p.len() == 0 => {
                println!("Nothing pending for {}", name);
                None
            }, 
            Some(p) => Some(p.remove(p.len()-1)),
            None => None, 
        }
    }
}

