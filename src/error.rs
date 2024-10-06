use esp_idf_svc::sys::EspError;

use rs_matter::error::{Error, ErrorCode};

/// Converts an ESP network error to an `rs-matter` error
pub fn to_net_error(_err: EspError) -> Error {
    // TODO: The `rs-matter` error code is too generic
    // TODO: Capture the backtrace and the original error
    ErrorCode::NoNetworkInterface.into()
}

/// Converts an ESP persistence error to an `rs-matter` error
pub fn to_persist_error(_err: EspError) -> Error {
    // TODO: The `rs-matter` error code is too generic
    // TODO: Capture the backtrace and the original error
    ErrorCode::StdIoError.into()
}
