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

    /// Stopping a run that would not stop. Separate from [`JodError::Spawn`]
    /// because a failure to *stop* rendered as "could not start the agent" is a
    /// contradiction printed in the one message a worried reader studies
    /// hardest — the one about an agent that may still be writing to their
    /// files.
    #[error("could not stop the agent: {0}")]
    Kill(String),

    /// A run is supervised by a detached process that reports through the
    /// database, so there is nowhere for its output to go without one. Failing
    /// here is better than launching an agent nobody will ever hear from.
    #[error("this Jod has no store, and a run cannot be observed without one")]
    StoreRequired,

    /// Something the caller asked for cannot mean anything — a cron expression
    /// that does not parse, a timezone that is not in the IANA database, a
    /// policy name nobody defined. Refused when it is written rather than
    /// discovered as silence weeks later.
    #[error("{0}")]
    Invalid(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, JodError>;
