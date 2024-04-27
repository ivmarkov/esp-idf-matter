use core::cell::RefCell;

use embassy_sync::blocking_mutex::{self, raw::RawMutex};
use embassy_sync::mutex::Mutex;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::sys::{EspError, ESP_ERR_INVALID_STATE, ESP_FAIL};
use esp_idf_svc::wifi::{self as wifi, AccessPointInfo, AsyncWifi, AuthMethod, EspWifi};

use log::{error, info};

use rs_matter::data_model::objects::{
    AsyncHandler, AttrDataEncoder, AttrDataWriter, AttrDetails, AttrType, CmdDataEncoder,
    CmdDetails, Dataver,
};
use rs_matter::data_model::sdm::nw_commissioning::{
    Attributes, Commands, NetworkCommissioningStatus, NwInfo, ResponseCommands, WIFI_CLUSTER,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::interaction_model::core::IMStatusCode;
use rs_matter::interaction_model::messages::ib::Status;
use rs_matter::tlv::{
    self, FromTLV, OctetStr, TLVArray, TLVElement, TLVList, TLVWriter, TagType, ToTLV,
};
use rs_matter::transport::exchange::Exchange;
use rs_matter::utils::notification::Notification;
use rs_matter::utils::{rand::Rand, writebuf::WriteBuf};

use strum::FromRepr;

pub struct Wifi<'d, M>(Mutex<M, Option<AsyncWifi<EspWifi<'d>>>>)
where
    M: RawMutex;

impl<'d, M> Wifi<'d, M>
where
    M: RawMutex,
{
    pub const fn new() -> Self {
        Self(Mutex::new(None))
    }

    pub(crate) async fn init(&self, wifi: AsyncWifi<EspWifi<'d>>) {
        *self.0.lock().await = Some(wifi);
    }

    fn mutex(&self) -> &Mutex<M, Option<AsyncWifi<EspWifi<'d>>>> {
        &self.0
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, FromTLV, ToTLV, FromRepr)]
pub enum WiFiSecurity {
    Unencrypted = 0x01,
    Wep = 0x02,
    WpaPersonal = 0x04,
    Wpa2Personal = 0x08,
    Wpa3Personal = 0x10,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, FromTLV, ToTLV, FromRepr)]
pub enum WifiBand {
    B3G4 = 0x01,
    B3G65 = 0x02,
    B5G = 0x04,
    B6G = 0x08,
    B60G = 0x10,
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct WiFiInterfaceScanResult<'a> {
    pub security: WiFiSecurity,
    pub ssid: OctetStr<'a>,
    pub bssid: OctetStr<'a>,
    pub channel: u16,
    pub band: Option<WifiBand>,
    pub rssi: Option<i8>,
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct ThreadInterfaceScanResult<'a> {
    pub pan_id: u16,
    pub extended_pan_id: u64,
    pub network_name: OctetStr<'a>,
    pub channel: u16,
    pub version: u8,
    pub extended_address: OctetStr<'a>,
    pub rssi: i8,
    pub lqi: u8,
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct WifiNetwork<'a> {
    ssid: OctetStr<'a>,
    bssid: OctetStr<'a>,
    channel: u16,
    security: WiFiSecurity,
    band: WifiBand,
    rssi: u8,
}
#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct ScanNetworksRequest<'a> {
    pub ssid: Option<OctetStr<'a>>,
    pub breadcrumb: Option<u64>,
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct ScanNetworksResponse<'a> {
    pub status: NetworkCommissioningStatus,
    pub debug_text: Option<OctetStr<'a>>,
    pub wifi_scan_results: Option<TLVArray<'a, WiFiInterfaceScanResult<'a>>>,
    pub thread_scan_results: Option<TLVArray<'a, WiFiInterfaceScanResult<'a>>>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ScanNetworksResponseTag {
    Status = 0,
    DebugText = 1,
    WifiScanResults = 2,
    ThreadScanResults = 3,
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct AddWifiNetworkRequest<'a> {
    pub ssid: OctetStr<'a>,
    pub credentials: OctetStr<'a>,
    pub breadcrumb: Option<u64>,
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct AddThreadNetworkRequest<'a> {
    pub op_dataset: OctetStr<'a>,
    pub breadcrumb: Option<u64>,
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct RemoveNetworkRequest<'a> {
    pub network_id: OctetStr<'a>,
    pub breadcrumb: Option<u64>,
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct NetworkConfigResponse<'a> {
    pub status: NetworkCommissioningStatus,
    pub debug_text: Option<OctetStr<'a>>,
    pub network_index: Option<u8>,
}

