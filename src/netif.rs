use core::borrow::Borrow;
use core::net::{Ipv4Addr, Ipv6Addr};
use core::pin::pin;

use alloc::sync::Arc;
use rs_matter::error::Error;

use std::io;

use edge_nal::UdpBind;
use edge_nal_std::{Stack, UdpSocket};

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::netif::{EspNetif, IpEvent};
use esp_idf_svc::sys::{esp, esp_netif_get_ip6_linklocal, EspError, ESP_FAIL};

use rs_matter::utils::sync::Notification;
use rs_matter_stack::netif::{Netif, NetifConf};

use crate::error::to_net_error;

const TIMEOUT_PERIOD_SECS: u8 = 5;

/// A `Netif` and `UdpBind` traits implementation via ESP-IDF
pub struct EspMatterNetif<T> {
    netif: T,
    sysloop: EspSystemEventLoop,
}

impl<T> EspMatterNetif<T>
where
    T: Borrow<EspNetif>,
{
    /// Create a new `EspMatterNetif` instance
    pub const fn new(netif: T, sysloop: EspSystemEventLoop) -> Self {
        Self { netif, sysloop }
    }

    fn get_conf(&self) -> Result<NetifConf, EspError> {
        Self::get_netif_conf(self.netif.borrow())
    }

    async fn wait_conf_change(&self) -> Result<(), EspError> {
        Self::wait_any_conf_change(&self.sysloop).await
    }

    /// Get the network interface configuration
    pub fn get_netif_conf(netif: &EspNetif) -> Result<NetifConf, EspError> {
        let ip_info = netif.get_ip_info()?;

        let ipv4: Ipv4Addr = ip_info.ip.octets().into();
        if ipv4.is_unspecified() {
            return Err(EspError::from_infallible::<ESP_FAIL>());
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

        Ok(NetifConf {
            ipv4,
            ipv6,
            interface,
            mac,
        })
    }

    /// Wait for any IP configuration change
    pub async fn wait_any_conf_change(sysloop: &EspSystemEventLoop) -> Result<(), EspError> {
        let notification = Arc::new(Notification::<EspRawMutex>::new());

        let _subscription = {
            let notification = notification.clone();

            sysloop.subscribe::<IpEvent, _>(move |_| {
                notification.notify();
            })
        }?;

        let mut events = pin!(notification.wait());
        let mut timer = pin!(Timer::after(Duration::from_secs(TIMEOUT_PERIOD_SECS as _)));

        select(&mut events, &mut timer).await;

        Ok(())
    }
}

impl<T> Netif for EspMatterNetif<T>
where
    T: Borrow<EspNetif>,
{
    async fn get_conf(&self) -> Result<Option<NetifConf>, Error> {
        Ok(EspMatterNetif::get_conf(self).ok())
    }

    async fn wait_conf_change(&self) -> Result<(), Error> {
        EspMatterNetif::wait_conf_change(self)
            .await
            .map_err(to_net_error)?;

        Ok(())
    }
}

impl<T> UdpBind for EspMatterNetif<T>
where
    T: Borrow<EspNetif>,
{
    type Error = io::Error;
    type Socket<'b>
        = UdpSocket
    where
        Self: 'b;

    async fn bind(&self, local: core::net::SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
        Stack::new().bind(local).await
    }
}
