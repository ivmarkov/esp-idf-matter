use core::borrow::Borrow;
use core::cell::RefCell;
use core::fmt::Write as _;
use core::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use core::pin::pin;

#[cfg(feature = "async-io-mini")]
use async_io_mini as async_io;

use embassy_futures::select::select3;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::task::block_on;
use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsPartitionId};

use log::info;

use rs_matter::acl::AclMgr;
use rs_matter::data_model::cluster_basic_information::{self, BasicInfoCluster, BasicInfoConfig};
use rs_matter::data_model::core::IMBuffer;
use rs_matter::data_model::objects::{AsyncHandler, AsyncMetadata, EmptyHandler, HandlerCompat};
use rs_matter::data_model::sdm::admin_commissioning::AdminCommCluster;
use rs_matter::data_model::sdm::dev_att::DevAttDataFetcher;
use rs_matter::data_model::sdm::failsafe::FailSafe;
use rs_matter::data_model::sdm::general_commissioning::GenCommCluster;
use rs_matter::data_model::sdm::general_diagnostics::GenDiagCluster;
use rs_matter::data_model::sdm::group_key_management::GrpKeyMgmtCluster;
use rs_matter::data_model::sdm::noc::NocCluster;
use rs_matter::data_model::sdm::{
    admin_commissioning, general_commissioning, general_diagnostics, group_key_management, noc,
    nw_commissioning,
};
use rs_matter::data_model::subscriptions::Subscriptions;
use rs_matter::data_model::system_model::access_control::AccessControlCluster;
use rs_matter::data_model::system_model::descriptor::DescriptorCluster;
use rs_matter::data_model::system_model::{access_control, descriptor};
use rs_matter::error::ErrorCode;
use rs_matter::fabric::FabricMgr;
use rs_matter::mdns::Mdns;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::respond::DefaultResponder;
use rs_matter::secure_channel::pake::PaseMgr;
use rs_matter::transport::network::{NetworkReceive, NetworkSend};
use rs_matter::utils::buf::{BufferAccess, PooledBuffers};
use rs_matter::utils::epoch::Epoch;
use rs_matter::utils::rand::Rand;
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::signal::Signal;
use rs_matter::{handler_chain_type, CommissioningData, Matter, MATTER_PORT};

use crate::error::Error;
use crate::multicast::{join_multicast_v4, join_multicast_v6};
use crate::netif::{get_info, NetifAccess, NetifInfo};
use crate::nvs;
use crate::udp;

pub use eth::*;
#[cfg(all(
    not(esp32h2),
    not(esp32s2),
    esp_idf_comp_esp_wifi_enabled,
    esp_idf_comp_esp_event_enabled,
    not(esp_idf_btdm_ctrl_mode_br_edr_only),
    esp_idf_bt_enabled,
    esp_idf_bt_bluedroid_enabled
))]
pub use wifible::*;

const MAX_SUBSCRIPTIONS: usize = 3;
const MAX_IM_BUFFERS: usize = 10;
const PSM_BUFFER_SIZE: usize = 4096;
const MAX_RESPONDERS: usize = 4;
const MAX_BUSY_RESPONDERS: usize = 2;

mod eth;
mod wifible;

/// A trait modeling a specific network type.
/// `MatterStack` is parameterized by a network type implementing this trait.
pub trait Network {
    const INIT: Self;
}

/// An enum modeling the mDNS service to be used.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum MdnsType {
    /// The mDNS service provided by the `rs-matter` crate.
    #[default]
    Builtin,
}

impl MdnsType {
    pub const fn default() -> Self {
        Self::Builtin
    }
}

/// The `MatterStack` struct is the main entry point for the Matter stack.
///
/// It wraps the actual `rs-matter` Matter instance and provides a simplified API for running the stack.
pub struct MatterStack<'a, N>
where
    N: Network,
{
    matter: Matter<'a>,
    buffers: PooledBuffers<MAX_IM_BUFFERS, NoopRawMutex, IMBuffer>,
    psm_buffer: PooledBuffers<1, NoopRawMutex, heapless::Vec<u8, PSM_BUFFER_SIZE>>,
    subscriptions: Subscriptions<MAX_SUBSCRIPTIONS>,
    #[allow(unused)]
    network: N,
    #[allow(unused)]
    mdns: MdnsType,
    netif_info: Signal<NoopRawMutex, Option<NetifInfo>>,
}

