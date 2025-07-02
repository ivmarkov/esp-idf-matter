//! This module provides the ESP-IDF Wifi implementation of the Matter `NetCtl`, `NetChangeNotif`, `WirelessDiag`, and `WifiDiag` traits.

use core::cell::RefCell;
use core::net::{Ipv4Addr, Ipv6Addr};
use core::time::Duration;

extern crate alloc;

use embassy_sync::blocking_mutex::{self, raw::CriticalSectionRawMutex};
use embassy_sync::mutex::Mutex;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::handle::RawHandle as _;
use esp_idf_svc::netif::EspNetif;
use esp_idf_svc::sys::{esp, EspError};
use esp_idf_svc::wifi::{
    AsyncWifi, AuthMethod, ClientConfiguration, Configuration::Client, EspWifi,
};

use rs_matter_stack::matter::dm::clusters::gen_diag::{InterfaceTypeEnum, NetifDiag, NetifInfo};
use rs_matter_stack::matter::dm::clusters::net_comm::{
    NetCtl, NetCtlError, NetworkScanInfo, NetworkType, WirelessCreds,
};
use rs_matter_stack::matter::dm::clusters::net_comm::{WiFiBandEnum, WiFiSecurityBitmap};
use rs_matter_stack::matter::dm::clusters::wifi_diag::{
    SecurityTypeEnum, WiFiVersionEnum, WifiDiag, WirelessDiag,
};
use rs_matter_stack::matter::dm::networks::NetChangeNotif;
use rs_matter_stack::matter::error::{Error, ErrorCode};
use rs_matter_stack::matter::tlv::Nullable;
use rs_matter_stack::matter::utils::sync::Notification;

use crate::error::to_net_error;
use crate::netif::EspMatterNetif;

/// This type provides the ESP-IDF Wifi implementation of the Matter `NetCtl`, `NetChangeNotif`, `WirelessDiag`, and `WifiDiag` traits
// TODO: Revert to `EspRawMutex` when `esp-idf-svc` is updated to `embassy-sync 0.7`
pub struct EspMatterWifiCtl<'a> {
    wifi: Mutex<CriticalSectionRawMutex /*EspRawMutex*/, AsyncWifi<EspWifi<'a>>>,
    netif_state: blocking_mutex::Mutex<
        CriticalSectionRawMutex, /*EspRawMutex*/
        RefCell<NetifInfoOwned>,
    >,
    netif_state_changed: Notification<CriticalSectionRawMutex /*EspRawMutex*/>,
    sysloop: EspSystemEventLoop,
}

impl<'a> EspMatterWifiCtl<'a> {
    /// Create a new instance of the `EspMatterWifiCtl` type.
    pub const fn new(wifi: AsyncWifi<EspWifi<'a>>, sysloop: EspSystemEventLoop) -> Self {
        Self {
            wifi: Mutex::new(wifi),
            netif_state: blocking_mutex::Mutex::new(RefCell::new(NetifInfoOwned::new())),
            netif_state_changed: Notification::new(),
            sysloop,
        }
    }

    fn load(&self, wifi: &EspWifi<'_>) -> Result<(), EspError> {
        self.netif_state.lock(|state| {
            if state.borrow_mut().load(wifi)? {
                self.netif_state_changed.notify();
            }

            Ok(())
        })
    }
}

impl NetCtl for EspMatterWifiCtl<'_> {
    fn net_type(&self) -> NetworkType {
        NetworkType::Wifi
    }

    async fn scan<F>(&self, network: Option<&[u8]>, mut f: F) -> Result<(), NetCtlError>
    where
        F: FnMut(&NetworkScanInfo) -> Result<(), Error>,
    {
        let mut wifi = self.wifi.lock().await;

        if !wifi.is_started().map_err(to_net_error)? {
            wifi.start().await.map_err(to_net_error)?;
        }

        for ap in wifi.scan().await.map_err(to_net_error)? {
            if network
                .map(|network| ap.ssid.as_bytes() == network)
                .unwrap_or(true)
            {
                f(&NetworkScanInfo::Wifi {
                    security: if let Some(auth_method) = ap.auth_method {
                        match auth_method {
                            AuthMethod::None => WiFiSecurityBitmap::UNENCRYPTED,
                            AuthMethod::WEP => WiFiSecurityBitmap::WEP,
                            AuthMethod::WPA => WiFiSecurityBitmap::WPA_PERSONAL,
                            AuthMethod::WPA2Personal => WiFiSecurityBitmap::WPA_2_PERSONAL,
                            AuthMethod::WPAWPA2Personal => {
                                WiFiSecurityBitmap::WPA_PERSONAL
                                    | WiFiSecurityBitmap::WPA_2_PERSONAL
                            }
                            AuthMethod::WPA3Personal => WiFiSecurityBitmap::WPA_3_PERSONAL,
                            AuthMethod::WPA2WPA3Personal => {
                                WiFiSecurityBitmap::WPA_2_PERSONAL
                                    | WiFiSecurityBitmap::WPA_3_PERSONAL
                            }
                            _ => WiFiSecurityBitmap::empty(),
                        }
                    } else {
                        WiFiSecurityBitmap::empty()
                    },
                    ssid: ap.ssid.as_bytes(),
                    bssid: &ap.bssid,
                    channel: ap.channel as _,
                    band: WiFiBandEnum::V2G4,
                    rssi: ap.signal_strength,
                })?;
            }
        }

        Ok(())
    }

    async fn connect(&self, creds: &WirelessCreds<'_>) -> Result<(), NetCtlError> {
        let WirelessCreds::Wifi { ssid, pass } = creds else {
            return Err(NetCtlError::Other(ErrorCode::InvalidData.into()));
        };

        let mut wifi = self.wifi.lock().await;

        self.load(wifi.wifi()).map_err(to_net_error)?;

        let mut result = Ok(());

        let mut conf = Client(ClientConfiguration {
            ssid: core::str::from_utf8(ssid)
                .map_err(to_net_constr_error)?
                .try_into()
                .map_err(to_net_constr_error)?,
            password: core::str::from_utf8(pass)
                .map_err(to_net_constr_error)?
                .try_into()
                .map_err(to_net_constr_error)?,
            auth_method: AuthMethod::None,
            ..Default::default()
        });

        for auth_method in [
            AuthMethod::WPA2Personal,
            AuthMethod::WPA,
            AuthMethod::WPA2WPA3Personal,
            AuthMethod::WEP,
        ] {
            if wifi.is_started().map_err(to_net_error)? {
                wifi.stop().await.map_err(to_net_error)?;
            }

            conf.as_client_conf_mut().auth_method = auth_method;
            wifi.set_configuration(&conf).map_err(to_net_error)?;

            if !wifi.is_started().map_err(to_net_error)? {
                wifi.start().await.map_err(to_net_error)?;
            }

            result = wifi
                .connect()
                .await
                .map_err(|_| NetCtlError::OtherConnectionFailure);

            if result.is_ok() {
                break;
            }
        }

        result?;

        // Matter needs an IPv6 address to work
        esp!(unsafe {
            esp_idf_svc::sys::esp_netif_create_ip6_linklocal(wifi.wifi().sta_netif().handle() as _)
        })
        .map_err(to_net_error)?;

        // Wait not just for the wireless interface to come up, but also for the
        // IP addresses to be assigned.
        wifi.ip_wait_while(
            |wifi| {
                self.netif_state.lock(|state| {
                    let mut state = state.borrow_mut();

                    let changed = state.load(wifi.wifi())?;

                    if changed {
                        self.netif_state_changed.notify();
                    }

                    Ok(!state.is_operational())
                })
            },
            Some(Duration::from_secs(15)),
        )
        .await
        .map_err(|_| NetCtlError::IpBindFailed)?;

        Ok(())
    }
}