pub type ConnectNetworkRequest<'a> = RemoveNetworkRequest<'a>;

#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct ReorderNetworkRequest<'a> {
    pub network_id: OctetStr<'a>,
    pub index: u8,
    pub breadcrumb: Option<u64>,
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
pub struct ConnectNetworkResponse<'a> {
    pub status: NetworkCommissioningStatus,
    pub debug_text: Option<OctetStr<'a>>,
    pub error_value: i32,
}

struct WifiStatus {
    ssid: heapless::String<32>,
    status: NetworkCommissioningStatus,
    value: i32,
}

pub struct WifiState0<const N: usize> {
    pub(crate) networks: heapless::Vec<WifiCredentials, N>,
    pub(crate) connected_once: bool,
    pub(crate) connect_requested: Option<heapless::String<32>>,
    pub(crate) status: Option<WifiStatus>,
    pub(crate) changed: bool,
}

impl<const N: usize> WifiState0<N> {
    fn load(&mut self, data: &[u8]) -> Result<(), Error> {
        let root = TLVList::new(data).iter().next().ok_or(ErrorCode::Invalid)?;

        tlv::from_tlv(&mut self.networks, &root)?;

        self.changed = false;

        Ok(())
    }

    fn store<'m>(&mut self, buf: &'m mut [u8]) -> Result<Option<&'m [u8]>, Error> {
        if !self.changed {
            return Ok(None);
        }

        let mut wb = WriteBuf::new(buf);
        let mut tw = TLVWriter::new(&mut wb);

        self.networks
            .as_slice()
            .to_tlv(&mut tw, TagType::Anonymous)?;

        self.changed = false;

        let len = tw.get_tail();

        Ok(Some(&buf[..len]))
    }
}

pub struct WifiManager<const N: usize, M>(pub(crate) WifiNetworks<N, M>)
where
    M: RawMutex;