impl<'a, N> MatterStack<'a, N>
where
    N: Network,
{
    /// Create a new `MatterStack` instance.
    pub const fn new(
        dev_det: &'a BasicInfoConfig,
        dev_att: &'a dyn DevAttDataFetcher,
        mdns: MdnsType,
    ) -> Self {
        Self {
            matter: Matter::new_default(
                dev_det,
                dev_att,
                rs_matter::mdns::MdnsService::Builtin,
                MATTER_PORT,
            ),
            buffers: PooledBuffers::new(0),
            psm_buffer: PooledBuffers::new(0),
            subscriptions: Subscriptions::new(),
            network: N::INIT,
            mdns,
            netif_info: Signal::new(None),
        }
    }

    /// Get a reference to the `Matter` instance.
    pub const fn matter(&self) -> &Matter<'a> {
        &self.matter
    }

    /// Get a reference to the `Network` instance.
    /// Useful when the user instantiates `MatterStack` with a custom network type.
    pub const fn network(&self) -> &N {
        &self.network
    }

    /// Notifies the Matter instance that there is a change in the state
    /// of one of the clusters.
    ///
    /// User is expected to call this method when user-provided clusters
    /// change their state.
    ///
    /// This is necessary so as the Matter instance can notify clients
    /// that have active subscriptions to some of the changed clusters.
    pub fn notify_changed(&self) {
        self.subscriptions.notify_changed();
    }

    /// User code hook to get the state of the netif passed to the
    /// `run_with_netif` method.
    ///
    /// Useful when user code needs to bring up/down its own IP services depending on
    /// when the netif controlled by Matter goes up, down or changes its IP configuration.
    pub async fn get_netif_info(&self) -> Option<NetifInfo> {
        self.netif_info
            .wait(|netif_info| Some(netif_info.clone()))
            .await
    }

    /// User code hook to detect changes to the IP state of the netif passed to the
    /// `run_with_netif` method.
    ///
    /// Useful when user code needs to bring up/down its own IP services depending on
    /// when the netif controlled by Matter goes up, down or changes its IP configuration.
    pub async fn wait_netif_changed(
        &self,
        prev_netif_info: Option<&NetifInfo>,
    ) -> Option<NetifInfo> {
        self.netif_info
            .wait(|netif_info| (netif_info.as_ref() != prev_netif_info).then(|| netif_info.clone()))
            .await
    }

    /// This method is a specialization of `run_with_transport` over the UDP transport (both IPv4 and IPv6).
    /// It calls `run_with_transport` and in parallel runs the mDNS service.
    ///
    /// The netif instance is necessary, so that the loop can monitor the network and bring up/down
    /// the main UDP transport and the mDNS service when the netif goes up/down or changes its IP addresses.
    pub async fn run_with_netif<'d, H, P, E>(
        &self,
        sysloop: EspSystemEventLoop,
        nvs: EspNvsPartition<P>,
        netif: E,
        dev_comm: Option<(CommissioningData, DiscoveryCapabilities)>,
        handler: H,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: NvsPartitionId,
        E: NetifAccess,
    {
        loop {
            info!("Waiting for the network to come up...");

            let reset_netif_info = || {
                self.netif_info.modify(|global_netif_info| {
                    if global_netif_info.is_some() {
                        *global_netif_info = None;
                        (true, ())
                    } else {
                        (false, ())
                    }
                });
            };

            let _guard = scopeguard::guard((), |_| reset_netif_info());

            reset_netif_info();

            let netif_info = netif
                .wait(sysloop.clone(), |netif| Ok(get_info(netif).ok()))
                .await?;

            info!("Got IP network: {netif_info}");

            self.netif_info.modify(|global_netif_info| {
                *global_netif_info = Some(netif_info.clone());

                (true, ())
            });

            let socket = async_io::Async::<std::net::UdpSocket>::bind(SocketAddr::V6(
                SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, MATTER_PORT, 0, netif_info.interface),
            ))?;

            let mut main = pin!(self.run_with_transport(
                udp::Udp(&socket),
                udp::Udp(&socket),
                nvs.clone(),
                dev_comm.clone(),
                &handler
            ));
            let mut mdns = pin!(self.run_builtin_mdns(&netif_info));
            let mut down = pin!(netif.wait(sysloop.clone(), |netif| {
                let next = get_info(netif).ok();
                let next = next.as_ref();

                Ok((Some(&netif_info) != next).then_some(()))
            }));

            select3(&mut main, &mut mdns, &mut down).coalesce().await?;

            info!("Network change detected");
        }
    }

    /// A transport-agnostic method to run the main Matter loop.
    /// The user is expected to provide a transport implementation in the form of
    /// `NetworkSend` and `NetworkReceive` implementations.
    ///
    /// The utility runs the following tasks:
    /// - The main Matter transport via the user-provided traits
    /// - The PSM task handling changes to fabrics and ACLs as well as initial load of these from NVS
    /// - The Matter responder (i.e. handling incoming exchanges)
    ///
    /// Unlike `run_with_netif`, this utility method does _not_ run the mDNS service, as the
    /// user-provided transport might not be IP-based (i.e. BLE).
    ///
    /// It also has no facilities for monitoring the transport network state.
    pub async fn run_with_transport<'d, S, R, H, P>(
        &self,
        send: S,
        recv: R,
        nvs: EspNvsPartition<P>,
        dev_comm: Option<(CommissioningData, DiscoveryCapabilities)>,
        handler: H,
    ) -> Result<(), Error>
    where
        S: NetworkSend,
        R: NetworkReceive,
        H: AsyncHandler + AsyncMetadata,
        P: NvsPartitionId,
    {
        // Reset the Matter transport buffers and all sessions first
        self.matter().reset_transport()?;

        let mut psm = pin!(self.run_psm(nvs, nvs::Network::<0, NoopRawMutex>::Eth));
        let mut respond = pin!(self.run_responder(handler));
        let mut transport = pin!(self.run_transport(send, recv, dev_comm));

        select3(&mut psm, &mut respond, &mut transport)
            .coalesce()
            .await?;

        Ok(())
    }

    async fn run_psm<P, const W: usize, R>(
        &self,
        nvs: EspNvsPartition<P>,
        network: nvs::Network<'_, W, R>,
    ) -> Result<(), Error>
    where
        P: NvsPartitionId,
        R: RawMutex,
    {
        if false {
            let mut psm_buf = self
                .psm_buffer
                .get()
                .await
                .ok_or(ErrorCode::ResourceExhausted)?;
            psm_buf.resize_default(4096).unwrap();

            let nvs = EspNvs::new(nvs, "rs_matter", true)?;

            let mut psm = nvs::Psm::new(self.matter(), network, nvs, &mut psm_buf)?;

            psm.run().await
        } else {
            core::future::pending().await
        }
    }

    async fn run_responder<H>(&self, handler: H) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
    {
        let responder =
            DefaultResponder::new(self.matter(), &self.buffers, &self.subscriptions, handler);

        info!(
            "Responder memory: Responder={}B, Runner={}B",
            core::mem::size_of_val(&responder),
            core::mem::size_of_val(&responder.run::<MAX_RESPONDERS, MAX_BUSY_RESPONDERS>())
        );

        // Run the responder with up to MAX_RESPONDERS handlers (i.e. MAX_RESPONDERS exchanges can be handled simultenously)
        // Clients trying to open more exchanges than the ones currently running will get "I'm busy, please try again later"
        responder
            .run::<MAX_RESPONDERS, MAX_BUSY_RESPONDERS>()
            .await?;

        Ok(())
    }

    async fn run_builtin_mdns(&self, netif_info: &NetifInfo) -> Result<(), Error> {
        use rs_matter::mdns::{
            Host, MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_PORT,
        };

        let socket = async_io::Async::<std::net::UdpSocket>::bind(SocketAddr::V6(
            SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, MDNS_PORT, 0, netif_info.interface),
        ))?;

        join_multicast_v4(&socket, MDNS_IPV4_BROADCAST_ADDR, netif_info.ipv4)?;
        join_multicast_v6(&socket, MDNS_IPV6_BROADCAST_ADDR, netif_info.interface)?;

        let mut hostname = heapless::String::<12>::new();
        write!(
            hostname,
            "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            netif_info.mac[0],
            netif_info.mac[1],
            netif_info.mac[2],
            netif_info.mac[3],
            netif_info.mac[4],
            netif_info.mac[5]
        )
        .unwrap();

        self.matter()
            .run_builtin_mdns(
                udp::Udp(&socket),
                udp::Udp(&socket),
                &Host {
                    id: 0,
                    hostname: &hostname,
                    ip: netif_info.ipv4.octets(),
                    ipv6: Some(netif_info.ipv6.octets()),
                },
                Some(netif_info.interface),
            )
            .await?;

        Ok(())
    }

    async fn run_transport<S, R>(
        &self,
        send: S,
        recv: R,
        dev_comm: Option<(CommissioningData, DiscoveryCapabilities)>,
    ) -> Result<(), Error>
    where
        S: NetworkSend,
        R: NetworkReceive,
    {
        self.matter().run(send, recv, dev_comm).await?;

        Ok(())
    }
}

