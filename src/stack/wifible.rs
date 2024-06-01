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

use rs_matter::data_model::objects::{AsyncHandler, AsyncMetadata, Endpoint, HandlerCompat};
use rs_matter::data_model::root_endpoint;
use rs_matter::data_model::root_endpoint::{handler, OperNwType, RootEndpointHandler};
use rs_matter::data_model::sdm::failsafe::FailSafe;
use rs_matter::data_model::sdm::wifi_nw_diagnostics;
use rs_matter::data_model::sdm::wifi_nw_diagnostics::{
    WiFiSecurity, WiFiVersion, WifiNwDiagCluster, WifiNwDiagData,
};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::btp::{Btp, BtpContext};
use rs_matter::utils::select::Coalesce;
use rs_matter::CommissioningData;

use crate::ble::{BtpGattContext, BtpGattPeripheral};
use crate::error::Error;
use crate::wifi::mgmt::WifiManager;
use crate::wifi::{comm, WifiContext};
use crate::{MatterStack, Network};

const MAX_WIFI_NETWORKS: usize = 2;
const GATTS_APP_ID: u16 = 0;

/// An implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over WiFi when operating.
///
/// The supported commissioning is of the non-concurrent type (as per the Matter Core spec),
/// where the device - at any point in time - either runs Bluetooth or Wifi, but not both.
/// This is done to save memory and to avoid the usage of the ESP IDF Co-exist driver.
///
/// The BLE implementation used is the ESP IDF Bluedroid stack (not NimBLE).
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

pub type WifiBleMatterStack<'a> = MatterStack<'a, WifiBle>;

impl<'a> MatterStack<'a, WifiBle> {
    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    pub const fn root_metadata() -> Endpoint<'static> {
        root_endpoint::endpoint(0, OperNwType::Wifi)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    pub fn root_handler(&self) -> WifiBleRootEndpointHandler<'_> {
        handler(
            0,
            self.matter(),
            comm::WifiNwCommCluster::new(*self.matter().borrow(), &self.network.wifi_context),
            wifi_nw_diagnostics::ID,
            HandlerCompat(WifiNwDiagCluster::new(
                *self.matter().borrow(),
                // TODO: Update with actual information
                WifiNwDiagData {
                    bssid: [0; 6],
                    security_type: WiFiSecurity::Unspecified,
                    wifi_version: WiFiVersion::B,
                    channel_number: 20,
                    rssi: 0,
                },
            )),
            false,
        )
    }

    /// Resets the Matter instance to the factory defaults putting it into a
    /// Commissionable mode.
    pub fn reset(&self) -> Result<(), Error> {
        // TODO: Reset fabrics and ACLs
        self.network.btp_gatt_context.reset()?;
        // TODO self.network.btp_context.reset();
        self.network.wifi_context.reset();

        Ok(())
    }

    /// Return information whether the Matter instance is already commissioned.
    ///
    /// User might need to reach out to this method only when it needs finer-grained control
    /// and utilizes the `commission` and `operate` methods rather than the all-in-one `run` loop.
    pub async fn is_commissioned(&self, _nvs: EspDefaultNvsPartition) -> Result<bool, Error> {
        // TODO
        Ok(false)
    }

    /// A utility method to run the Matter stack in Operating mode (as per the Matter Core spec) over Wifi.
    ///
    /// This method assumes that the Matter instance is already commissioned and therefore
    /// does not take a `CommissioningData` parameter.
    ///
    /// It is just a composition of the `MatterStack::run_with_netif` method, and the `WifiManager::run` method,
    /// where the former takes care of running the main Matter loop, while the latter takes care of ensuring
    /// that the Matter instance stays connected to the Wifi network.
    pub async fn operate<'d, H>(
        &self,
        sysloop: EspSystemEventLoop,
        timer_service: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        wifi: &mut EspWifi<'d>,
        handler: H,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
    {
        info!("Running Matter in operating mode (Wifi)");

        let wifi =
            Mutex::<NoopRawMutex, _>::new(AsyncWifi::wrap(wifi, sysloop.clone(), timer_service)?);

        let mgr = WifiManager::new(&wifi, &self.network.wifi_context, sysloop.clone());

        let mut main = pin!(self.run_with_netif(sysloop, nvs, &wifi, None, handler));
        let mut wifi = pin!(mgr.run());

        select(&mut wifi, &mut main).coalesce().await
    }

    /// A utility method to run the Matter stack in Commissioning mode (as per the Matter Core spec) over BLE.
    /// Essentially an instantiation of `MatterStack::run_with_transport` with the BLE transport.
    ///
    /// Note: make sure to call `MatterStack::reset` before calling this method, as all fabrics and ACLs, as well as all
    /// transport state should be reset.
    pub async fn commission<'d, H, B>(
        &'static self,
        nvs: EspDefaultNvsPartition,
        bt: &BtDriver<'d, B>,
        dev_comm: CommissioningData,
        handler: H,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        B: BleEnabled,
    {
        info!("Running Matter in commissioning mode (BLE)");

        let peripheral = BtpGattPeripheral::new(GATTS_APP_ID, bt, &self.network.btp_gatt_context)?;

        let btp = Btp::new(peripheral, &self.network.btp_context);

        let mut ble = pin!(async {
            btp.run("BT", self.matter().dev_det(), &dev_comm)
                .await
                .map_err(Into::into)
        });

        let mut main = pin!(self.run_with_transport(
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

    /// An all-in-one "run everything" method that automatically
    /// places the Matter stack either in Commissioning or in Operating mode, depending
    /// on the state of the device as persisted in the NVS storage.
    pub async fn run<'d, H>(
        &'static self,
        sysloop: EspSystemEventLoop,
        timer_service: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        mut modem: impl Peripheral<P = Modem> + 'd,
        dev_comm: CommissioningData,
        handler: H,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        loop {
            if !self.is_commissioned(nvs.clone()).await? {
                // Reset to factory defaults everything, as we'll do commissioning all over
                self.reset()?;

                let bt = BtDriver::<Ble>::new(&mut modem, Some(nvs.clone()))?;

                info!("BLE driver initialized");

                let mut main = pin!(self.commission(nvs.clone(), &bt, dev_comm.clone(), &handler));
                let mut wait_network_connect =
                    pin!(self.network.wifi_context.wait_network_connect());

                select(&mut main, &mut wait_network_connect)
                    .coalesce()
                    .await?;
            }

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
    HandlerCompat<WifiNwDiagCluster>,
>;
