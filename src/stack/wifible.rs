#![cfg(all(
    not(esp32h2),
    not(esp32s2),
    esp_idf_comp_esp_wifi_enabled,
    esp_idf_comp_esp_event_enabled,
    not(esp_idf_btdm_ctrl_mode_br_edr_only),
    esp_idf_bt_enabled,
    esp_idf_bt_bluedroid_enabled
))]

use core::borrow::Borrow;
use core::cell::RefCell;
use core::pin::pin;

use alloc::boxed::Box;

use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;

use esp_idf_svc::bt::{Ble, BleEnabled, BtDriver};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{AsyncWifi, EspWifi};

use log::info;

use rs_matter::data_model::cluster_basic_information;
use rs_matter::data_model::objects::{
    AsyncHandler, AsyncMetadata, Cluster, Endpoint, HandlerCompat,
};
use rs_matter::data_model::sdm::failsafe::FailSafe;
use rs_matter::data_model::sdm::{
    admin_commissioning, general_commissioning, general_diagnostics, group_key_management, noc,
    nw_commissioning,
};
use rs_matter::data_model::system_model::{access_control, descriptor};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::btp::{Btp, BtpContext};
use rs_matter::utils::select::Coalesce;
use rs_matter::CommissioningData;

use crate::ble::{BtpGattContext, BtpGattPeripheral};
use crate::error::Error;
use crate::wifi::mgmt::WifiManager;
use crate::wifi::{comm, diag, WifiContext};
use crate::{handler, MatterStack, Network, RootEndpointHandler};

const MAX_WIFI_NETWORKS: usize = 2;
const GATTS_APP_ID: u16 = 0;

pub struct WifiBle {
    btp_context: BtpContext<EspRawMutex>,
    btp_gatt_context: BtpGattContext,
    wifi_context: WifiContext<MAX_WIFI_NETWORKS, NoopRawMutex>,
}

impl WifiBle {
    const fn new() -> Self {
        Self {
            btp_context: BtpContext::new(),
            btp_gatt_context: BtpGattContext::new(),
            wifi_context: WifiContext::new(),
        }
    }
}

impl Network for WifiBle {
    const INIT: Self = Self::new();
}

impl<'a> MatterStack<'a, WifiBle> {
    pub const fn root_metadata() -> Endpoint<'static> {
        Endpoint {
            id: 0,
            device_type: rs_matter::data_model::device_types::DEV_TYPE_ROOT_NODE,
            clusters: &CLUSTERS,
        }
    }

    pub fn root_handler(&self) -> WifiBleRootEndpointHandler<'_> {
        handler(
            0,
            self.matter(),
            comm::WifiNwCommCluster::new(*self.matter().borrow(), &self.network.wifi_context),
            diag::ID,
            HandlerCompat(diag::WifiNwDiagCluster::new(*self.matter().borrow())),
        )
    }

    pub async fn is_commissioned(&self, _nvs: EspDefaultNvsPartition) -> Result<bool, Error> {
        // TODO
        Ok(false)
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
        info!("Running Matter in operating mode (Wifi)");

        let wifi =
            Mutex::<NoopRawMutex, _>::new(AsyncWifi::wrap(wifi, sysloop.clone(), timer_service)?);

        let mgr = WifiManager::new(&wifi, &self.network.wifi_context, sysloop.clone());

        let mut main = pin!(self.run_with_netif(sysloop, nvs, &wifi, None, handler));
        let mut wifi = pin!(mgr.run());

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
        info!("Running Matter in commissioning mode (BLE)");

        let peripheral = BtpGattPeripheral::new(GATTS_APP_ID, bt, &self.network.btp_gatt_context);

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
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        loop {
            if !self.is_commissioned(nvs.clone()).await? {
                let bt = BtDriver::<Ble>::new(&mut modem, Some(nvs.clone()))?;

                info!("BLE driver initialized");

                let mut main = pin!(self.commission(nvs.clone(), &bt, dev_comm.clone(), &handler));
                let mut wait_network_connect =
                    pin!(self.network.wifi_context.wait_network_connect());

                select(&mut main, &mut wait_network_connect)
                    .coalesce()
                    .await?;
            }

            // Reset the matter transport to free up sessions and exchanges
            // and to clear any PASE sessions and their keys (as per Matter Core spec)
            self.matter().reset();

            // As per spec, we need to indicate the expectation of a re-arm with a CASE session
            // even if the current session is a PASE one (this is specific for non-concurrent commissiioning flows)
            let failsafe: &RefCell<FailSafe> = self.matter().borrow();
            failsafe.borrow_mut().expect_case_rearm()?;

            let mut wifi = Box::new(EspWifi::new(
                &mut modem,
                sysloop.clone(),
                Some(nvs.clone()),
            )?);

            info!("Wifi driver initialized");

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

pub type WifiBleRootEndpointHandler<'a> = RootEndpointHandler<
    'a,
    comm::WifiNwCommCluster<'a, MAX_WIFI_NETWORKS, NoopRawMutex>,
    HandlerCompat<diag::WifiNwDiagCluster>,
>;

const CLUSTERS: [Cluster<'static>; 10] = [
    descriptor::CLUSTER,
    cluster_basic_information::CLUSTER,
    general_commissioning::CLUSTER,
    nw_commissioning::WIFI_CLUSTER,
    admin_commissioning::CLUSTER,
    noc::CLUSTER,
    access_control::CLUSTER,
    general_diagnostics::CLUSTER,
    diag::CLUSTER,
    group_key_management::CLUSTER,
];
