use thiserror::Error;

#[derive(Debug, Error)]
pub enum JodError {
    #[error("harness `{0}` is not installed or could not be found on this machine")]
    HarnessNotFound(String),

    #[error("tmux is required to run agents but was not found on this machine")]
    TmuxNotFound,

    #[error("no agent with id `{0}`")]
    UnknownAgent(String),

    #[error("tmux command failed: {0}")]
    Tmux(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, JodError>;
