use serde::Deserialize;
use toml;

use std::fs::File;
use std::io::Read;

#[derive(Deserialize)]
pub struct Config {
    pub ip_address: String,
    pub port: usize,

    pub logger_file: String,
    pub database_file: String,
}

impl Config {

    pub fn new(config_path: &str) -> Result<Config, Box<dyn std::error::Error>> {
        // Read configuration file
        let mut file = File::open(config_path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        // Deserialize the config string
        Ok(toml::from_str(&contents)?)
    }
}
