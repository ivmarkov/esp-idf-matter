use std::io;

use alloc::sync::Arc;

use edge_nal::UdpBind;
use edge_nal_std::{Stack, UdpSocket};

use embassy_sync::mutex::Mutex;

use embedded_svc::wifi::asynch::Wifi as WifiSvc;

use enumset::EnumSet;

use esp_idf_svc::bt::{self, BtDriver};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::into_ref;
use esp_idf_svc::hal::modem::{BluetoothModemPeripheral, WifiModemPeripheral};
use esp_idf_svc::hal::peripheral::{Peripheral, PeripheralRef};
use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::EspError;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{AccessPointInfo, AsyncWifi, Capability, Configuration, EspWifi};

use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{FromTLV, ToTLV};
use rs_matter::utils::init::{init, Init};

use rs_matter_stack::netif::{Netif, NetifConf, NetifRun};
use rs_matter_stack::network::{Embedding, Network};
use rs_matter_stack::persist::KvBlobBuf;
use rs_matter_stack::wireless::svc::SvcWifiController;
use rs_matter_stack::wireless::traits::{
    Ble, Thread, Wifi, WifiData, Wireless, WirelessConfig, WirelessData, NC,
};
use rs_matter_stack::{MatterStack, WirelessBle};

use crate::ble::{EspBtpGattContext, EspBtpGattPeripheral};

use super::netif::EspMatterNetif;

/// A type alias for an ESP-IDF Matter stack running over Wifi (and BLE, during commissioning).
pub type EspWifiMatterStack<'a, E> = EspWirelessMatterStack<'a, Wifi, E>;

/// A type alias for an ESP-IDF Matter stack running over Thread (and BLE, during commissioning).
pub type EspThreadMatterStack<'a, E> = EspWirelessMatterStack<'a, Thread, E>;

/// A type alias for an ESP-IDF Matter stack running over Wifi (and BLE, during commissioning).
///
/// Unlike `EspWifiMatterStack`, this type alias runs the commissioning in a non-concurrent mode,
/// where the device runs either BLE or Wifi, but not both at the same time.
///
/// This is useful to save memory by only having one of the stacks active at any point in time.
///
/// Note that Alexa does not (yet) work with non-concurrent commissioning.
pub type EspWifiNCMatterStack<'a, E> = EspWirelessMatterStack<'a, Wifi<NC>, E>;

/// A type alias for an ESP-IDF Matter stack running over Thread (and BLE, during commissioning).
///
/// Unlike `EspThreadMatterStack`, this type alias runs the commissioning in a non-concurrent mode,
/// where the device runs either BLE or Thread, but not both at the same time.
///
/// This is useful to save memory by only having one of the stacks active at any point in time.
///
/// Note that Alexa does not (yet) work with non-concurrent commissioning.
pub type EspThreadNCMatterStack<'a, E> = EspWirelessMatterStack<'a, Thread<NC>, E>;

/// A type alias for an ESP-IDF Matter stack running over a wireless network (Wifi or Thread) and BLE.
pub type EspWirelessMatterStack<'a, T, E> = MatterStack<'a, EspWirelessBle<T, E>>;

/// A type alias for an ESP-IDF implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over either WiFi or Thread when operating.
pub type EspWirelessBle<T, E> = WirelessBle<EspRawMutex, T, KvBlobBuf<EspGatt<E>>>;

/// An embedding of the ESP IDF Bluedroid Gatt peripheral context for the `WirelessBle` network type from `rs-matter-stack`.
/// Allows the memory of this context to be statically allocated and cost-initialized.
///
/// Usage:
/// ```no_run
/// MatterStack<WirelessBle<EspRawMutex, Wifi, KvBlobBuf<EspGatt<E>>>>::new(...);
/// ```
///
/// ... where `E` can be a next-level, user-supplied embedding or just `()` if the user does not need to embed anything.
pub struct EspGatt<E = ()> {
    btp_gatt_context: EspBtpGattContext,
    embedding: E,
}

