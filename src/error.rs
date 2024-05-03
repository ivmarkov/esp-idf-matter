use core::fmt::{self, Display};

use embassy_sync::mutex::TryLockError;
use esp_idf_svc::sys::EspError;

/// The error used throughout this crate.
/// A composition of `rs_matter::error::Error` and `EspError`.
#[derive(Debug)]
pub enum Error {
    Matter(rs_matter::error::Error),
    Esp(EspError),
    InvalidState,
}

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Matter(e) => write!(f, "Matter error: {}", e),
            Error::Esp(e) => write!(f, "ESP error: {}", e),
            Error::InvalidState => write!(f, "Invalid state"),
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

impl From<TryLockError> for Error {
    fn from(_: TryLockError) -> Self {
        Error::InvalidState
    }
}

impl From<rs_matter::utils::ifmutex::TryLockError> for Error {
    fn from(_: rs_matter::utils::ifmutex::TryLockError) -> Self {
        Error::InvalidState
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Matter(e.into())
    }
}
