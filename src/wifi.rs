//! This module provides the ESP-IDF Wifi implementation of the Matter `NetCtl`, `NetChangeNotif`, `WirelessDiag`, and `WifiDiag` traits.

use core::cell::RefCell;
use core::net::{Ipv4Addr, Ipv6Addr};

extern crate alloc;

use alloc::sync::Arc;

use embassy_sync::blocking_mutex::{self, raw::CriticalSectionRawMutex};
use embassy_sync::mutex::Mutex;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::handle::RawHandle as _;
use esp_idf_svc::netif::EspNetif;
use esp_idf_svc::sys::{esp, EspError};
use esp_idf_svc::wifi::{
    AsyncWifi, AuthMethod, ClientConfiguration, Configuration::Client, EspWifi,
};

use rs_matter_stack::matter::data_model::networks::NetChangeNotif;
use rs_matter_stack::matter::data_model::sdm::gen_diag::{InterfaceTypeEnum, NetifDiag, NetifInfo};
use rs_matter_stack::matter::data_model::sdm::net_comm::{
    NetCtl, NetCtlError, NetworkScanInfo, NetworkType, WirelessCreds,
};
use rs_matter_stack::matter::data_model::sdm::net_comm::{WiFiBandEnum, WiFiSecurityBitmap};
use rs_matter_stack::matter::data_model::sdm::wifi_diag::{
    SecurityTypeEnum, WiFiVersionEnum, WifiDiag, WirelessDiag,
};
use rs_matter_stack::matter::error::{Error, ErrorCode};
use rs_matter_stack::matter::tlv::Nullable;

use crate::error::to_net_error;
use crate::netif::EspMatterNetif;

/// This type provides the ESP-IDF Wifi implementation of the Matter `NetCtl`, `NetChangeNotif`, `WirelessDiag`, and `WifiDiag` traits
// TODO: Revert to `EspRawMutex` when `esp-idf-svc` is updated to `embassy-sync 0.7`
#[derive(Clone)]
pub struct EspSharedWifi<'a> {
    wifi: Arc<Mutex<CriticalSectionRawMutex /*EspRawMutex*/, AsyncWifi<EspWifi<'a>>>>,
    state: Arc<blocking_mutex::Mutex<CriticalSectionRawMutex /*EspRawMutex*/, RefCell<State>>>,
    sysloop: EspSystemEventLoop,
}

impl<'a> EspSharedWifi<'a> {
    /// Create a new instance of the `EspSharedWifi` type.
    pub fn new(wifi: AsyncWifi<EspWifi<'a>>, sysloop: EspSystemEventLoop) -> Self {
        Self {
            wifi: Arc::new(Mutex::new(wifi)),
            state: Arc::new(blocking_mutex::Mutex::new(RefCell::new(State::new()))),
            sysloop,
        }
    }

    fn load(&self, wifi: &EspWifi<'_>) -> Result<(), EspError> {
        EspMatterNetif::<EspNetif>::get_netif_conf(wifi.sta_netif(), |info| {
            self.state.lock(|state| {
                let mut state = state.borrow_mut();

                state.netif_info.load(info);
                state.connected = wifi.is_connected().unwrap();
            });

            Ok(())
        })
    }
}

impl NetCtl for EspSharedWifi<'_> {
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

        self.load(wifi.wifi()).map_err(to_net_error)?;

        wifi.wait_netif_up()
            .await
            .map_err(|_| NetCtlError::IpBindFailed)?;

        self.load(wifi.wifi()).map_err(to_net_error)?;

        Ok(())
    }
}

impl NetChangeNotif for EspSharedWifi<'_> {
    async fn wait_changed(&self) {
        let _ = self.load(self.wifi.lock().await.wifi());

        let _ = EspMatterNetif::<EspNetif>::wait_any_conf_change(&self.sysloop).await;

        let _ = self.load(self.wifi.lock().await.wifi());
    }
}

impl WirelessDiag for EspSharedWifi<'_> {
    fn connected(&self) -> Result<bool, Error> {
        Ok(self.state.lock(|state| {
            let state = state.borrow();

            state.connected
                && state.netif_info.operational
                && !state.netif_info.ipv4_addr.is_unspecified()
                && !state.netif_info.ipv6_addr.is_unspecified()
        }))
    }
}

// TODO
impl WifiDiag for EspSharedWifi<'_> {
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

impl NetifDiag for EspSharedWifi<'_> {
    fn netifs(&self, f: &mut dyn FnMut(&NetifInfo) -> Result<(), Error>) -> Result<(), Error> {
        self.state.lock(|info| info.borrow().netif_info.as_ref(f))
    }
}

#[derive(Debug)]
struct State {
    netif_info: NetifInfoOwned,
    connected: bool,
}

impl State {
    const fn new() -> Self {
        Self {
            netif_info: NetifInfoOwned::new(),
            connected: false,
        }
    }
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

    fn load(&mut self, info: &NetifInfo<'_>) {
        let hw_addr: &[u8] = info.hw_addr;

        self.name = info.name.try_into().unwrap();
        self.operational = info.operational;
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
