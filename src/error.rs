use std::fmt;

/// `Usage` exits 2 (bad invocation), `Failed` exits 1 (runtime problem).
#[derive(Debug)]
pub enum Error {
    Usage(String),
    Failed(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Usage(m) | Error::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Failed(e.to_string())
    }
}

pub fn usage<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Usage(msg.into()))
}

pub fn failed<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Failed(msg.into()))
}
