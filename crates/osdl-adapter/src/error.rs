//! Error type for the live database adapters.

use thiserror::Error;

/// Errors raised while connecting to or mutating a live database.
#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("failed to connect: {0}")]
    Connect(String),

    #[error("DDL execution failed: {0}")]
    Exec(String),

    #[error("invalid migration target: {0}")]
    InvalidTarget(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("JSON (de)serialization error: {0}")]
    Json(String),
}

impl From<sea_orm::DbErr> for AdapterError {
    fn from(e: sea_orm::DbErr) -> Self {
        AdapterError::Exec(e.to_string())
    }
}

impl From<mongodb::error::Error> for AdapterError {
    fn from(e: mongodb::error::Error) -> Self {
        AdapterError::Exec(e.to_string())
    }
}

impl From<std::io::Error> for AdapterError {
    fn from(e: std::io::Error) -> Self {
        AdapterError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for AdapterError {
    fn from(e: serde_json::Error) -> Self {
        AdapterError::Json(e.to_string())
    }
}
