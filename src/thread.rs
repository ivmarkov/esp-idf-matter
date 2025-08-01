//! This module provides the ESP-IDF Thread implementation of the Matter `NetCtl`, `NetChangeNotif`, `WirelessDiag`, and `ThreadDiag` traits.

use core::cell::RefCell;
use core::fmt::Write;

use alloc::sync::Arc;

use embassy_sync::blocking_mutex::{self, raw::CriticalSectionRawMutex};
use embassy_sync::mutex::Mutex;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::netif::EspNetif;
use esp_idf_svc::sys::{esp, esp_mac_type_t_ESP_MAC_IEEE802154, esp_read_mac, EspError};
use esp_idf_svc::thread::{
    ActiveScanResult, EspThread, NetifMode, Role, SrpConf, SrpService, SrpServiceSlot,
};

use log::{error, info};

use rs_matter_stack::matter::dm::clusters::decl::general_diagnostics::InterfaceTypeEnum;
use rs_matter_stack::matter::dm::clusters::gen_diag::{NetifDiag, NetifInfo};
use rs_matter_stack::matter::dm::clusters::net_comm::{
    NetCtl, NetCtlError, NetworkScanInfo, NetworkType, WirelessCreds,
};
use rs_matter_stack::matter::dm::clusters::thread_diag::{
    NeighborTable, NetworkFaultEnum, OperationalDatasetComponents, RouteTable, RoutingRoleEnum,
    SecurityPolicy, ThreadDiag,
};
use rs_matter_stack::matter::dm::clusters::wifi_diag::WirelessDiag;
use rs_matter_stack::matter::dm::networks::NetChangeNotif;
use rs_matter_stack::matter::error::{Error, ErrorCode};
use rs_matter_stack::matter::transport::network::mdns::Service;
use rs_matter_stack::matter::utils::storage::Vec;
use rs_matter_stack::matter::utils::sync::Notification;
use rs_matter_stack::matter::{Matter, MatterMdnsService};
use rs_matter_stack::mdns::Mdns;

use crate::error::to_net_error;
use crate::netif::{EspMatterNetif, NetifInfoOwned};

extern crate alloc;

/// This type provides the ESP-IDF Thread implementation of the Matter `NetCtl`, `NetChangeNotif`, `WirelessDiag`, and `ThreadDiag` traits
// TODO: Revert to `EspRawMutex` when `esp-idf-svc` is updated to `embassy-sync 0.7`
pub struct EspMatterThreadCtl<'a, 'd, M>
where
    M: NetifMode,
{
    thread: Mutex<CriticalSectionRawMutex /*EspRawMutex*/, &'a EspThread<'d, M>>,
    netif_state: blocking_mutex::Mutex<
        CriticalSectionRawMutex, /*EspRawMutex*/
        RefCell<NetifInfoOwned>,
    >,
    netif_state_changed: Notification<CriticalSectionRawMutex /*EspRawMutex*/>,
    sysloop: EspSystemEventLoop,
}

impl<'a, 'd, M> EspMatterThreadCtl<'a, 'd, M>
where
    M: NetifMode,
{
    /// Create a new instance of the `EspMatterThreadCtl` type.
    pub const fn new(thread: &'a EspThread<'d, M>, sysloop: EspSystemEventLoop) -> Self {
        Self {
            thread: Mutex::new(thread),
            netif_state: blocking_mutex::Mutex::new(RefCell::new(NetifInfoOwned::new())),
            netif_state_changed: Notification::new(),
            sysloop,
        }
    }

    fn load<MM: NetifMode>(&self, thread: &EspThread<'_, MM>) -> Result<(), EspError> {
        self.netif_state.lock(|state| {
            if state.borrow_mut().load(
                Self::is_thread_connected(thread)?,
                thread.netif(),
                InterfaceTypeEnum::Thread,
            )? {
                self.netif_state_changed.notify();
            }

            Ok(())
        })
    }

    fn is_thread_connected<MM: NetifMode>(thread: &EspThread<'_, MM>) -> Result<bool, EspError> {
        Ok(!matches!(thread.role()?, Role::Detached | Role::Disabled))
    }
}

