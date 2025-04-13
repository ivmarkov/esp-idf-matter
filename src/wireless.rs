use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;

use rs_matter_stack::matter::utils::init::{init, Init};
use rs_matter_stack::network::Embedding;
use rs_matter_stack::wireless::WirelessBle;
use rs_matter_stack::MatterStack;

use crate::ble::EspBtpGattContext;

#[cfg(all(
    esp_idf_comp_openthread_enabled,
    esp_idf_openthread_enabled,
    esp_idf_comp_vfs_enabled,
))]
pub use thread::*;

#[cfg(esp_idf_comp_esp_wifi_enabled)]
pub use wifi::*;

/// A type alias for an ESP-IDF Matter stack running over a wireless network (Wifi or Thread) and BLE.
pub type EspWirelessMatterStack<'a, T, E> = MatterStack<'a, EspWirelessBle<T, E>>;

/// A type alias for an ESP-IDF implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over either WiFi or Thread when operating.
pub type EspWirelessBle<T, E> = WirelessBle<EspRawMutex, T, EspGatt<E>>;

/// An embedding of the ESP IDF Bluedroid Gatt peripheral context for the `WirelessBle` network type from `rs-matter-stack`.
///
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

#[cfg(all(
    esp_idf_comp_openthread_enabled,
    esp_idf_openthread_enabled,
    esp_idf_comp_vfs_enabled,
))]
mod thread {
    use esp_idf_svc::io::vfs::MountedEventfs;
    use rs_matter_stack::wireless::{Thread, ThreadData};

    use alloc::sync::Arc;

    use embassy_sync::mutex::Mutex;

    use embedded_svc::wifi::asynch::Wifi as WifiSvc;

    use enumset::EnumSet;

    use esp_idf_svc::bt::{self, BtDriver};
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::hal::into_ref;
    use esp_idf_svc::hal::modem::Modem;
    use esp_idf_svc::hal::peripheral::{Peripheral, PeripheralRef};
    use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;
    use esp_idf_svc::handle::RawHandle;
    use esp_idf_svc::netif::EspNetif;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use esp_idf_svc::sys::{esp, EspError};
    use esp_idf_svc::timer::EspTaskTimerService;
    use esp_idf_svc::wifi::{AccessPointInfo, AsyncWifi, Capability, Configuration, EspWifi};

    use rs_matter::error::Error;

    use rs_matter_stack::netif::{Netif, NetifConf};
    use rs_matter_stack::network::{Embedding, Network};
    use rs_matter_stack::wireless::svc::SvcWifiController;
    use rs_matter_stack::wireless::{
        Gatt, GattTask, Wifi, WifiData, Wireless, WirelessCoex, WirelessCoexTask, WirelessTask,
    };

    use crate::ble::{EspBtpGattContext, EspBtpGattPeripheral};
    use crate::error::to_net_error;
    use crate::netif::EspMatterNetif;
    use esp_idf_svc::thread::EspThread;

    use super::{EspWirelessMatterStack, GATTS_APP_ID};

    /// A type alias for an ESP-IDF Matter stack running over Thread (and BLE, during commissioning).
    pub type EspThreadMatterStack<'a, E> = EspWirelessMatterStack<'a, Thread, E>;

    /// A `Wireless` trait implementation via ESP-IDF's Thread/BT modem
    pub struct EspMatterThread<'a, 'd> {
        modem: PeripheralRef<'d, Modem>,
        sysloop: EspSystemEventLoop,
        timer: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        mounted_event_fs: Arc<MountedEventfs>,
        ble_context: &'a EspBtpGattContext,
    }

    impl<'a, 'd> EspMatterThread<'a, 'd> {
        /// Create a new instance of the `EspMatterThread` type.
        pub fn new<E>(
            modem: impl Peripheral<P = Modem> + 'd,
            sysloop: EspSystemEventLoop,
            timer: EspTaskTimerService,
            nvs: EspDefaultNvsPartition,
            mounted_event_fs: Arc<MountedEventfs>,
            stack: &'a EspThreadMatterStack<E>,
        ) -> Self
        where
            E: Embedding + 'static,
        {
            Self::wrap(
                modem,
                sysloop,
                timer,
                nvs,
                mounted_event_fs,
                stack.network().embedding().context(),
            )
        }

        /// Wrap existing parts into a new instance of the `EspMatterThread` type.
        pub fn wrap(
            modem: impl Peripheral<P = Modem> + 'd,
            sysloop: EspSystemEventLoop,
            timer: EspTaskTimerService,
            nvs: EspDefaultNvsPartition,
            mounted_event_fs: Arc<MountedEventfs>,
            ble_context: &'a EspBtpGattContext,
        ) -> Self {
            into_ref!(modem);

            Self {
                modem,
                sysloop,
                timer,
                nvs,
                mounted_event_fs,
                ble_context,
            }
        }
    }

