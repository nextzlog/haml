use std::fmt;
use std::io;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Io(io::ErrorKind, String),
    Disconnected,
    Unsupported,
    Timeout,
    Busy,
    Closed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(_, e) => f.write_str(e),
            Error::Disconnected => f.write_str("disconnected"),
            Error::Unsupported => f.write_str("not supported by the device"),
            Error::Timeout => f.write_str("no answer from the device"),
            Error::Busy => f.write_str("another radio is transmitting"),
            Error::Closed => f.write_str("the device thread has stopped"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut => Error::Timeout,
            io::ErrorKind::NotConnected | io::ErrorKind::BrokenPipe => Error::Disconnected,
            kind => Error::Io(kind, e.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_map_to_states() {
        let e = |k| Error::from(io::Error::from(k));
        assert_eq!(e(io::ErrorKind::TimedOut), Error::Timeout);
        assert_eq!(e(io::ErrorKind::BrokenPipe), Error::Disconnected);
        assert!(matches!(e(io::ErrorKind::Other), Error::Io(..)));
        assert_eq!(Error::Busy.to_string(), "another radio is transmitting");
    }
}
