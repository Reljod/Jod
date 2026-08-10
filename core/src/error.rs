use thiserror::Error;

#[derive(Debug, Error)]
pub enum JodError {
    #[error("harness `{0}` is not installed or could not be found on this machine")]
    HarnessNotFound(String),

    #[error("the `jod-run` supervisor was not found; it ships alongside `jod`")]
    SupervisorNotFound,

    #[error("no agent with id `{0}`")]
    UnknownAgent(String),

    #[error("could not start the agent: {0}")]
    Spawn(String),

    /// A run is supervised by a detached process that reports through the
    /// database, so there is nowhere for its output to go without one. Failing
    /// here is better than launching an agent nobody will ever hear from.
    #[error("this Jod has no store, and a run cannot be observed without one")]
    StoreRequired,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, JodError>;
