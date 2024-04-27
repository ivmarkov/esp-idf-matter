use core::net::{Ipv4Addr, Ipv6Addr};
use core::pin::pin;

use std::io;
use std::net::UdpSocket;

use async_io::Async;
use ble::BtpGattContext;
use embassy_futures::select::{select, select3, select4};
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};

use embassy_sync::mutex::Mutex;
use esp_idf_svc::bt::{Ble, BleEnabled, BtDriver};
use esp_idf_svc::eth::{AsyncEth, EspEth};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::{
    BluetoothModem, BluetoothModemPeripheral, Modem, WifiModem, WifiModemPeripheral,
};
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, EspNvsPartition, NvsPartitionId};

use esp_idf_svc::sys::{esp, esp_netif_get_ip6_linklocal, EspError, ESP_FAIL};

use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{AsyncWifi, EspWifi};
use log::info;

use netif::{EthNetifAccess, NetifAccess};
use rs_matter::data_model::cluster_basic_information::BasicInfoConfig;
use rs_matter::data_model::core::IMBuffer;
use rs_matter::data_model::objects::{
    AsyncHandler, AsyncMetadata, Endpoint, HandlerCompat, Metadata,
};
use rs_matter::data_model::root_endpoint::{self, RootEndpointHandler};
use rs_matter::data_model::sdm::dev_att::DevAttDataFetcher;
use rs_matter::data_model::subscriptions::Subscriptions;
use rs_matter::error::ErrorCode;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::respond::DefaultResponder;
use rs_matter::transport::core::MATTER_SOCKET_BIND_ADDR;
use rs_matter::transport::network::btp::{Btp, BtpContext};
use rs_matter::transport::network::{NetworkReceive, NetworkSend};
use rs_matter::utils::buf::{BufferAccess, PooledBuffers};
use rs_matter::utils::select::Coalesce;
use rs_matter::{CommissioningData, Matter, MATTER_PORT};
use wifi::{Wifi, WifiManager};

extern crate alloc;

use crate::ble::BluedroidGattPeripheral;
use crate::error::Error;
use crate::multicast::{join_multicast_v4, join_multicast_v6};

pub mod ble;
pub mod error;
pub mod mdns;
pub mod multicast;
pub mod netif;
pub mod nvs;
pub mod wifi;

pub trait Network {
    const INIT: Self;
}

pub struct Eth(());

impl Network for Eth {
    const INIT: Self = Self(());
}

pub struct WifiBle {
    btp_context: BtpContext<EspRawMutex>,
    btp_gatt_context: BtpGattContext,
    wifi_manager: WifiManager<3, NoopRawMutex>,
}

impl WifiBle {
    const fn new() -> Self {
        Self {
            btp_context: BtpContext::new(),
            btp_gatt_context: BtpGattContext::new(),
            wifi_manager: WifiManager::new(),
        }
    }
}

impl Network for WifiBle {
    const INIT: Self = Self::new();
}

pub struct MatterStack<'a, T>
where
    T: Network,
{
    matter: Matter<'a>,
    buffers: PooledBuffers<10, NoopRawMutex, IMBuffer>,
    psm_buffer: PooledBuffers<1, NoopRawMutex, heapless::Vec<u8, 4096>>,
    subscriptions: Subscriptions<3>,
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
            let (ipv4, ipv6) = netif.wait_ips_up(sysloop.clone()).await?;

            let socket = async_io::Async::<UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR)?;

            let mut main =
                pin!(self.run_once(&socket, &socket, nvs.clone(), dev_comm.clone(), &handler));
            let mut mdns = pin!(self.run_builtin_mdns(ipv4, ipv6));
            let mut down = pin!(netif.wait_ips_down(sysloop.clone()));

            select3(&mut main, &mut mdns, &mut down).coalesce().await?;
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
        let mut psm_buf = self
            .psm_buffer
            .get()
            .await
            .ok_or(ErrorCode::ResourceExhausted)?;
        psm_buf.resize_default(4096).unwrap();

        let nvs = EspNvs::new(nvs, "rs_matter", true)?;

        let mut psm = nvs::Psm::new(self.matter(), network, nvs, &mut psm_buf)?;

        psm.run().await
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
            core::mem::size_of_val(&responder.run::<4, 4>())
        );

        // Run the responder with up to 4 handlers (i.e. 4 exchanges can be handled simultenously)
        // Clients trying to open more exchanges than the ones currently running will get "I'm busy, please try again later"
        responder.run::<4, 4>().await?;

        Ok(())
    }

    async fn run_builtin_mdns(&self, ipv4: Ipv4Addr, ipv6: Ipv6Addr) -> Result<(), Error> {
        use rs_matter::mdns::{
            Host, MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_SOCKET_BIND_ADDR,
        };

        let socket = async_io::Async::<UdpSocket>::bind(MDNS_SOCKET_BIND_ADDR)?;

        join_multicast_v4(&socket, MDNS_IPV4_BROADCAST_ADDR, Ipv4Addr::UNSPECIFIED)?;
        join_multicast_v6(&socket, MDNS_IPV6_BROADCAST_ADDR, 0)?;

        self.matter()
            .run_builtin_mdns(
                &socket,
                &socket,
                Host {
                    id: 0,
                    hostname: self.matter().dev_det().device_name,
                    ip: ipv4.octets(),
                    ipv6: Some(ipv6.octets()),
                },
                Some(0),
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

impl<'a> MatterStack<'a, Eth> {
    pub const fn root_metadata() -> Endpoint<'static> {
        root_endpoint::endpoint(0)
    }

    pub fn root_handler(&self) -> impl AsyncHandler + '_ {
        HandlerCompat(root_endpoint::handler(0, self.matter()))
    }

    pub async fn run<'d, T, P, E>(
        &self,
        sysloop: EspSystemEventLoop,
        nvs: EspNvsPartition<P>,
        eth: E,
        dev_comm: CommissioningData,
        handler: T,
    ) -> Result<(), Error>
    where
        T: AsyncHandler + AsyncMetadata,
        P: NvsPartitionId,
        E: NetifAccess,
    {
        self.run_with_netif(
            sysloop,
            nvs,
            eth,
            Some((dev_comm, DiscoveryCapabilities::new(true, false, false))),
            handler,
        )
        .await
    }
}