    impl Wireless for EspMatterThread<'_, '_> {
        type Data = ThreadData;

        async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
        where
            A: WirelessTask<Data = Self::Data>,
        {
            let mut thread = EspThread::new(
                &mut self.modem, 
                self.sysloop.clone(), 
                self.nvs.clone(), 
                self.mounted_event_fs.clone(),
            )
            .unwrap();

            thread.init().unwrap();

            task.run(
                wifi.clone(),
                edge_nal_std::Stack::new(),
                SvcWifiController::new(wifi),
            )
            .await
        }
    }

    impl Gatt for EspMatterThread<'_, '_> {
        async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
        where
            A: GattTask,
        {
            let bt = BtDriver::new(&mut self.modem, Some(self.nvs.clone())).unwrap();

            let peripheral =
                EspBtpGattPeripheral::<bt::Ble>::new(GATTS_APP_ID, bt, self.ble_context).unwrap();

            task.run(peripheral).await
        }
    }

    impl WirelessCoex for EspMatterThread<'_, '_> {
        type Data = ThreadData;

        async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
        where
            A: WirelessCoexTask<Data = Self::Data>,
        {
            #[cfg(esp32h2)]
            let (thread_p, bt_p) = self.modem.split_ref();

            #[cfg(esp32c6)]
            let (_, thread_p, bt_p) = self.modem.split_ref();

            let mut thread = EspThread::new(
                thread_p, 
                self.sysloop.clone(), 
                self.nvs.clone(), 
                self.mounted_event_fs.clone(),
            )
            .unwrap();

            thread.init().unwrap();

            let bt = BtDriver::new(bt_p, Some(self.nvs.clone())).unwrap();

            let peripheral =
                EspBtpGattPeripheral::<bt::Ble>::new(GATTS_APP_ID, bt, self.ble_context).unwrap();

            task.run(
                wifi.clone(),
                edge_nal_std::Stack::new(),
                SvcWifiController::new(wifi),
                peripheral,
            )
            .await
        }
    }

    
}

#[cfg(esp_idf_comp_esp_wifi_enabled)]
mod wifi {
    use alloc::sync::Arc;

    use embassy_sync::mutex::Mutex;

    use embedded_svc::wifi::asynch::Wifi as WifiSvc;

    use enumset::EnumSet;

    use esp_idf_svc::bt::{self, BtDriver};
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::hal::into_ref;
    use esp_idf_svc::hal::modem::Modem;
    use esp_idf_svc::hal::peripheral::{Peripheral, PeripheralRef};
    use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;
    use esp_idf_svc::handle::RawHandle;
    use esp_idf_svc::netif::EspNetif;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use esp_idf_svc::sys::{esp, EspError};
    use esp_idf_svc::timer::EspTaskTimerService;
    use esp_idf_svc::wifi::{AccessPointInfo, AsyncWifi, Capability, Configuration, EspWifi};

    use rs_matter::error::Error;

    use rs_matter_stack::netif::{Netif, NetifConf};
    use rs_matter_stack::network::{Embedding, Network};
    use rs_matter_stack::wireless::svc::SvcWifiController;
    use rs_matter_stack::wireless::{
        Gatt, GattTask, Wifi, WifiData, Wireless, WirelessCoex, WirelessCoexTask, WirelessTask,
    };

    use crate::ble::{EspBtpGattContext, EspBtpGattPeripheral};
    use crate::error::to_net_error;
    use crate::netif::EspMatterNetif;

    use super::{EspWirelessMatterStack, GATTS_APP_ID};

    /// A type alias for an ESP-IDF Matter stack running over Wifi (and BLE, during commissioning).
    pub type EspWifiMatterStack<'a, E> = EspWirelessMatterStack<'a, Wifi, E>;