/// A type representing the type of the root (Endpoint 0) handler
/// which is generic over the operational transport clusters (i.e. Ethernet, Wifi or Thread)
pub type RootEndpointHandler<'a, NWCOMM, NWDIAG> = handler_chain_type!(
    NWCOMM,
    NWDIAG,
    HandlerCompat<descriptor::DescriptorCluster<'a>>,
    HandlerCompat<cluster_basic_information::BasicInfoCluster<'a>>,
    HandlerCompat<general_commissioning::GenCommCluster<'a>>,
    HandlerCompat<admin_commissioning::AdminCommCluster<'a>>,
    HandlerCompat<noc::NocCluster<'a>>,
    HandlerCompat<access_control::AccessControlCluster<'a>>,
    HandlerCompat<general_diagnostics::GenDiagCluster>,
    HandlerCompat<group_key_management::GrpKeyMgmtCluster>
);

/// A utility function to instantiate the root (Endpoint 0) handler.
/// Besides a reference to the main `Matter` object, this function
/// needs user-supplied implementations of the network commissioning
/// and network diagnostics clusters.
pub fn handler<'a, NWCOMM, NWDIAG, T>(
    matter: &'a T,
    nwcomm: NWCOMM,
    nwdiag_id: u32,
    nwdiag: NWDIAG,
) -> RootEndpointHandler<'a, NWCOMM, NWDIAG>
where
    T: Borrow<BasicInfoConfig<'a>>
        + Borrow<dyn DevAttDataFetcher + 'a>
        + Borrow<RefCell<PaseMgr>>
        + Borrow<RefCell<FabricMgr>>
        + Borrow<RefCell<AclMgr>>
        + Borrow<RefCell<FailSafe>>
        + Borrow<dyn Mdns + 'a>
        + Borrow<Epoch>
        + Borrow<Rand>
        + 'a,
{
    wrap(
        0,
        matter.borrow(),
        matter.borrow(),
        matter.borrow(),
        matter.borrow(),
        matter.borrow(),
        matter.borrow(),
        matter.borrow(),
        *matter.borrow(),
        *matter.borrow(),
        nwcomm,
        nwdiag_id,
        nwdiag,
    )
}

