#![cfg(feature = "rs-matter-stack")]

pub use eth::*;
pub use netif::*;
pub use persist::*;
pub use wifible::*;

mod eth;
mod netif;
mod persist;
mod wifible;

/// A utility function to initialize the `async-io` Reactor which is
/// used for IP-based networks (UDP and TCP).
///
/// User is expected to call this method early in the application's lifecycle
/// when there is plenty of task stack space available, as the initialization
/// consumes > 10KB of stack space, so it has to be done with care.
#[inline(never)]
#[cold]
#[cfg(feature = "std")]
pub fn init_async_io() -> Result<(), esp_idf_svc::sys::EspError> {
    // We'll use `async-io` for networking, so ESP IDF VFS needs to be initialized
    esp_idf_svc::io::vfs::initialize_eventfd(3)?;

    esp_idf_svc::hal::task::block_on(init_async_io_async());

    Ok(())
}

#[inline(never)]
#[cold]
#[cfg(feature = "std")]
async fn init_async_io_async() {
    #[cfg(all(feature = "async-io-mini", not(feature = "async-io-mini")))]
    {
        // Force the `async-io` lazy initialization to trigger earlier rather than later,
        // as it consumes a lot of temp stack memory
        async_io::Timer::after(core::time::Duration::from_millis(100)).await;
        ::log::info!("Async IO initialized; using `async-io`");
    }

    #[cfg(feature = "async-io-mini")]
    {
        // Nothing to initialize for `async-io-mini`
        ::log::info!("Async IO initialized; using `async-io-mini`");
    }
}
