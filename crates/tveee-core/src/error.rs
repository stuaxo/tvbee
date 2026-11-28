use std::fmt;

/// What went wrong. `Usage` exits 2 (bad invocation), the rest 1 (see
/// [`Error::kind`]). An error can keep its cause: an I/O failure (its kind
/// says timed out, refused, ...: [`Error::timed_out`], [`Error::refused`]),
/// or another error with what was being done ([`Context::context`]).
#[derive(Debug)]
pub enum Error {
    Usage(String),
    Failed(String),
    /// An app's own API isn't there (not running, switched off): callers
    /// can fall back to the generic way (see `provider::Outcome`).
    Unavailable(String),
    /// Reaching something failed: what was being done, and why.
    Io {
        doing: String,
        source: std::io::Error,
    },
    /// `source` happened while doing `doing`.
    Context {
        doing: String,
        source: Box<Error>,
    },
}

/// What an error means for the caller (and the exit code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Usage,
    Failed,
    Unavailable,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// An I/O failure while doing `doing` ("connecting to 192.0.2.1:8080").
    pub fn io(doing: impl Into<String>, source: std::io::Error) -> Error {
        Error::Io { doing: doing.into(), source }
    }

    /// What it means, through any context around it.
    pub fn kind(&self) -> Kind {
        match self {
            Error::Usage(_) => Kind::Usage,
            Error::Unavailable(_) => Kind::Unavailable,
            Error::Context { source, .. } => source.kind(),
            Error::Failed(_) | Error::Io { .. } => Kind::Failed,
        }
    }

    /// The I/O failure underneath, if that's what it was.
    pub fn io_kind(&self) -> Option<std::io::ErrorKind> {
        match self {
            Error::Io { source, .. } => Some(source.kind()),
            Error::Context { source, .. } => source.io_kind(),
            _ => None,
        }
    }

    /// Nothing answered in time.
    pub fn timed_out(&self) -> bool {
        matches!(self.io_kind(), Some(std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock))
    }

    /// The other end refused the connection (nothing listening).
    pub fn refused(&self) -> bool {
        self.io_kind() == Some(std::io::ErrorKind::ConnectionRefused)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Usage(m) | Error::Failed(m) | Error::Unavailable(m) => f.write_str(m),
            Error::Io { doing, source } if doing.is_empty() => write!(f, "{source}"),
            Error::Io { doing, source } => write!(f, "{doing}: {source}"),
            Error::Context { doing, source } => write!(f, "{doing}: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Context { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::io("", e)
    }
}

/// Adding what was being done to an error, keeping it as the cause.
pub trait Context<T> {
    fn context(self, doing: impl Into<String>) -> Result<T>;
}

impl<T> Context<T> for Result<T> {
    fn context(self, doing: impl Into<String>) -> Result<T> {
        self.map_err(|e| Error::Context { doing: doing.into(), source: Box::new(e) })
    }
}

pub fn usage<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Usage(msg.into()))
}

pub fn failed<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Failed(msg.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;
    use std::io::ErrorKind;

    #[test]
    fn causes_kept() {
        let e = Error::io("connecting to 192.0.2.1:8080", std::io::Error::new(ErrorKind::ConnectionRefused, "refused"));
        assert!(e.refused() && !e.timed_out());
        assert_eq!(e.to_string(), "connecting to 192.0.2.1:8080: refused");
        // with context around it: still refused, still Failed, the cause reachable
        let wrapped: Result<()> = Err(e);
        let wrapped = wrapped.context("Kodi JSONRPC.Ping").unwrap_err();
        assert!(wrapped.refused());
        assert_eq!(wrapped.kind(), Kind::Failed);
        assert_eq!(wrapped.to_string(), "Kodi JSONRPC.Ping: connecting to 192.0.2.1:8080: refused");
        assert!(wrapped.source().is_some_and(|s| s.source().is_some()));
        // what it means goes through the context
        let u: Result<()> = usage("bad");
        assert_eq!(u.context("parsing").unwrap_err().kind(), Kind::Usage);
        let t = Error::from(std::io::Error::new(ErrorKind::TimedOut, "timed out"));
        assert!(t.timed_out());
        assert_eq!(t.to_string(), "timed out");
    }
}