    /// The relation between a network interface and a controller is slightly different
    /// in the ESP-IDF crates compared to what `rs-matter-stack` wants, hence we need this helper type.
    #[derive(Clone)]
    pub struct EspSharedWifi<'a>(
        Arc<Mutex<EspRawMutex, AsyncWifi<EspWifi<'a>>>>,
        EspSystemEventLoop,
    );

    impl<'a> EspSharedWifi<'a> {
        /// Create a new instance of the `EspSharedWifi` type.
        pub fn new(wifi: AsyncWifi<EspWifi<'a>>, sysloop: EspSystemEventLoop) -> Self {
            Self(Arc::new(Mutex::new(wifi)), sysloop)
        }
    }

    impl WifiSvc for EspSharedWifi<'_> {
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

            wifi.connect().await?;

            // Matter needs an IPv6 address to work
            esp!(unsafe {
                esp_idf_svc::sys::esp_netif_create_ip6_linklocal(
                    wifi.wifi().sta_netif().handle() as _
                )
            })?;

            Ok(())
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

    impl Netif for EspSharedWifi<'_> {
        async fn get_conf(&self) -> Result<Option<NetifConf>, Error> {
            let wifi = self.0.lock().await;

            EspMatterNetif::new(wifi.wifi().sta_netif(), self.1.clone())
                .get_conf()
                .await
        }

        async fn wait_conf_change(&self) -> Result<(), Error> {
            // Wait on any conf change
            // We anyway cannot lock the wifi mutex here (would be a deadlock), so we just wait for the event

            EspMatterNetif::<EspNetif>::wait_any_conf_change(&self.1)
                .await
                .map_err(to_net_error)?;

            Ok(())
        }
    }

    /// A `Wireless` trait implementation via ESP-IDF's Wifi/BT modem
    pub struct EspMatterWifi<'a, 'd> {
        modem: PeripheralRef<'d, Modem>,
        sysloop: EspSystemEventLoop,
        timer: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        ble_context: &'a EspBtpGattContext,
    }

    impl<'a, 'd> EspMatterWifi<'a, 'd> {
        /// Create a new instance of the `EspMatterWifi` type.
        pub fn new<E>(
            modem: impl Peripheral<P = Modem> + 'd,
            sysloop: EspSystemEventLoop,
            timer: EspTaskTimerService,
            nvs: EspDefaultNvsPartition,
            stack: &'a EspWifiMatterStack<E>,
        ) -> Self
        where
            E: Embedding + 'static,
        {
            Self::wrap(
                modem,
                sysloop,
                timer,
                nvs,
                stack.network().embedding().context(),
            )
        }

        /// Wrap existing parts into a new instance of the `EspMatterWifi` type.
        pub fn wrap(
            modem: impl Peripheral<P = Modem> + 'd,
            sysloop: EspSystemEventLoop,
            timer: EspTaskTimerService,
            nvs: EspDefaultNvsPartition,
            ble_context: &'a EspBtpGattContext,
        ) -> Self {
            into_ref!(modem);

            Self {
                modem,
                sysloop,
                timer,
                nvs,
                ble_context,
            }
        }
    }

    impl Wireless for EspMatterWifi<'_, '_> {
        type Data = WifiData;

        async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
        where
            A: WirelessTask<Data = Self::Data>,
        {
            let wifi = AsyncWifi::wrap(
                EspWifi::new(
                    &mut self.modem,
                    self.sysloop.clone(),
                    Some(self.nvs.clone()),
                )
                .map_err(to_net_error)?,
                self.sysloop.clone(),
                self.timer.clone(),
            )
            .map_err(to_net_error)?;

            let wifi = EspSharedWifi::new(wifi, self.sysloop.clone());

            task.run(
                wifi.clone(),
                edge_nal_std::Stack::new(),
                SvcWifiController::new(wifi),
            )
            .await
        }
    }

    impl Gatt for EspMatterWifi<'_, '_> {
        async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
        where
            A: GattTask,
        {
            let bt = BtDriver::new(&mut self.modem, Some(self.nvs.clone())).unwrap();

            let peripheral =
                EspBtpGattPeripheral::<bt::Ble>::new(GATTS_APP_ID, bt, self.ble_context).unwrap();

            task.run(peripheral).await
        }
    }

    impl WirelessCoex for EspMatterWifi<'_, '_> {
        type Data = WifiData;

        async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
        where
            A: WirelessCoexTask<Data = Self::Data>,
        {
            #[cfg(not(esp32c6))]
            let (wifi_p, bt_p) = self.modem.split_ref();

            #[cfg(esp32c6)]
            let (wifi_p, _, bt_p) = self.modem.split_ref();

            let wifi = AsyncWifi::wrap(
                EspWifi::new(wifi_p, self.sysloop.clone(), Some(self.nvs.clone()))
                    .map_err(to_net_error)?,
                self.sysloop.clone(),
                self.timer.clone(),
            )
            .map_err(to_net_error)?;

            let wifi = EspSharedWifi::new(wifi, self.sysloop.clone());

            let bt = BtDriver::new(bt_p, Some(self.nvs.clone())).unwrap();

            let peripheral =
                EspBtpGattPeripheral::<bt::Ble>::new(GATTS_APP_ID, bt, self.ble_context).unwrap();

            task.run(
                wifi.clone(),
                edge_nal_std::Stack::new(),
                SvcWifiController::new(wifi),
                peripheral,
            )
            .await
        }
    }
}