impl<const N: usize, M> WifiManager<N, M>
where
    M: RawMutex,
{
    pub const fn new() -> Self {
        Self(WifiNetworks::new())
    }

    pub async fn run<'d, R>(
        &self,
        wifi: &Mutex<R, AsyncWifi<&mut EspWifi<'d>>>,
    ) -> Result<(), crate::error::Error>
    where
        R: RawMutex,
    {
        todo!()
    }

    pub async fn wait_network_connect(&self) -> Result<(), crate::error::Error> {
        todo!()
    }

    // pub async fn connect(&self) -> Result<(), EspError> {
    //     let response = self
    //         .do_connect(None)
    //         .await
    //         .ok_or(EspError::from_infallible::<ESP_ERR_INVALID_STATE>())?;

    //     if matches!(response.status, NetworkCommissioningStatus::Success) {
    //         Ok(())
    //     } else {
    //         Err(EspError::from_infallible::<ESP_FAIL>()) // TODO
    //     }
    // }

    // async fn do_scan(
    //     &self,
    // ) -> Option<Result<alloc::vec::Vec<AccessPointInfo>, NetworkCommissioningStatus>> {
    //     // TODO: Use IfMutex instead of Mutex
    //     let mut wifi_mutex = self.wifi.mutex().lock().await;
    //     let wifi = wifi_mutex.as_mut()?;

    //     let result = Self::wifi_scan(wifi).await;

    //     let status = if result.is_ok() {
    //         NetworkCommissioningStatus::Success
    //     } else {
    //         NetworkCommissioningStatus::OtherConnectionFailure
    //     };

    //     self.state.lock(|state| {
    //         let mut state = state.borrow_mut();

    //         if let Some(last_network_outcome) = state.last_network_outcome.as_mut() {
    //             last_network_outcome.status = status;
    //         } else {
    //             state.last_network_outcome = Some(WifiStatus {
    //                 status,
    //                 ssid: "".try_into().unwrap(),
    //                 value: 0,
    //             });
    //         }
    //     });

    //     Some(result.map_err(|_| status))
    // }

    // async fn do_connect(&self, ssid: Option<&str>) -> Option<ConnectNetworkResponse<'static>> {
    //     let creds = self.state.lock(|state| {
    //         let state = state.borrow();

    //         let creds = if let Some(ssid) = ssid {
    //             state
    //                 .networks
    //                 .iter()
    //                 .find(|creds| creds.ssid.as_str().as_bytes() == ssid.as_bytes())
    //         } else {
    //             state.networks.first()
    //         };

    //         creds.cloned()
    //     });

    //     let ssid = ssid
    //         .map(|ssid| ssid.try_into().unwrap())
    //         .or_else(|| creds.as_ref().map(|creds| creds.ssid.clone()));

    //     let response = if let Some(creds) = creds {
    //         // Found

    //         // TODO: Use IfMutex instead of Mutex
    //         let mut wifi_mutex = self.wifi.mutex().lock().await;
    //         let wifi = wifi_mutex.as_mut()?;

    //         let result = Self::wifi_connect(wifi, creds).await;

    //         ConnectNetworkResponse {
    //             status: if result.is_ok() {
    //                 NetworkCommissioningStatus::Success
    //             } else {
    //                 NetworkCommissioningStatus::OtherConnectionFailure
    //             },
    //             debug_text: None,
    //             error_value: 0,
    //         }
    //     } else {
    //         // Not found
    //         ConnectNetworkResponse {
    //             status: NetworkCommissioningStatus::NetworkIdNotFound,
    //             debug_text: None,
    //             error_value: 1, // TODO
    //         }
    //     };

    //     self.state.lock(|state| {
    //         let mut state = state.borrow_mut();

    //         state.last_network_outcome = Some(WifiStatus {
    //             status: response.status,
    //             ssid: ssid.unwrap(),
    //             value: response.error_value,
    //         });
    //     });

    //     Some(response)
    // }

    // async fn wifi_scan(
    //     wifi: &mut AsyncWifi<EspWifi<'d>>,
    // ) -> Result<alloc::vec::Vec<AccessPointInfo>, EspError> {
    //     let _ = wifi.stop().await;

    //     wifi.set_configuration(&wifi::Configuration::Client(
    //         wifi::ClientConfiguration::default(),
    //     ))?;
    //     wifi.start().await?;

    //     wifi.scan().await
    // }

    // async fn wifi_connect(
    //     wifi: &mut AsyncWifi<EspWifi<'d>>,
    //     creds: WifiCredentials,
    // ) -> Result<(), EspError> {
    //     let auth_methods: &[AuthMethod] = if creds.password.is_empty() {
    //         &[AuthMethod::None]
    //     } else {
    //         &[
    //             AuthMethod::WPA2WPA3Personal,
    //             AuthMethod::WPAWPA2Personal,
    //             AuthMethod::WEP,
    //         ]
    //     };

    //     let mut result = Ok(());

    //     for auth_method in auth_methods.iter().copied() {
    //         let connect = !matches!(auth_method, wifi::AuthMethod::None);
    //         let conf = wifi::Configuration::Client(wifi::ClientConfiguration {
    //             ssid: creds.ssid.clone(),
    //             auth_method,
    //             password: creds.password.clone(),
    //             ..Default::default()
    //         });

    //         result = Self::wifi_connect_with(wifi, &conf, connect).await;

    //         if result.is_ok() {
    //             break;
    //         }
    //     }

    //     result
    // }

    // async fn wifi_connect_with(
    //     wifi: &mut AsyncWifi<EspWifi<'d>>,
    //     conf: &wifi::Configuration,
    //     connect: bool,
    // ) -> Result<(), EspError> {
    //     let _ = wifi.stop().await;

    //     wifi.set_configuration(conf)?;
    //     wifi.start().await?;

    //     if connect {
    //         wifi.connect().await?;
    //     }

    //     Ok(())
    // }
}