impl<'d, M> NetifAccess for &Mutex<M, AsyncWifi<&mut EspWifi<'d>>>
where
    M: RawMutex,
{
    async fn with_netif<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&EspNetif) -> R,
    {
        f(self.lock().await.wifi().sta_netif())
    }
}

impl<'a> MatterStack<'a, WifiBle> {
    pub const fn root_metadata() -> Endpoint<'static> {
        root_endpoint::endpoint(0)
    }

    pub fn root_handler(&self, wifi: &Wifi<'_, NoopRawMutex>) -> RootEndpointHandler<'_> {
        root_endpoint::handler(0, self.matter())
    }

    pub async fn is_commissioned(&self, nvs: EspDefaultNvsPartition) -> Result<bool, Error> {
        todo!()
    }

    pub async fn operate<'d, T>(
        &self,
        sysloop: EspSystemEventLoop,
        timer_service: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        wifi: &mut EspWifi<'d>,
        handler: T,
    ) -> Result<(), Error>
    where
        T: AsyncHandler + AsyncMetadata,
    {
        let wifi =
            Mutex::<NoopRawMutex, _>::new(AsyncWifi::wrap(wifi, sysloop.clone(), timer_service)?);

        let mut main = pin!(self.run_with_netif(sysloop, nvs, &wifi, None, handler));
        let mut wifi = pin!(self.network.wifi_manager.run(&wifi));

        select(&mut wifi, &mut main).coalesce().await
    }

    pub async fn commission<'d, T, M>(
        &'static self,
        nvs: EspDefaultNvsPartition,
        bt: &BtDriver<'d, M>,
        dev_comm: CommissioningData,
        handler: T,
    ) -> Result<(), Error>
    where
        T: AsyncHandler + AsyncMetadata,
        M: BleEnabled,
    {
        let peripheral = BluedroidGattPeripheral::new(&self.network.btp_gatt_context, bt);

        let btp = Btp::new(peripheral, &self.network.btp_context);

        let mut ble = pin!(async {
            btp.run("BT", self.matter().dev_det(), &dev_comm)
                .await
                .map_err(Into::into)
        });
        let mut main = pin!(self.run_once(
            &btp,
            &btp,
            nvs,
            Some((
                dev_comm.clone(),
                DiscoveryCapabilities::new(false, true, false)
            )),
            &handler
        ));

        select(&mut ble, &mut main).coalesce().await
    }

    pub async fn run<'d, T>(
        &'static self,
        sysloop: EspSystemEventLoop,
        timer_service: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        mut modem: impl Peripheral<P = Modem> + 'd,
        dev_comm: CommissioningData,
        handler: T,
    ) -> Result<(), Error>
    where
        T: AsyncHandler + AsyncMetadata,
    {
        loop {
            if !self.is_commissioned(nvs.clone()).await? {
                let bt = BtDriver::<Ble>::new(&mut modem, Some(nvs.clone()))?;

                let mut main = pin!(self.commission(nvs.clone(), &bt, dev_comm.clone(), &handler));
                let mut wait_network_connect =
                    pin!(self.network.wifi_manager.wait_network_connect());

                select(&mut main, &mut wait_network_connect)
                    .coalesce()
                    .await?;
            }

            let mut wifi = EspWifi::new(&mut modem, sysloop.clone(), Some(nvs.clone()))?;

            self.operate(
                sysloop.clone(),
                timer_service.clone(),
                nvs.clone(),
                &mut wifi,
                &handler,
            )
            .await?;
        }
    }
}
