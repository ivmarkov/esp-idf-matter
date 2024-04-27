use core::fmt::{self, Display};

use esp_idf_svc::sys::EspError;

#[derive(Debug)]
pub enum Error {
    Matter(rs_matter::error::Error),
    Esp(EspError),
}

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Matter(e) => write!(f, "Matter error: {}", e),
            Error::Esp(e) => write!(f, "ESP error: {}", e),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

impl From<rs_matter::error::Error> for Error {
    fn from(e: rs_matter::error::Error) -> Self {
        Error::Matter(e)
    }
}

impl From<rs_matter::error::ErrorCode> for Error {
    fn from(e: rs_matter::error::ErrorCode) -> Self {
        Error::Matter(e.into())
    }
}

impl From<EspError> for Error {
    fn from(e: EspError) -> Self {
        Error::Esp(e)
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Matter(e.into())
    }
}