pub struct WifiNetworks<const N: usize, M>
where
    M: RawMutex,
{
    state: blocking_mutex::Mutex<M, RefCell<WifiState0<N>>>,
    network_connect_requested: Notification<M>,
}

impl<const N: usize, M> WifiNetworks<N, M>
where
    M: RawMutex,
{
    pub const fn new() -> Self {
        Self {
            state: blocking_mutex::Mutex::new(RefCell::new(WifiState0 {
                networks: heapless::Vec::new(),
                connected_once: false,
                connect_requested: None,
                status: None,
                changed: false,
            })),
            network_connect_requested: Notification::new(),
        }
    }

    pub fn load(&self, data: &[u8]) -> Result<(), Error> {
        self.state.lock(|state| state.borrow_mut().load(data))
    }

    pub fn store<'m>(&self, buf: &'m mut [u8]) -> Result<Option<&'m [u8]>, Error> {
        self.state.lock(|state| state.borrow_mut().store(buf))
    }
}

#[derive(Debug, Clone, ToTLV, FromTLV)]
pub struct WifiCredentials {
    ssid: heapless::String<32>,
    password: heapless::String<64>,
}

struct WifiState<const N: usize> {
    networks: heapless::Vec<WifiCredentials, N>,
    last_network_outcome: Option<WifiStatus>,
    changed: bool,
}

pub struct WifiCommCluster<'a, const N: usize, M>
where
    M: RawMutex,
{
    data_ver: Dataver,
    networks: &'a WifiNetworks<N, M>,
}

