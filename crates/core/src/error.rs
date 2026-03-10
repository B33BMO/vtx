use thiserror::Error;

#[derive(Debug, Error)]
pub enum VtxError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Pane not found: {0}")]
    PaneNotFound(u32),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("PTY error: {0}")]
    Pty(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, VtxError>;
