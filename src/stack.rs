use core::borrow::Borrow;
use core::cell::RefCell;
use core::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use core::pin::pin;
use core::time::Duration;

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
use rs_matter::{handler_chain_type, CommissioningData, Matter, MATTER_PORT};

use crate::error::Error;
use crate::multicast::{join_multicast_v4, join_multicast_v6};
use crate::netif::{get_ips, NetifAccess};
use crate::nvs;

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

pub trait Network {
    const INIT: Self;
}

pub struct MatterStack<'a, T>
where
    T: Network,
{
    matter: Matter<'a>,
    buffers: PooledBuffers<MAX_IM_BUFFERS, NoopRawMutex, IMBuffer>,
    psm_buffer: PooledBuffers<1, NoopRawMutex, heapless::Vec<u8, PSM_BUFFER_SIZE>>,
    subscriptions: Subscriptions<MAX_SUBSCRIPTIONS>,
    #[allow(unused)]
    network: T,
}

impl<'a, T> MatterStack<'a, T>
where
    T: Network,
{
    pub const fn new(
        dev_det: &'static BasicInfoConfig,
        dev_att: &'static dyn DevAttDataFetcher,
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
            network: T::INIT,
        }
    }

    pub const fn matter(&self) -> &Matter<'a> {
        &self.matter
    }

    pub fn notify_changed(&self) {
        self.subscriptions.notify_changed();
    }

    pub fn reset(&self) {
        todo!()
    }

    pub async fn run_with_netif<'d, H, P, N>(
        &self,
        sysloop: EspSystemEventLoop,
        nvs: EspNvsPartition<P>,
        netif: N,
        dev_comm: Option<(CommissioningData, DiscoveryCapabilities)>,
        handler: H,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: NvsPartitionId,
        N: NetifAccess,
    {
        loop {
            info!("Waiting for the network to come up...");

            let (ipv4, ipv6, interface) = netif
                .wait(sysloop.clone(), |netif| Ok(get_ips(netif).ok()))
                .await?;

            info!("Got network with IPs: IPv4={ipv4}, IPv6={ipv6}, if={interface}");

            let socket = async_io::Async::<std::net::UdpSocket>::bind(SocketAddr::V6(
                SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, MATTER_PORT, 0, interface),
            ))?;

            let mut main =
                pin!(self.run_once(&socket, &socket, nvs.clone(), dev_comm.clone(), &handler));
            let mut mdns = pin!(self.run_builtin_mdns(ipv4, ipv6, interface));
            let mut down = pin!(netif.wait(sysloop.clone(), |netif| {
                let prev = Some((ipv4, ipv6, interface));
                let next = get_ips(netif).ok();

                Ok((prev != next).then_some(()))
            }));

            select3(&mut main, &mut mdns, &mut down).coalesce().await?;

            info!("Network change detected");
        }
    }

    pub async fn run_once<'d, S, R, H, P>(
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
        let mut psm = pin!(self.run_psm(nvs, nvs::Network::<0, NoopRawMutex>::None));
        let mut respond = pin!(self.run_responder(handler));
        let mut transport = pin!(self.run_transport(send, recv, dev_comm));

        select3(&mut psm, &mut respond, &mut transport)
            .coalesce()
            .await?;

        Ok(())
    }

    async fn run_psm<P, const N: usize, M>(
        &self,
        nvs: EspNvsPartition<P>,
        network: nvs::Network<'_, N, M>,
    ) -> Result<(), Error>
    where
        P: NvsPartitionId,
        M: RawMutex,
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

    async fn run_builtin_mdns(
        &self,
        ipv4: Ipv4Addr,
        ipv6: Ipv6Addr,
        interface: u32,
    ) -> Result<(), Error> {
        use rs_matter::mdns::{
            Host, MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_PORT,
        };

        let socket = async_io::Async::<std::net::UdpSocket>::bind(SocketAddr::V6(
            SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, MDNS_PORT, 0, interface),
        ))?;

        join_multicast_v4(&socket, MDNS_IPV4_BROADCAST_ADDR, ipv4)?;
        join_multicast_v6(&socket, MDNS_IPV6_BROADCAST_ADDR, interface)?;

        self.matter()
            .run_builtin_mdns(
                &socket,
                &socket,
                &Host {
                    id: 0,
                    hostname: self.matter().dev_det().device_name,
                    ip: ipv4.octets(),
                    ipv6: Some(ipv6.octets()),
                },
                Some(interface),
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

pub fn handler<'a, NWCOMM, NWDIAG, T>(
    endpoint_id: u16,
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
        endpoint_id,
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

#[inline(never)]
#[cold]
pub fn init_async_io() -> Result<(), Error> {
    // We'll use `async-io` for networking, so ESP IDF VFS needs to be initialized
    esp_idf_svc::io::vfs::initialize_eventfd(3)?;

    block_on(init_async_io_async());

    info!("Async IO initialized");

    Ok(())
}

#[inline(never)]
#[cold]
async fn init_async_io_async() {
    // Force the `async-io` lazy initialization to trigger earlier rather than later,
    // as it consumes a lot of temp stack memory
    async_io::Timer::after(Duration::from_millis(100)).await;
}
