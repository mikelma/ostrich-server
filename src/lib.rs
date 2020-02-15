use ostrich_core::{RawMessage, Command, PCK_SIZE};

use tokio::sync::mpsc;
use tokio::net::{TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncRead};
use tokio::stream::{Stream};

use std::collections::HashMap;
use std::io::{self, BufReader, prelude::*};
use std::fs::File;

use core::task::{Poll, Context};
use core::pin::Pin;

pub type Tx = mpsc::UnboundedSender<Command>;
pub type Rx = mpsc::UnboundedReceiver<Command>;

pub struct SharedConn {
    shared_conn: HashMap<String, Tx>,
}

impl SharedConn {

    pub fn new() -> SharedConn{
        SharedConn{ shared_conn: HashMap::new()}
    }

    pub fn add(&mut self, name: String, tx: Tx) -> Result<(), io::Error> {
        if self.shared_conn.contains_key(&name) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                                      "A user with the same credentials is already loged in"));
        }
        self.shared_conn.insert(name, tx);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<(), io::Error> {
        match self.shared_conn.remove(name) {
            Some(_) => Ok(()),
            None => Err(io::Error::new(io::ErrorKind::NotFound, 
                                       "User not found")),
        }
    }
    
    pub async fn send(&mut self, 
                      command: Command) -> Result<(), io::Error>{

        // Get the target's name from the MSG command
        let target = match &command {
            Command::Msg(_,t,_) => t,
            _ => return Err(io::Error::new(
                    io::ErrorKind::InvalidInput, 
                    "Wrong command type. Only MSG commands can be sended")),
        };

        // Get the target user's tx
        let target_tx = match self.shared_conn.get_mut(&target.to_string()) {
            Some(t) => t,
            None => return Err(io::Error::new(
                    io::ErrorKind::InvalidInput, 
                    format!("Target {} not found", target))),
        };

        // Send the message, MSG~sender~message
        if let Err(_) = target_tx.send(command) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, 
                                      "Cannot transmit data to target"));
        }

        Ok(())
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

    pub async fn send_command(&mut self, command: &Command) -> Result<usize, io::Error> {
        self.socket.write(&RawMessage::to_raw(command)?).await
    }

    pub async fn read_command(&mut self) -> Result<Option<Command>, io::Error> {
        let mut buffer = [0u8;PCK_SIZE];
        let n = self.socket.read(&mut buffer).await?;

        if n == 0 {
            return Ok(None);
        }
        // else 
        let command = RawMessage::from_raw(&buffer)?;
        Ok(Some(command))
    }
    
}

pub enum Message {
    ToSend(Command),
    Received(Command),
}

impl Stream for Peer {

    type Item = Result<Message, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, 
                 cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        
        // Check if we have received something
        if let Poll::Ready(Some(v)) = Pin::new(&mut self.rx).poll_next(cx) {
            return Poll::Ready(Some(Ok(Message::Received(v))));
        }

        // Check if we have received something
        let mut data = [0u8; PCK_SIZE];
        let n = match Pin::new(&mut self.socket).poll_read(cx, &mut data) {
            Poll::Ready(Ok(n)) => n,
            Poll::Ready(Err(err)) => return Poll::Ready(Some(Err(err))),
            Poll::Pending => return Poll::Pending,
        };
        
        if n > 0 {
            let command = RawMessage::from_raw(&data)?;
            return Poll::Ready(Some(Ok(Message::ToSend(command))));

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
        
        Ok(DataBase {db})
    }
    
    // Returns the username and password from the user input 
    pub fn check_log_in_credentials(&self, command: Command) -> Result<String, io::Error> {
        // Check if the command is USR login command, and get username and password
        let (username, password) = match &command {
            Command::Usr(u, p) => (u, p),
            _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, 
                                           "Incorrect log in command")),
        };

        // Create a user with the given username and password
        let usr = User {
            name: username.clone().to_string(),
            password: password.to_string()};

        // Try to find the requested user in the db
        match self.db.iter().position(|x| x.name == usr.name) {
            Some(index) => {
                // The username exists in the db, now check if 
                // the password is also correct
                if self.db[index] == usr {
                    return Ok(username.to_string());
                }
                // If the password does not match
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, 
                                          "Wrong credentials"))
            },
            // The username is not registered in the server.
            // Accept the connection.
            None => return Ok(username.to_string()),
        }
    }
}

