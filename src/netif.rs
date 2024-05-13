#![cfg(all(esp_idf_comp_esp_netif_enabled, esp_idf_comp_esp_event_enabled))]

use core::fmt;
use core::net::{Ipv4Addr, Ipv6Addr};
use core::pin::pin;

use alloc::sync::Arc;

use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::netif::{EspNetif, IpEvent};
use esp_idf_svc::sys::{esp, esp_netif_get_ip6_linklocal, EspError, ESP_FAIL};

use rs_matter::utils::notification::Notification;

use crate::error::Error;

const TIMEOUT_PERIOD_SECS: u8 = 5;

/// Async trait for accessing the `EspNetif` network interface (netif) of a driver.
///
/// Allows sharing the network interface between multiple tasks, where one task
/// may be waiting for the network interface to be ready, while the other might
/// be mutably operating on the L2 driver below the netif, or on the netif itself.
pub trait NetifAccess {
    /// Waits until the network interface is available and then
    /// calls the provided closure with a reference to the network interface.
    async fn with_netif<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&EspNetif) -> R;

    /// Waits until a certain condition `f` becomes `Some` for a network interface
    /// and then returns the result.
    ///
    /// The condition is checked every 5 seconds and every time a new `IpEvent` is
    /// generated on the ESP IDF system event loop.
    ///
    /// The main use case of this method is to wait and listen the netif for changes
    /// (netif up/down, IP address changes, etc.)
    async fn wait<F, R>(&self, sysloop: EspSystemEventLoop, mut f: F) -> Result<R, Error>
    where
        F: FnMut(&EspNetif) -> Result<Option<R>, Error>,
    {
        let notification = Arc::new(Notification::<EspRawMutex>::new());

        let _subscription = {
            let notification = notification.clone();

            sysloop.subscribe::<IpEvent, _>(move |_| {
                notification.notify();
            })
        }?;

        loop {
            if let Some(result) = self.with_netif(&mut f).await? {
                break Ok(result);
            }

            let mut events = pin!(notification.wait());
            let mut timer = pin!(Timer::after(Duration::from_secs(TIMEOUT_PERIOD_SECS as _)));

            select(&mut events, &mut timer).await;
        }
    }
}

impl<T> NetifAccess for &T
where
    T: NetifAccess,
{
    async fn with_netif<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&EspNetif) -> R,
    {
        (**self).with_netif(f).await
    }
}

impl<T> NetifAccess for &mut T
where
    T: NetifAccess,
{
    async fn with_netif<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&EspNetif) -> R,
    {
        (**self).with_netif(f).await
    }
}

impl NetifAccess for EspNetif {
    async fn with_netif<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&EspNetif) -> R,
    {
        f(self)
    }
}

impl<M, T> NetifAccess for Mutex<M, T>
where
    M: RawMutex,
    T: NetifAccess,
{
    async fn with_netif<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&EspNetif) -> R,
    {
        let netif = self.lock().await;

        netif.with_netif(f).await
    }
}

/// The current configuration of a network interface (if the netif is configured and up)
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetifInfo {
    /// Ipv4 address
    pub ipv4: Ipv4Addr,
    // Ipv6 address
    pub ipv6: Ipv6Addr,
    // Interface index
    pub interface: u32,
    // MAC address
    pub mac: [u8; 6],
}

impl fmt::Display for NetifInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "IPv4: {}, IPv6: {}, Interface: {}, MAC: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.ipv4,
            self.ipv6,
            self.interface,
            self.mac[0],
            self.mac[1],
            self.mac[2],
            self.mac[3],
            self.mac[4],
            self.mac[5]
        )
    }
}

/// Get the MAC, IP addresses and the interface index of the network interface.
///
/// Return an error when some of the IP addresses are unspecified.
pub fn get_info(netif: &EspNetif) -> Result<NetifInfo, Error> {
    let ip_info = netif.get_ip_info()?;

    let ipv4: Ipv4Addr = ip_info.ip.octets().into();
    if ipv4.is_unspecified() {
        return Err(EspError::from_infallible::<ESP_FAIL>().into());
    }

    let mut ipv6: esp_idf_svc::sys::esp_ip6_addr_t = Default::default();

    esp!(unsafe { esp_netif_get_ip6_linklocal(netif.handle() as _, &mut ipv6) })?;

    let ipv6: Ipv6Addr = [
        ipv6.addr[0].to_le_bytes()[0],
        ipv6.addr[0].to_le_bytes()[1],
        ipv6.addr[0].to_le_bytes()[2],
        ipv6.addr[0].to_le_bytes()[3],
        ipv6.addr[1].to_le_bytes()[0],
        ipv6.addr[1].to_le_bytes()[1],
        ipv6.addr[1].to_le_bytes()[2],
        ipv6.addr[1].to_le_bytes()[3],
        ipv6.addr[2].to_le_bytes()[0],
        ipv6.addr[2].to_le_bytes()[1],
        ipv6.addr[2].to_le_bytes()[2],
        ipv6.addr[2].to_le_bytes()[3],
        ipv6.addr[3].to_le_bytes()[0],
        ipv6.addr[3].to_le_bytes()[1],
        ipv6.addr[3].to_le_bytes()[2],
        ipv6.addr[3].to_le_bytes()[3],
    ]
    .into();

    let interface = netif.get_index();

    let mac = netif.get_mac()?;

    Ok(NetifInfo {
        ipv4,
        ipv6,
        interface,
        mac,
    })
}

/// Implementation of `NetifAccess` for the `EspEth` and `AsyncEth` drivers.
#[cfg(esp_idf_comp_esp_eth_enabled)]
#[cfg(any(
    all(esp32, esp_idf_eth_use_esp32_emac),
    any(
        esp_idf_eth_spi_ethernet_dm9051,
        esp_idf_eth_spi_ethernet_w5500,
        esp_idf_eth_spi_ethernet_ksz8851snl
    ),
    esp_idf_eth_use_openeth
))]
pub mod eth {
    use esp_idf_svc::{
        eth::{AsyncEth, EspEth},
        netif::EspNetif,
    };

    use super::NetifAccess;

    impl<'d, T> NetifAccess for EspEth<'d, T> {
        async fn with_netif<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&EspNetif) -> R,
        {
            f(self.netif())
        }
    }

    impl<'d, T> NetifAccess for AsyncEth<EspEth<'d, T>> {
        async fn with_netif<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&EspNetif) -> R,
        {
            f(self.eth().netif())
        }
    }
}