impl<M> NetCtl for EspMatterThreadCtl<'_, '_, M>
where
    M: NetifMode,
{
    fn net_type(&self) -> NetworkType {
        NetworkType::Thread
    }

    async fn scan<F>(&self, network: Option<&[u8]>, mut f: F) -> Result<(), NetCtlError>
    where
        F: FnMut(&NetworkScanInfo) -> Result<(), Error>,
    {
        const POLL_SCAN_WAIT: embassy_time::Duration = embassy_time::Duration::from_millis(500);

        let thread = self.thread.lock().await;

        struct OwnedScanResult {
            pan_id: u16,
            ext_pan_id: u64,
            network_name: heapless::String<16>,
            channel: u16,
            version: u8,
            ext_addr: [u8; 8],
            rssi: i8,
            lqi: u8,
        }

        impl From<ActiveScanResult<'_>> for OwnedScanResult {
            fn from(result: ActiveScanResult<'_>) -> Self {
                Self {
                    pan_id: result.pan_id(),
                    ext_pan_id: u64::from_be_bytes(result.extended_pan_id().try_into().unwrap()),
                    network_name: result
                        .network_name_cstr()
                        .to_str()
                        .unwrap_or("???")
                        .try_into()
                        .unwrap_or("???".try_into().unwrap()),
                    channel: result.channel() as _,
                    version: result.version(),
                    ext_addr: result.extended_address().try_into().unwrap(),
                    rssi: result.max_rssi(),
                    lqi: result.lqi(),
                }
            }
        }

        impl<'a> From<&'a OwnedScanResult> for NetworkScanInfo<'a> {
            fn from(result: &'a OwnedScanResult) -> Self {
                NetworkScanInfo::Thread {
                    pan_id: result.pan_id,
                    ext_pan_id: result.ext_pan_id,
                    network_name: result.network_name.as_str(),
                    channel: result.channel,
                    version: result.version,
                    ext_addr: &result.ext_addr,
                    rssi: result.rssi,
                    lqi: result.lqi,
                }
            }
        }

        let scan_result = Arc::new(blocking_mutex::Mutex::<CriticalSectionRawMutex, _>::new(
            RefCell::new(Some(heapless::Vec::<_, 5>::new())),
        ));

        {
            let scan_result = scan_result.clone();

            thread
                .scan(move |info: Option<ActiveScanResult<'_>>| {
                    if let Some(info) = info {
                        scan_result.lock(|results| {
                            let mut results = results.borrow_mut();

                            if let Some(results) = results.as_mut() {
                                results.push(OwnedScanResult::from(info)).ok();
                            }
                        });
                    }
                })
                .map_err(to_net_error)?;
        }

        loop {
            if !thread.is_scan_in_progress().map_err(to_net_error)? {
                break;
            }

            embassy_time::Timer::after(POLL_SCAN_WAIT).await;
        }

        let results = scan_result
            .lock(|results| results.borrow_mut().take())
            .unwrap();

        for result in results {
            if network
                .map(|network| result.ext_pan_id.to_be_bytes() == network)
                .unwrap_or(true)
            {
                f(&NetworkScanInfo::Thread {
                    pan_id: result.pan_id,
                    ext_pan_id: result.ext_pan_id,
                    network_name: result.network_name.as_str(),
                    channel: result.channel,
                    version: result.version,
                    ext_addr: &result.ext_addr,
                    rssi: result.rssi,
                    lqi: result.lqi,
                })?;
            }
        }

        Ok(())
    }

    async fn connect(&self, creds: &WirelessCreds<'_>) -> Result<(), NetCtlError> {
        const CONNECT_WAIT: embassy_time::Duration = embassy_time::Duration::from_millis(30000);
        const POLL_CONNECT_WAIT: embassy_time::Duration = embassy_time::Duration::from_millis(1000);

        let WirelessCreds::Thread { dataset_tlv } = creds else {
            return Err(NetCtlError::Other(ErrorCode::InvalidData.into()));
        };

        let thread = self.thread.lock().await;

        self.load(&thread).map_err(to_net_error)?;

        thread.set_tod(dataset_tlv).map_err(to_net_constr_error)?;

        let connect_attempt_time = embassy_time::Instant::now();

        loop {
            let operational = self
                .netif_state
                .lock(|state| {
                    let mut state = state.borrow_mut();

                    let changed = state.load(
                        Self::is_thread_connected(*thread)?,
                        thread.netif(),
                        InterfaceTypeEnum::Thread,
                    )?;

                    if changed {
                        self.netif_state_changed.notify();
                    }

                    Result::<_, EspError>::Ok(state.is_operational_v6())
                })
                .map_err(to_net_error)?;

            if operational {
                break;
            }

            if connect_attempt_time.elapsed() > CONNECT_WAIT {
                return Err(NetCtlError::AuthFailure);
            }

            embassy_time::Timer::after(POLL_CONNECT_WAIT).await;
        }

        Ok(())
    }
}