impl NetChangeNotif for EspMatterWifiCtl<'_> {
    async fn wait_changed(&self) {
        let _ = self.load(self.wifi.lock().await.wifi());

        let _ = EspMatterNetif::<EspNetif>::wait_any_conf_change(&self.sysloop).await;

        let _ = self.load(self.wifi.lock().await.wifi());
    }
}

impl WirelessDiag for EspMatterWifiCtl<'_> {
    fn connected(&self) -> Result<bool, Error> {
        Ok(self
            .netif_state
            .lock(|state| state.borrow().is_operational()))
    }
}

// TODO
impl WifiDiag for EspMatterWifiCtl<'_> {
    fn bssid(&self, f: &mut dyn FnMut(Option<&[u8]>) -> Result<(), Error>) -> Result<(), Error> {
        f(None)
    }

    fn security_type(&self) -> Result<Nullable<SecurityTypeEnum>, Error> {
        Ok(Nullable::none())
    }

    fn wi_fi_version(&self) -> Result<Nullable<WiFiVersionEnum>, Error> {
        Ok(Nullable::none())
    }

    fn channel_number(&self) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::none())
    }

    fn rssi(&self) -> Result<Nullable<i8>, Error> {
        Ok(Nullable::none())
    }
}

/// This type provides the ESP-IDF implementation of the Matter `NetifDiag` and `NetChangeNotif`
pub struct EspMatterWifiNotif<'a, 'd>(&'a EspMatterWifiCtl<'d>);

impl<'a, 'd> EspMatterWifiNotif<'a, 'd> {
    /// Create a new instance of the `EspMatterWifiNotif` type.
    pub const fn new(wifi: &'a EspMatterWifiCtl<'d>) -> Self {
        Self(wifi)
    }
}

impl NetifDiag for EspMatterWifiNotif<'_, '_> {
    fn netifs(&self, f: &mut dyn FnMut(&NetifInfo) -> Result<(), Error>) -> Result<(), Error> {
        self.0.netif_state.lock(|info| info.borrow().as_ref(f))
    }
}

impl NetChangeNotif for EspMatterWifiNotif<'_, '_> {
    async fn wait_changed(&self) {
        self.0.netif_state_changed.wait().await;
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct LoadOutcome {
    pub operational: bool,
    pub changed: bool,
}

#[derive(Debug)]
struct NetifInfoOwned {
    name: heapless::String<6>,
    operational: bool,
    hw_addr: [u8; 8],
    ipv4_addr: Ipv4Addr,
    ipv6_addr: Ipv6Addr,
    netif_type: InterfaceTypeEnum,
    netif_index: u32,
}

impl NetifInfoOwned {
    const fn new() -> Self {
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

    fn is_operational(&self) -> bool {
        self.operational && !self.ipv4_addr.is_unspecified() && !self.ipv6_addr.is_unspecified()
    }

    fn load(&mut self, wifi: &EspWifi<'_>) -> Result<bool, EspError> {
        EspMatterNetif::<EspNetif>::get_netif_conf(wifi.sta_netif(), |info| {
            Ok(self.load_from_info(wifi.is_connected().unwrap_or(false), info))
        })
    }

    fn load_from_info(&mut self, connected: bool, info: &NetifInfo<'_>) -> bool {
        let hw_addr: &[u8] = info.hw_addr;

        let changed = self.name != info.name
            || self.operational != info.operational && connected
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
            self.operational = info.operational && connected;
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

    fn as_ref<F>(&self, f: F) -> Result<(), Error>
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

fn to_net_constr_error<E>(_err: E) -> NetCtlError {
    NetCtlError::Other(ErrorCode::ConstraintError.into())
}
