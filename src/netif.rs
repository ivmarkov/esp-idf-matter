use core::net::{Ipv4Addr, Ipv6Addr};
use core::pin::pin;

use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};

use esp_idf_svc::eth::{AsyncEth, EspEth};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::netif::{EspNetif, IpEvent};
use esp_idf_svc::sys::{esp, esp_netif_get_ip6_linklocal, EspError, ESP_FAIL};

use log::info;

use crate::error::Error;

pub trait NetifAccess {
    async fn with_netif<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&EspNetif) -> R;

    async fn wait<F, R>(&self, sysloop: EspSystemEventLoop, mut f: F) -> Result<R, Error>
    where
        F: FnMut(&EspNetif) -> Result<Option<R>, Error>,
    {
        // TODO: Maybe wait on Wifi and Eth events as well
        let mut subscription = sysloop.subscribe_async::<IpEvent>()?;

        loop {
            if let Some(result) = self.with_netif(&mut f).await? {
                break Ok(result);
            }

            let mut events = pin!(subscription.recv());
            let mut timer = pin!(Timer::after(Duration::from_secs(5)));

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

impl<'d, T> NetifAccess for &mut EspEth<'d, T> {
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

pub struct EthNetifAccess<'a, 'd, M, T>(pub &'a Mutex<M, AsyncEth<EspEth<'d, T>>>)
where
    M: RawMutex;

impl<'a, 'd, M, T> NetifAccess for EthNetifAccess<'a, 'd, M, T>
where
    M: RawMutex,
{
    async fn with_netif<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&EspNetif) -> R,
    {
        let eth = self.0.lock().await;

        f(eth.eth().netif())
    }
}

pub fn get_ips(netif: &EspNetif) -> Result<(Ipv4Addr, Ipv6Addr), Error> {
    let ip_info = netif.get_ip_info()?;

    let ipv4: Ipv4Addr = ip_info.ip.octets().into();
    if ipv4.is_unspecified() {
        return Err(EspError::from_infallible::<ESP_FAIL>().into());
    }

    let mut ipv6: esp_idf_svc::sys::esp_ip6_addr_t = Default::default();

    info!("Waiting for IPv6 address");

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

    Ok((ipv4, ipv6))
}