impl<E> EspGatt<E>
where
    E: Embedding,
{
    /// Creates a new instance of the `EspGatt` embedding.
    #[allow(clippy::large_stack_frames)]
    #[inline(always)]
    const fn new() -> Self {
        Self {
            btp_gatt_context: EspBtpGattContext::new(),
            embedding: E::INIT,
        }
    }

    /// Return an in-place initializer for the `EspGatt` embedding.
    fn init() -> impl Init<Self> {
        init!(Self {
            btp_gatt_context <- EspBtpGattContext::init(),
            embedding <- E::init(),
        })
    }

    /// Return a reference to the Bluedroid Gatt peripheral context.
    pub fn context(&self) -> &EspBtpGattContext {
        &self.btp_gatt_context
    }

    /// Return a reference to the embedding.
    pub fn embedding(&self) -> &E {
        &self.embedding
    }
}

impl<E> Embedding for EspGatt<E>
where
    E: Embedding,
{
    const INIT: Self = Self::new();

    fn init() -> impl Init<Self> {
        EspGatt::init()
    }
}

const GATTS_APP_ID: u16 = 0;

/// A `Ble` trait implementation via ESP-IDF
pub struct EspMatterBle<'a, 'd, T> {
    context: &'a EspBtpGattContext,
    modem: PeripheralRef<'d, T>,
    nvs: EspDefaultNvsPartition,
}

impl<'a, 'd, T> EspMatterBle<'a, 'd, T>
where
    T: BluetoothModemPeripheral,
{
    /// Create a new instance of the `EspBle` type.
    pub fn new<C, E>(
        modem: impl Peripheral<P = T> + 'd,
        nvs: EspDefaultNvsPartition,
        stack: &'a EspWirelessMatterStack<C, E>,
    ) -> Self
    where
        C: WirelessConfig,
        <C::Data as WirelessData>::NetworkCredentials: Clone + for<'t> FromTLV<'t> + ToTLV,
        E: Embedding + 'static,
    {
        Self::wrap(
            modem,
            nvs,
            stack.network().embedding().embedding().context(),
        )
    }

    /// Wrap an existing `EspBtpGattContext` and `BluetoothModemPeripheral` into a new instance of the `EspBle` type.
    pub fn wrap(
        modem: impl Peripheral<P = T> + 'd,
        nvs: EspDefaultNvsPartition,
        context: &'a EspBtpGattContext,
    ) -> Self {
        into_ref!(modem);

        Self {
            context,
            modem,
            nvs,
        }
    }
}

impl<'a, 'd, T> Ble for EspMatterBle<'a, 'd, T>
where
    T: BluetoothModemPeripheral,
{
    type Peripheral<'t>
        = EspBtpGattPeripheral<'a, 't, bt::Ble>
    where
        Self: 't;

    async fn start(&mut self) -> Result<Self::Peripheral<'_>, Error> {
        let bt = BtDriver::new(&mut self.modem, Some(self.nvs.clone())).unwrap();

        let peripheral = EspBtpGattPeripheral::new(GATTS_APP_ID, bt, self.context).unwrap();

        Ok(peripheral)
    }
}

/// The relation between a network interface and a controller is slightly different
/// in the ESP-IDF crates compared to what `rs-matter-stack` wants, hence we need this helper type.
pub struct EspWifiSplit<'a>(
    Arc<Mutex<EspRawMutex, AsyncWifi<EspWifi<'a>>>>,
    EspSystemEventLoop,
);

impl<'a> WifiSvc for EspWifiSplit<'a> {
    type Error = EspError;

    async fn get_capabilities(&self) -> Result<EnumSet<Capability>, Self::Error> {
        let wifi = self.0.lock().await;

        wifi.get_capabilities()
    }

    async fn get_configuration(&self) -> Result<Configuration, Self::Error> {
        let wifi = self.0.lock().await;

        wifi.get_configuration()
    }

    async fn set_configuration(&mut self, conf: &Configuration) -> Result<(), Self::Error> {
        let mut wifi = self.0.lock().await;

        wifi.set_configuration(conf)
    }

    async fn start(&mut self) -> Result<(), Self::Error> {
        let mut wifi = self.0.lock().await;

        wifi.start().await
    }