impl<M> NetChangeNotif for EspMatterThreadCtl<'_, '_, M>
where
    M: NetifMode,
{
    async fn wait_changed(&self) {
        let _ = self.load(*self.thread.lock().await);

        let _ = EspMatterNetif::<EspNetif>::wait_any_conf_change(&self.sysloop).await;

        let _ = self.load(*self.thread.lock().await);
    }
}

impl<M> WirelessDiag for EspMatterThreadCtl<'_, '_, M>
where
    M: NetifMode,
{
    fn connected(&self) -> Result<bool, Error> {
        Ok(self
            .netif_state
            .lock(|state| state.borrow().is_operational_v6()))
    }
}

// TODO
impl<M> ThreadDiag for EspMatterThreadCtl<'_, '_, M>
where
    M: NetifMode,
{
    fn channel(&self) -> Result<Option<u16>, Error> {
        Ok(None)
    }

    fn routing_role(&self) -> Result<Option<RoutingRoleEnum>, Error> {
        Ok(None)
    }

    fn network_name(
        &self,
        f: &mut dyn FnMut(Option<&str>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        f(None)
    }

    fn pan_id(&self) -> Result<Option<u16>, Error> {
        Ok(None)
    }

    fn extended_pan_id(&self) -> Result<Option<u64>, Error> {
        Ok(None)
    }

    fn mesh_local_prefix(
        &self,
        f: &mut dyn FnMut(Option<&[u8]>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        f(None)
    }

    fn neightbor_table(
        &self,
        _f: &mut dyn FnMut(&NeighborTable) -> Result<(), Error>,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn route_table(
        &self,
        _f: &mut dyn FnMut(&RouteTable) -> Result<(), Error>,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn partition_id(&self) -> Result<Option<u32>, Error> {
        Ok(None)
    }

    fn weighting(&self) -> Result<Option<u16>, Error> {
        Ok(None)
    }

    fn data_version(&self) -> Result<Option<u16>, Error> {
        Ok(None)
    }

    fn stable_data_version(&self) -> Result<Option<u16>, Error> {
        Ok(None)
    }

    fn leader_router_id(&self) -> Result<Option<u8>, Error> {
        Ok(None)
    }

    fn security_policy(&self) -> Result<Option<SecurityPolicy>, Error> {
        Ok(None)
    }

    fn channel_page0_mask(
        &self,
        f: &mut dyn FnMut(Option<&[u8]>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        f(None)
    }

    fn operational_dataset_components(
        &self,
        f: &mut dyn FnMut(Option<&OperationalDatasetComponents>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        f(None)
    }

    fn active_network_faults_list(
        &self,
        _f: &mut dyn FnMut(NetworkFaultEnum) -> Result<(), Error>,
    ) -> Result<(), Error> {
        Ok(())
    }
}

/// This type provides the ESP-IDF Thread implementation of the Matter `NetifDiag` and `NetChangeNotif`
pub struct EspMatterThreadNotif<'a, 'd, M>(&'a EspMatterThreadCtl<'a, 'd, M>)
where
    M: NetifMode;

impl<'a, 'd, M> EspMatterThreadNotif<'a, 'd, M>
where
    M: NetifMode,
{
    /// Create a new instance of the `EspMatterThreadNotif` type.
    pub const fn new(thread: &'a EspMatterThreadCtl<'a, 'd, M>) -> Self {
        Self(thread)
    }
}

impl<M> NetifDiag for EspMatterThreadNotif<'_, '_, M>
where
    M: NetifMode,
{
    fn netifs(&self, f: &mut dyn FnMut(&NetifInfo) -> Result<(), Error>) -> Result<(), Error> {
        self.0.netif_state.lock(|info| info.borrow().as_ref(f))
    }
}

impl<M> NetChangeNotif for EspMatterThreadNotif<'_, '_, M>
where
    M: NetifMode,
{
    async fn wait_changed(&self) {
        self.0.netif_state_changed.wait().await;
    }
}

const MAX_MATTER_SERVICES: usize = 3;

pub struct EspMatterThreadSrp<'a, 'd, M>
where
    M: NetifMode,
{
    thread: &'a EspThread<'d, M>,
    services: Vec<(MatterMdnsService, SrpServiceSlot), MAX_MATTER_SERVICES>,
}

impl<'a, 'd, M> EspMatterThreadSrp<'a, 'd, M>
where
    M: NetifMode,
{
    /// Create a new instance of the `EspMatterThreadSrp` type.
    pub fn new(thread: &'a EspThread<'d, M>) -> Self {
        Self {
            thread,
            services: Vec::new(),
        }
    }

    pub async fn run(
        &mut self,
        matter: &Matter<'_>,
        _ipv6: core::net::Ipv6Addr,
    ) -> Result<(), Error> {
        let mut ieee_eui64 = [0; 8];
        esp!(unsafe { esp_read_mac(ieee_eui64.as_mut_ptr(), esp_mac_type_t_ESP_MAC_IEEE802154) })
            .map_err(to_net_error)?;

        let mut hostname = heapless::String::<16>::new();
        write!(
            hostname,
            "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            ieee_eui64[0],
            ieee_eui64[1],
            ieee_eui64[2],
            ieee_eui64[3],
            ieee_eui64[4],
            ieee_eui64[5],
            ieee_eui64[6],
            ieee_eui64[7]
        )
        .unwrap();

        self.thread
            .srp_set_conf(&SrpConf {
                host_name: &hostname,
                host_addrs: &[],
                ..Default::default()
            })
            .map_err(to_net_error)?;

        loop {
            matter.wait_mdns().await;

            let mut services = Vec::<_, MAX_MATTER_SERVICES>::new();
            matter.mdns_services(|service| {
                if services.push(service).is_err() {
                    error!("Too many mDNS services registered, max is {MAX_MATTER_SERVICES}");

                    Err(ErrorCode::ConstraintError)?;
                }

                Ok(())
            })?;

            info!("mDNS services changed, updating...");

            self.update_services(matter, &services)?;

            info!("mDNS services updated");
        }
    }

    fn update_services(
        &mut self,
        matter: &Matter,
        services: &[MatterMdnsService],
    ) -> Result<(), Error> {
        for service in services {
            if !self.services.iter().any(|(s, _)| s == service) {
                info!("Registering mDNS service: {service:?}");
                let slot = self.register(matter, service)?;
                if self.services.push((service.clone(), slot)).is_err() {
                    error!("Too many mDNS services registered, max is {MAX_MATTER_SERVICES}");

                    Err(ErrorCode::ConstraintError)?;
                }
            }
        }

        loop {
            let removed = self
                .services
                .iter()
                .find(|(service, _)| !services.contains(service))
                .map(|(service, slot)| (service.clone(), *slot));

            if let Some((service, slot)) = removed {
                info!("Deregistering mDNS service: {service:?}");
                self.deregister(slot)?;
                self.services.retain(|(_, s)| *s != slot);
            } else {
                break;
            }
        }

        Ok(())
    }

    fn register(
        &mut self,
        matter: &Matter,
        service: &MatterMdnsService,
    ) -> Result<SrpServiceSlot, Error> {
        Service::call_with(service, matter.dev_det(), matter.port(), |service| {
            let slot = self
                .thread
                .srp_add_service(&SrpService {
                    name: service.service_protocol,
                    instance_name: service.name,
                    port: service.port,
                    subtype_labels: service.service_subtypes.iter().cloned(),
                    txt_entries: service
                        .txt_kvs
                        .iter()
                        .cloned()
                        .filter(|(k, _)| !k.is_empty())
                        .map(|(k, v)| (k, v.as_bytes())),
                    priority: 0,
                    weight: 0,
                    lease_secs: 0,
                    key_lease_secs: 0,
                })
                .map_err(to_net_error)?;

            Ok(slot)
        })
    }

    fn deregister(&mut self, slot: SrpServiceSlot) -> Result<(), Error> {
        self.thread
            .srp_remove_service(slot, false)
            .map_err(to_net_error)?;

        Ok(())
    }
}

impl<M> Mdns for EspMatterThreadSrp<'_, '_, M>
where
    M: NetifMode,
{
    async fn run<U>(
        &mut self,
        matter: &Matter<'_>,
        _udp: U,
        _mac: &[u8],
        _ipv4: core::net::Ipv4Addr,
        ipv6: core::net::Ipv6Addr,
        _interface: u32,
    ) -> Result<(), Error>
    where
        U: edge_nal::UdpBind,
    {
        Self::run(self, matter, ipv6).await
    }
}

fn to_net_constr_error<E>(_err: E) -> NetCtlError {
    NetCtlError::Other(ErrorCode::ConstraintError.into())
}
