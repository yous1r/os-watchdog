use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::Path;
use std::io::Read;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub nodes: Vec<NodeConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerConfig {
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig { port: 30308 }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct NodeConfig {
    pub ip: String,
    pub port: u16,
    pub ssh_port: u16,
    pub group_name: Option<String>,
    pub auth_type: String,
    pub auth_secret: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: ServerConfig::default(),
            nodes: Vec::new(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let mut file = File::open(path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        let config: Config = serde_yaml::from_str(&contents)?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File::create(path)?;
        serde_yaml::to_writer(file, self)?;
        Ok(())
    }
}