#[allow(clippy::too_many_arguments)]
fn wrap<'a, NWCOMM, NWDIAG>(
    endpoint_id: u16,
    basic_info: &'a BasicInfoConfig<'a>,
    dev_att: &'a dyn DevAttDataFetcher,
    pase: &'a RefCell<PaseMgr>,
    fabric: &'a RefCell<FabricMgr>,
    acl: &'a RefCell<AclMgr>,
    failsafe: &'a RefCell<FailSafe>,
    mdns: &'a dyn Mdns,
    epoch: Epoch,
    rand: Rand,
    nwcomm: NWCOMM,
    nwdiag_id: u32,
    nwdiag: NWDIAG,
) -> RootEndpointHandler<'a, NWCOMM, NWDIAG> {
    EmptyHandler
        .chain(
            endpoint_id,
            group_key_management::ID,
            HandlerCompat(GrpKeyMgmtCluster::new(rand)),
        )
        .chain(
            endpoint_id,
            general_diagnostics::ID,
            HandlerCompat(GenDiagCluster::new(rand)),
        )
        .chain(
            endpoint_id,
            access_control::ID,
            HandlerCompat(AccessControlCluster::new(acl, rand)),
        )
        .chain(
            endpoint_id,
            noc::ID,
            HandlerCompat(NocCluster::new(
                dev_att, fabric, acl, failsafe, mdns, epoch, rand,
            )),
        )
        .chain(
            endpoint_id,
            admin_commissioning::ID,
            HandlerCompat(AdminCommCluster::new(pase, mdns, rand)),
        )
        .chain(
            endpoint_id,
            general_commissioning::ID,
            HandlerCompat(GenCommCluster::new(failsafe, false, rand)),
        )
        .chain(
            endpoint_id,
            cluster_basic_information::ID,
            HandlerCompat(BasicInfoCluster::new(basic_info, rand)),
        )
        .chain(
            endpoint_id,
            descriptor::ID,
            HandlerCompat(DescriptorCluster::new(rand)),
        )
        .chain(endpoint_id, nwdiag_id, nwdiag)
        .chain(endpoint_id, nw_commissioning::ID, nwcomm)
}

/// A utility function to initialize the `async-io` Reactor which is
/// used for IP-based networks (UDP and TCP).
///
/// User is expected to call this method early in the application's lifecycle
/// when there is plenty of task stack space available, as the initialization
/// consumes > 10KB of stack space, so it has to be done with care.
#[inline(never)]
#[cold]
pub fn init_async_io() -> Result<(), Error> {
    // We'll use `async-io` for networking, so ESP IDF VFS needs to be initialized
    esp_idf_svc::io::vfs::initialize_eventfd(3)?;

    block_on(init_async_io_async());

    Ok(())
}

#[inline(never)]
#[cold]
async fn init_async_io_async() {
    #[cfg(not(feature = "async-io-mini"))]
    {
        // Force the `async-io` lazy initialization to trigger earlier rather than later,
        // as it consumes a lot of temp stack memory
        async_io::Timer::after(core::time::Duration::from_millis(100)).await;
        info!("Async IO initialized; using `async-io`");
    }

    #[cfg(feature = "async-io-mini")]
    {
        // Nothing to initialize for `async-io-mini`
        info!("Async IO initialized; using `async-io-mini`");
    }
}
