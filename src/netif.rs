//! This module provides the ESP-IDF implementation of the `Netif` trait for the Matter stack, as
//! well as the `EspMatterNetStack` type alias for a STD stack which is based on `async-io` or `async-io-mini`.

use core::borrow::Borrow;
use core::net::{Ipv4Addr, Ipv6Addr};
use core::pin::pin;

use alloc::sync::Arc;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::netif::{EspNetif, IpEvent};
use esp_idf_svc::sys::{esp, esp_netif_get_ip6_linklocal, EspError};

use rs_matter_stack::matter::dm::clusters::gen_diag::{InterfaceTypeEnum, NetifDiag, NetifInfo};
use rs_matter_stack::matter::dm::networks::NetChangeNotif;
use rs_matter_stack::matter::error::Error;
use rs_matter_stack::matter::utils::sync::Notification;

use crate::error::to_net_error;

const TIMEOUT_PERIOD_SECS: u8 = 5;

/// A network stack for ESP-IDF
pub type EspMatterNetStack = edge_nal_std::Stack;

/// A `Netif` trait implementation for ESP-IDF
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

    /// Get the network interface configuration
    pub fn get_netif_conf<F, R>(netif: &EspNetif, f: F) -> Result<R, EspError>
    where
        F: FnOnce(&NetifInfo) -> Result<R, EspError>,
    {
        let ip_info = netif.get_ip_info()?;

        let ipv4: Ipv4Addr = ip_info.ip.octets().into();
        // if ipv4.is_unspecified() {
        //     return Err(EspError::from_infallible::<ESP_FAIL>());
        // }

        let ipv6 = {
            let mut ipv6: esp_idf_svc::sys::esp_ip6_addr_t = Default::default();
            if esp!(unsafe { esp_netif_get_ip6_linklocal(netif.handle() as _, &mut ipv6) }).is_ok()
            {
                [
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
                .into()
            } else {
                Ipv6Addr::UNSPECIFIED
            }
        };

        let mut mac: [u8; 8] = Default::default();
        mac[..6].copy_from_slice(&netif.get_mac()?);

        f(&NetifInfo {
            name: &netif.get_name(),
            operational: netif.is_up()?,
            offprem_svc_reachable_ipv4: None,
            offprem_svc_reachable_ipv6: None,
            hw_addr: &mac,
            ipv4_addrs: &[ipv4],
            ipv6_addrs: &[ipv6],
            netif_type: InterfaceTypeEnum::Unspecified, // TODO: We can figure this out
            netif_index: netif.get_index(),
        })
    }

    /// Wait for any IP configuration change
    pub async fn wait_any_conf_change(sysloop: &EspSystemEventLoop) -> Result<(), EspError> {
        // TODO: Revert to `EspRawMutex` when `esp-idf-svc` is updated to `embassy-sync 0.7`
        let notification = Arc::new(Notification::<CriticalSectionRawMutex /*EspRawMutex*/>::new());

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

    fn get_conf<F>(&self, f: F) -> Result<(), EspError>
    where
        F: FnOnce(&NetifInfo) -> Result<(), EspError>,
    {
        Self::get_netif_conf(self.netif.borrow(), f)
    }

    async fn wait_conf_change(&self) -> Result<(), EspError> {
        Self::wait_any_conf_change(&self.sysloop).await
    }
}

impl<T> NetifDiag for EspMatterNetif<T>
where
    T: Borrow<EspNetif>,
{
    fn netifs(&self, f: &mut dyn FnMut(&NetifInfo) -> Result<(), Error>) -> Result<(), Error> {
        let mut result = None;
        EspMatterNetif::get_conf(self, |netif| {
            result = Some(f(netif));
            Ok(())
        })
        .map_err(to_net_error)?;

        if let Some(Err(e)) = result {
            Err(e)
        } else {
            Ok(())
        }
    }
}

impl<T> NetChangeNotif for EspMatterNetif<T>
where
    T: Borrow<EspNetif>,
{
    async fn wait_changed(&self) {
        self.wait_conf_change().await.unwrap();
    }
}

#[derive(Debug)]
pub(crate) struct NetifInfoOwned {
    name: heapless::String<6>,
    operational: bool,
    hw_addr: [u8; 8],
    ipv4_addr: Ipv4Addr,
    ipv6_addr: Ipv6Addr,
    netif_type: InterfaceTypeEnum,
    netif_index: u32,
}

impl NetifInfoOwned {
    pub(crate) const fn new() -> Self {
        Self {
            name: heapless::String::new(),
            operational: false,
            hw_addr: [0; 8],
            ipv4_addr: Ipv4Addr::UNSPECIFIED,
            ipv6_addr: Ipv6Addr::UNSPECIFIED,
            netif_type: InterfaceTypeEnum::WiFi,
            netif_index: 0,
        }
    }

    pub(crate) fn is_operational(&self) -> bool {
        self.operational && !self.ipv4_addr.is_unspecified() && !self.ipv6_addr.is_unspecified()
    }

    pub(crate) fn load(&mut self, l2_connected: bool, netif: &EspNetif) -> Result<bool, EspError> {
        EspMatterNetif::<EspNetif>::get_netif_conf(netif, |info| {
            Ok(self.load_from_info(l2_connected, info))
        })
    }

    fn load_from_info(&mut self, l2_connected: bool, info: &NetifInfo<'_>) -> bool {
        let hw_addr: &[u8] = info.hw_addr;

        let changed = self.name != info.name
            || self.operational != info.operational && l2_connected
            || self.hw_addr != hw_addr
            || self.ipv4_addr
                != info
                    .ipv4_addrs
                    .first()
                    .copied()
                    .unwrap_or(Ipv4Addr::UNSPECIFIED)
            || self.ipv6_addr
                != info
                    .ipv6_addrs
                    .first()
                    .copied()
                    .unwrap_or(Ipv6Addr::UNSPECIFIED)
            || self.netif_type != info.netif_type
            || self.netif_index != info.netif_index;

        if changed {
            self.name = info.name.try_into().unwrap();
            self.operational = info.operational && l2_connected;
            self.hw_addr = hw_addr.try_into().unwrap();
            self.ipv4_addr = if info.ipv4_addrs.is_empty() {
                Ipv4Addr::UNSPECIFIED
            } else {
                info.ipv4_addrs[0]
            };
            self.ipv6_addr = if info.ipv6_addrs.is_empty() {
                Ipv6Addr::UNSPECIFIED
            } else {
                info.ipv6_addrs[0]
            };
            self.netif_type = info.netif_type;
            self.netif_index = info.netif_index;
        }

        changed
    }

    pub(crate) fn as_ref<F>(&self, f: F) -> Result<(), Error>
    where
        F: FnOnce(&NetifInfo<'_>) -> Result<(), Error>,
    {
        let ipv4_addrs = [self.ipv4_addr];
        let ipv6_addrs = [self.ipv6_addr];

        f(&NetifInfo {
            name: &self.name,
            operational: self.operational,
            hw_addr: &self.hw_addr,
            ipv4_addrs: if self.ipv4_addr.is_unspecified() {
                &[]
            } else {
                &ipv4_addrs
            },
            ipv6_addrs: if self.ipv6_addr.is_unspecified() {
                &[]
            } else {
                &ipv6_addrs
            },
            netif_type: self.netif_type,
            offprem_svc_reachable_ipv4: None,
            offprem_svc_reachable_ipv6: None,
            netif_index: self.netif_index,
        })
    }
}
