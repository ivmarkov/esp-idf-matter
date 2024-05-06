#![no_std]
#![allow(async_fn_in_trait)]
#![allow(unknown_lints)]
#![allow(renamed_and_removed_lints)]
#![allow(unexpected_cfgs)]
#![allow(clippy::declare_interior_mutable_const)]
#![warn(clippy::large_futures)]

#[cfg(feature = "std")]
#[allow(unused_imports)]
#[macro_use]
extern crate std;

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

pub use error::*;
#[cfg(all(
    feature = "std",
    esp_idf_comp_nvs_flash_enabled,
    esp_idf_comp_esp_netif_enabled,
    esp_idf_comp_esp_event_enabled
))]
pub use stack::*;

pub mod ble;
mod error;
pub mod mdns;
pub mod multicast;
pub mod netif;
pub mod nvs;
#[cfg(all(
    feature = "std",
    esp_idf_comp_nvs_flash_enabled,
    esp_idf_comp_esp_netif_enabled,
    esp_idf_comp_esp_event_enabled
))]
mod stack;
pub mod wifi;
