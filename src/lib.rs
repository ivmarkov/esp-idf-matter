#![no_std]
#![allow(async_fn_in_trait)]
#![allow(unknown_lints)]
#![allow(renamed_and_removed_lints)]
#![allow(unexpected_cfgs)]
#![allow(clippy::declare_interior_mutable_const)]
#![warn(clippy::large_futures)]
#![warn(clippy::large_stack_frames)]
#![warn(clippy::large_types_passed_by_value)]

#[cfg(feature = "std")]
#[allow(unused_imports)]
#[macro_use]
extern crate std;

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

#[cfg(all(feature = "std", feature = "rs-matter-stack"))]
pub use eth::*;
#[cfg(all(
    not(esp32h2),
    not(esp32s2),
    esp_idf_comp_esp_event_enabled,
    not(esp_idf_btdm_ctrl_mode_br_edr_only),
    esp_idf_bt_enabled,
    esp_idf_bt_bluedroid_enabled,
    feature = "std",
    feature = "rs-matter-stack"
))]
pub use wireless::*;

#[cfg(all(
    not(esp_idf_btdm_ctrl_mode_br_edr_only),
    esp_idf_bt_enabled,
    esp_idf_bt_bluedroid_enabled,
    not(esp32s2)
))]
pub mod ble;
pub mod error;
#[cfg(all(feature = "std", feature = "rs-matter-stack"))]
pub mod eth;
#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
pub mod mdns;
#[cfg(all(
    esp_idf_comp_esp_netif_enabled,
    esp_idf_comp_esp_event_enabled,
    feature = "std",
    feature = "rs-matter-stack"
))]
pub mod netif;
#[cfg(esp_idf_comp_nvs_flash_enabled)]
#[cfg(feature = "rs-matter-stack")]
pub mod persist;
#[cfg(all(
    not(esp32h2),
    not(esp32s2),
    esp_idf_comp_esp_wifi_enabled,
    esp_idf_comp_esp_event_enabled,
    not(esp_idf_btdm_ctrl_mode_br_edr_only),
    esp_idf_bt_enabled,
    esp_idf_bt_bluedroid_enabled,
    feature = "std",
    feature = "rs-matter-stack"
))]
pub mod wireless;

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
    #[cfg(not(feature = "async-io-mini"))]
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
