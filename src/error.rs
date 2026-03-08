use std::fmt;

#[derive(Debug)]
pub enum YeelightError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// A parameter was out of range or semantically invalid.
    InvalidParam(&'static str),
    /// The bulb sent back an unexpected or malformed response.
    Protocol(String),
}

impl fmt::Display for YeelightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e)           => write!(f, "IO error: {e}"),
            Self::Json(e)         => write!(f, "JSON error: {e}"),
            Self::InvalidParam(s) => write!(f, "Invalid parameter: {s}"),
            Self::Protocol(s)     => write!(f, "Protocol error: {s}"),
        }
    }
}

impl std::error::Error for YeelightError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e)   => Some(e),
            Self::Json(e) => Some(e),
            _             => None,
        }
    }
}

impl From<std::io::Error> for YeelightError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}

impl From<serde_json::Error> for YeelightError {
    fn from(e: serde_json::Error) -> Self { Self::Json(e) }
}