impl<'a, const N: usize, M> WifiCommCluster<'a, N, M>
where
    M: RawMutex,
{
    pub fn new(rand: Rand, networks: &'a WifiNetworks<N, M>) -> Self {
        Self {
            data_ver: Dataver::new(rand),
            networks,
        }
    }

    async fn read(
        &self,
        attr: &AttrDetails<'_>,
        encoder: AttrDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        if let Some(mut writer) = encoder.with_dataver(self.data_ver.get())? {
            if attr.is_system() {
                WIFI_CLUSTER.read(attr.attr_id, writer)
            } else {
                match attr.attr_id.try_into()? {
                    Attributes::MaxNetworks => AttrType::<u8>::new().encode(writer, N as u8),
                    Attributes::Networks => {
                        writer.start_array(AttrDataWriter::TAG)?;

                        self.networks.state.lock(|state| {
                            let state = state.borrow();

                            for network in &state.networks {
                                let nw_info = NwInfo {
                                    network_id: OctetStr(network.ssid.as_str().as_bytes()),
                                    connected: state
                                        .status
                                        .as_ref()
                                        .map(|status| {
                                            *status.ssid == network.ssid
                                                && matches!(
                                                    status.status,
                                                    NetworkCommissioningStatus::Success
                                                )
                                        })
                                        .unwrap_or(false),
                                };

                                nw_info.to_tlv(&mut writer, TagType::Anonymous)?;
                            }

                            Ok::<_, Error>(())
                        })?;

                        writer.end_container()?;
                        writer.complete()
                    }
                    Attributes::ScanMaxTimeSecs => AttrType::new().encode(writer, 30_u8),
                    Attributes::ConnectMaxTimeSecs => AttrType::new().encode(writer, 60_u8),
                    Attributes::InterfaceEnabled => AttrType::new().encode(writer, true),
                    Attributes::LastNetworkingStatus => self.networks.state.lock(|state| {
                        AttrType::new().encode(
                            writer,
                            state.borrow().status.as_ref().map(|o| o.status as u8),
                        )
                    }),
                    Attributes::LastNetworkID => self.networks.state.lock(|state| {
                        AttrType::new().encode(
                            writer,
                            state
                                .borrow()
                                .status
                                .as_ref()
                                .map(|o| OctetStr(o.ssid.as_str().as_bytes())),
                        )
                    }),
                    Attributes::LastConnectErrorValue => self.networks.state.lock(|state| {
                        AttrType::new()
                            .encode(writer, state.borrow().status.as_ref().map(|o| o.value))
                    }),
                }
            }
        } else {
            Ok(())
        }
    }

    async fn invoke(
        &self,
        exchange: &Exchange<'_>,
        cmd: &CmdDetails<'_>,
        data: &TLVElement<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        match cmd.cmd_id.try_into()? {
            Commands::ScanNetworks => {
                info!("ScanNetworks");
                self.scan_networks(exchange, &ScanNetworksRequest::from_tlv(data)?, encoder)
                    .await?;
            }
            Commands::AddOrUpdateWifiNetwork => {
                info!("AddOrUpdateWifiNetwork");
                self.add_network(exchange, &AddWifiNetworkRequest::from_tlv(data)?, encoder)
                    .await?;
            }
            Commands::RemoveNetwork => {
                info!("RemoveNetwork");
                self.remove_network(exchange, &RemoveNetworkRequest::from_tlv(data)?, encoder)
                    .await?;
            }
            Commands::ConnectNetwork => {
                info!("ConnectNetwork");
                self.connect_network(exchange, &ConnectNetworkRequest::from_tlv(data)?, encoder)
                    .await?;
            }
            Commands::ReorderNetwork => {
                info!("ReorderNetwork");
                self.reorder_network(exchange, &ReorderNetworkRequest::from_tlv(data)?, encoder)
                    .await?;
            }
            other => {
                error!("{other:?} (not supported)");
                todo!()
            }
        }

        self.data_ver.changed();

        Ok(())
    }

    async fn scan_networks(
        &self,
        _exchange: &Exchange<'_>,
        _req: &ScanNetworksRequest<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        let mut tw = encoder.with_command(ResponseCommands::ScanNetworksResponse as _)?;

        Status::new(IMStatusCode::Busy, 0).to_tlv(&mut tw, TagType::Anonymous)?;

        Ok(())
    }

    async fn add_network(
        &self,
        exchange: &Exchange<'_>,
        req: &AddWifiNetworkRequest<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        // TODO: Check failsafe status

        self.networks.state.lock(|state| {
            let mut state = state.borrow_mut();

            let index = state
                .networks
                .iter()
                .position(|conf| conf.ssid.as_str().as_bytes() == req.ssid.0);

            let mut tw = encoder.with_command(ResponseCommands::NetworkConfigResponse as _)?;

            if let Some(index) = index {
                // Update
                state.networks[index].ssid = core::str::from_utf8(req.ssid.0)
                    .unwrap()
                    .try_into()
                    .unwrap();
                state.networks[index].password = core::str::from_utf8(req.credentials.0)
                    .unwrap()
                    .try_into()
                    .unwrap();

                state.changed = true;
                exchange.matter().notify_changed();

                NetworkConfigResponse {
                    status: NetworkCommissioningStatus::Success,
                    debug_text: None,
                    network_index: Some(index as _),
                }
                .to_tlv(&mut tw, TagType::Anonymous)?;
            } else {
                // Add
                let network = WifiCredentials {
                    // TODO
                    ssid: core::str::from_utf8(req.ssid.0)
                        .unwrap()
                        .try_into()
                        .unwrap(),
                    password: core::str::from_utf8(req.credentials.0)
                        .unwrap()
                        .try_into()
                        .unwrap(),
                };

                if state.networks.push(network).is_ok() {
                    state.changed = true;
                    exchange.matter().notify_changed();

                    NetworkConfigResponse {
                        status: NetworkCommissioningStatus::Success,
                        debug_text: None,
                        network_index: Some(state.networks.len() as _),
                    }
                    .to_tlv(&mut tw, TagType::Anonymous)?;
                } else {
                    NetworkConfigResponse {
                        status: NetworkCommissioningStatus::BoundsExceeded,
                        debug_text: None,
                        network_index: None,
                    }
                    .to_tlv(&mut tw, TagType::Anonymous)?;
                }
            }

            Ok(())
        })
    }

    async fn remove_network(
        &self,
        exchange: &Exchange<'_>,
        req: &RemoveNetworkRequest<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        // TODO: Check failsafe status

        self.networks.state.lock(|state| {
            let mut state = state.borrow_mut();

            let index = state
                .networks
                .iter()
                .position(|conf| conf.ssid.as_str().as_bytes() == req.network_id.0);

            let mut tw = encoder.with_command(ResponseCommands::NetworkConfigResponse as _)?;

            if let Some(index) = index {
                // Found
                state.networks.remove(index);
                state.changed = true;
                exchange.matter().notify_changed();

                NetworkConfigResponse {
                    status: NetworkCommissioningStatus::Success,
                    debug_text: None,
                    network_index: Some(index as _),
                }
                .to_tlv(&mut tw, TagType::Anonymous)?;
            } else {
                // Not found
                NetworkConfigResponse {
                    status: NetworkCommissioningStatus::NetworkIdNotFound,
                    debug_text: None,
                    network_index: None,
                }
                .to_tlv(&mut tw, TagType::Anonymous)?;
            }

            Ok(())
        })
    }

    async fn connect_network(
        &self,
        _exchange: &Exchange<'_>,
        req: &ConnectNetworkRequest<'_>,
        _encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        // TODO: Check failsafe status

        // Non-concurrent commissioning scenario (i.e. only BLE is active, and the ESP IDF co-exist mode is not enabled)
        // Notify that we have received a connect command

        self.networks.state.lock(|state| {
            let mut state = state.borrow_mut();

            state.connect_requested = Some(
                core::str::from_utf8(req.network_id.0)
                    .unwrap()
                    .try_into()
                    .unwrap(),
            );
        });

        self.networks.network_connect_requested.notify();

        // Block forever waitinng for the firware to restart
        core::future::pending().await
    }

    async fn reorder_network(
        &self,
        exchange: &Exchange<'_>,
        req: &ReorderNetworkRequest<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        // TODO: Check failsafe status

        self.networks.state.lock(|state| {
            let mut state = state.borrow_mut();

            let index = state
                .networks
                .iter()
                .position(|conf| conf.ssid.as_str().as_bytes() == req.network_id.0);

            let mut tw = encoder.with_command(ResponseCommands::NetworkConfigResponse as _)?;

            if let Some(index) = index {
                // Found

                if req.index < state.networks.len() as u8 {
                    let conf = state.networks.remove(index);
                    state
                        .networks
                        .insert(req.index as usize, conf)
                        .map_err(|_| ())
                        .unwrap();

                    state.changed = true;
                    exchange.matter().notify_changed();

                    NetworkConfigResponse {
                        status: NetworkCommissioningStatus::Success,
                        debug_text: None,
                        network_index: Some(req.index as _),
                    }
                    .to_tlv(&mut tw, TagType::Anonymous)?;
                } else {
                    NetworkConfigResponse {
                        status: NetworkCommissioningStatus::OutOfRange,
                        debug_text: None,
                        network_index: Some(req.index as _),
                    }
                    .to_tlv(&mut tw, TagType::Anonymous)?;
                }
            } else {
                // Not found
                NetworkConfigResponse {
                    status: NetworkCommissioningStatus::NetworkIdNotFound,
                    debug_text: None,
                    network_index: None,
                }
                .to_tlv(&mut tw, TagType::Anonymous)?;
            }

            Ok(())
        })
    }
}

impl<'a, const N: usize, M> AsyncHandler for WifiCommCluster<'a, N, M>
where
    M: RawMutex,
{
    async fn read<'m>(
        &'m self,
        attr: &'m AttrDetails<'_>,
        encoder: AttrDataEncoder<'m, '_, '_>,
    ) -> Result<(), Error> {
        WifiCommCluster::read(self, attr, encoder).await
    }

    async fn invoke<'m>(
        &'m self,
        exchange: &'m Exchange<'_>,
        cmd: &'m CmdDetails<'_>,
        data: &'m TLVElement<'_>,
        encoder: CmdDataEncoder<'m, '_, '_>,
    ) -> Result<(), Error> {
        WifiCommCluster::invoke(self, exchange, cmd, data, encoder).await
    }
}

// impl ChangeNotifier<()> for WifiCommCluster {
//     fn consume_change(&mut self) -> Option<()> {
//         self.data_ver.consume_change(())
//     }
// }
