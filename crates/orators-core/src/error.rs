use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OratorsError {
    #[error("unknown bluetooth device: {0}")]
    UnknownDevice(String),
    #[error("another device is already active: {0}")]
    AlreadyActiveDevice(String),
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("io error for {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("toml serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),
}

pub type Result<T> = std::result::Result<T, OratorsError>;
