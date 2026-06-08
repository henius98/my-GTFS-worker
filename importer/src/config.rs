use serde::Deserialize;
use std::fs;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parsing error: {0}")]
    Toml(#[from] toml::de::Error),
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProviderConfig {
    pub name: String,
    #[serde(default = "default")]
    pub is_active: bool,
    pub static_url: String,
    pub static_provider: String,
    pub database_id: String,
}

fn default() -> bool {
    false
}

#[derive(Deserialize, Debug)]
pub struct ProvidersToml {
    pub providers: Vec<ProviderConfig>,
}

pub fn load_config(path: &str) -> Result<ProvidersToml, ConfigError> {
    let providers_toml_str = fs::read_to_string(path)?;
    let config: ProvidersToml = toml::from_str(&providers_toml_str)?;
    Ok(config)
}