    async fn stop(&mut self) -> Result<(), Self::Error> {
        let mut wifi = self.0.lock().await;

        wifi.stop().await
    }

    async fn connect(&mut self) -> Result<(), Self::Error> {
        let mut wifi = self.0.lock().await;

        wifi.connect().await
    }

    async fn disconnect(&mut self) -> Result<(), Self::Error> {
        let mut wifi = self.0.lock().await;

        wifi.disconnect().await
    }

    async fn is_started(&self) -> Result<bool, Self::Error> {
        let wifi = self.0.lock().await;

        wifi.is_started()
    }

    async fn is_connected(&self) -> Result<bool, Self::Error> {
        let wifi = self.0.lock().await;

        wifi.is_connected()
    }

    async fn scan_n<const N: usize>(
        &mut self,
    ) -> Result<(heapless::Vec<AccessPointInfo, N>, usize), Self::Error> {
        let mut wifi = self.0.lock().await;

        wifi.scan_n().await
    }

    async fn scan(&mut self) -> Result<alloc::vec::Vec<AccessPointInfo>, Self::Error> {
        let mut wifi = self.0.lock().await;

        wifi.scan().await
    }
}

impl<'a> Netif for EspWifiSplit<'a> {
    async fn get_conf(&self) -> Result<Option<NetifConf>, Error> {
        let wifi = self.0.lock().await;

        EspMatterNetif::new(wifi.wifi().sta_netif(), self.1.clone())
            .get_conf()
            .await
    }

    async fn wait_conf_change(&self) -> Result<(), Error> {
        let wifi = self.0.lock().await;

        EspMatterNetif::new(wifi.wifi().sta_netif(), self.1.clone())
            .wait_conf_change()
            .await
    }
}

impl<'a> NetifRun for EspWifiSplit<'a> {
    async fn run(&self) -> Result<(), Error> {
        core::future::pending().await
    }
}

impl<'a> UdpBind for EspWifiSplit<'a> {
    type Error = io::Error;
    type Socket<'b>
        = UdpSocket
    where
        Self: 'b;

    async fn bind(&self, local: core::net::SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
        Stack::new().bind(local).await
    }
}

/// A `Wireless` trait implementation via ESP-IDF's Wifi modem
pub struct EspMatterWifi<'d, T> {
    modem: PeripheralRef<'d, T>,
    sysloop: EspSystemEventLoop,
    timer: EspTaskTimerService,
    nvs: EspDefaultNvsPartition,
}

impl<'d, T> EspMatterWifi<'d, T>
where
    T: WifiModemPeripheral,
{
    /// Create a new instance of the `EspMatterWifi` type.
    pub fn new(
        modem: impl Peripheral<P = T> + 'd,
        sysloop: EspSystemEventLoop,
        timer: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
    ) -> Self {
        into_ref!(modem);

        Self {
            modem,
            sysloop,
            timer,
            nvs,
        }
    }
}

impl<'d, T> Wireless for EspMatterWifi<'d, T>
where
    T: WifiModemPeripheral,
{
    type Data = WifiData;

    type Netif<'a>
        = EspWifiSplit<'a>
    where
        Self: 'a;

    type Controller<'a>
        = SvcWifiController<EspWifiSplit<'a>>
    where
        Self: 'a;

    async fn start(&mut self) -> Result<(Self::Netif<'_>, Self::Controller<'_>), Error> {
        let wifi = Arc::new(Mutex::new(
            AsyncWifi::wrap(
                EspWifi::new(
                    &mut self.modem,
                    self.sysloop.clone(),
                    Some(self.nvs.clone()),
                )
                .map_err(|_| ErrorCode::NoNetworkInterface)?, // TODO
                self.sysloop.clone(),
                self.timer.clone(),
            )
            .map_err(|_| ErrorCode::NoNetworkInterface)?, // TODO
        ));

        Ok((
            EspWifiSplit(wifi.clone(), self.sysloop.clone()),
            SvcWifiController::new(EspWifiSplit(wifi.clone(), self.sysloop.clone())),
        ))
    }
}
