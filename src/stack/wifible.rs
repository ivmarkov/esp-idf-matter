#![cfg(all(
    not(esp32h2),
    not(esp32s2),
    esp_idf_comp_esp_wifi_enabled,
    esp_idf_comp_esp_event_enabled,
    not(esp_idf_btdm_ctrl_mode_br_edr_only),
    esp_idf_bt_enabled,
    esp_idf_bt_bluedroid_enabled,
    feature = "std"
))]

use core::net::SocketAddr;

use std::io;

use alloc::boxed::Box;

use edge_nal::UdpBind;
use edge_nal_std::{Stack, UdpSocket};

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;

use embedded_svc::wifi::asynch::Wifi;

use enumset::EnumSet;

use esp_idf_svc::bt::{Ble, BtDriver};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripheral::{Peripheral, PeripheralRef};
use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;
use esp_idf_svc::hal::{into_ref, modem};
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{esp, esp_netif_create_ip6_linklocal, EspError};
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{AccessPointInfo, AsyncWifi, Capability, Configuration, EspWifi};

use rs_matter::error::{Error, ErrorCode};
use rs_matter::utils::init::{init, Init};

use rs_matter_stack::modem::{Modem, WifiDevice};
use rs_matter_stack::netif::{Netif, NetifConf};
use rs_matter_stack::network::{Embedding, Network};
use rs_matter_stack::persist::KvBlobBuf;
use rs_matter_stack::{MatterStack, WifiBle};

use crate::ble::{BtpGattContext, BtpGattPeripheral};

use super::netif::EspMatterNetif;

pub type EspWifiBleMatterStack<'a, E> = MatterStack<'a, EspWifiBle<E>>;
pub type EspWifiBle<E> = WifiBle<EspRawMutex, KvBlobBuf<EspGatt<E>>>;

/// An embedding of the ESP IDF Bluedroid Gatt peripheral context for the `WifiBle` network type from `rs-matter-stack`.
/// Allows the memory of this context to be statically allocated and cost-initialized.
///
/// Usage:
/// ```no_run
/// MatterStack<WifiBle<EspGatt<E>>>::new();
/// ```
///
/// ... where `E` can be a next-level, user-supplied embedding or just `()` if the user does not need to embed anything.
pub struct EspGatt<E = ()> {
    btp_gatt_context: BtpGattContext,
    embedding: E,
}

impl<E> EspGatt<E>
where
    E: Embedding,
{
    #[allow(clippy::large_stack_frames)]
    #[inline(always)]
    const fn new() -> Self {
        Self {
            btp_gatt_context: BtpGattContext::new(),
            embedding: E::INIT,
        }
    }

    fn init() -> impl Init<Self> {
        init!(Self {
            btp_gatt_context <- BtpGattContext::init(),
            embedding <- E::init(),
        })
    }

    pub fn context(&self) -> &BtpGattContext {
        &self.btp_gatt_context
    }

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

pub struct EspModem<'a, 'd> {
    context: &'a BtpGattContext,
    modem: PeripheralRef<'d, modem::Modem>,
    sysloop: EspSystemEventLoop,
    timers: EspTaskTimerService,
    nvs: EspDefaultNvsPartition,
}

impl<'a, 'd> EspModem<'a, 'd> {
    pub fn new<E>(
        modem: impl Peripheral<P = modem::Modem> + 'd,
        sysloop: EspSystemEventLoop,
        timers: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        stack: &'a EspWifiBleMatterStack<E>,
    ) -> Self
    where
        E: Embedding + 'static,
    {
        Self::wrap(
            modem,
            sysloop,
            timers,
            nvs,
            stack.network().embedding().embedding().context(),
        )
    }

    pub fn wrap(
        modem: impl Peripheral<P = modem::Modem> + 'd,
        sysloop: EspSystemEventLoop,
        timers: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        context: &'a BtpGattContext,
    ) -> Self {
        into_ref!(modem);

        Self {
            context,
            modem,
            sysloop,
            timers,
            nvs,
        }
    }
}

impl<'a, 'd> Modem for EspModem<'a, 'd> {
    type BleDevice<'t> = BtpGattPeripheral<'t, 't, Ble> where Self: 't;

    type WifiDevice<'t> = EspWifiDevice<'t> where Self: 't;

    async fn ble(&mut self) -> Self::BleDevice<'_> {
        let bt = BtDriver::new(&mut self.modem, Some(self.nvs.clone())).unwrap();

        let peripheral = BtpGattPeripheral::new(GATTS_APP_ID, bt, self.context).unwrap();

        peripheral
    }

    async fn wifi(&mut self) -> Self::WifiDevice<'_> {
        EspWifiDevice {
            sysloop: self.sysloop.clone(),
            wifi: Mutex::new(Box::new(
                AsyncWifi::wrap(
                    EspWifi::new(
                        &mut self.modem,
                        self.sysloop.clone(),
                        Some(self.nvs.clone()),
                    )
                    .unwrap(),
                    self.sysloop.clone(),
                    self.timers.clone(),
                )
                .unwrap(),
            )),
        }
    }
}

pub struct EspWifiDevice<'d> {
    sysloop: EspSystemEventLoop,
    wifi: Mutex<NoopRawMutex, Box<AsyncWifi<EspWifi<'d>>>>,
}

impl<'d> WifiDevice for EspWifiDevice<'d> {
    type L2<'t> = EspL2<'t, 'd> where Self: 't;

    type L3<'t> = EspL3<'t, 'd> where Self: 't;

    async fn split(&mut self) -> (Self::L2<'_>, Self::L3<'_>) {
        (EspL2(self), EspL3(self))
    }
}

pub struct EspL2<'a, 'd>(&'a EspWifiDevice<'d>);

impl<'a, 'd> Wifi for EspL2<'a, 'd> {
    type Error = EspError;

    async fn get_capabilities(&self) -> Result<EnumSet<Capability>, Self::Error> {
        self.0.wifi.lock().await.get_capabilities()
    }

    async fn get_configuration(&self) -> Result<Configuration, Self::Error> {
        self.0.wifi.lock().await.get_configuration()
    }

    async fn set_configuration(&mut self, conf: &Configuration) -> Result<(), Self::Error> {
        self.0.wifi.lock().await.set_configuration(conf)
    }

    async fn start(&mut self) -> Result<(), Self::Error> {
        self.0.wifi.lock().await.start().await
    }

    async fn stop(&mut self) -> Result<(), Self::Error> {
        self.0.wifi.lock().await.stop().await
    }

    async fn connect(&mut self) -> Result<(), Self::Error> {
        let mut wifi = self.0.wifi.lock().await;

        wifi.connect().await?;

        esp!(unsafe { esp_netif_create_ip6_linklocal(wifi.wifi().sta_netif().handle() as _) })?;

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), Self::Error> {
        self.0.wifi.lock().await.disconnect().await
    }

    async fn is_started(&self) -> Result<bool, Self::Error> {
        self.0.wifi.lock().await.is_started()
    }

    async fn is_connected(&self) -> Result<bool, Self::Error> {
        self.0.wifi.lock().await.is_connected()
    }

    async fn scan_n<const N: usize>(
        &mut self,
    ) -> Result<(heapless::Vec<AccessPointInfo, N>, usize), Self::Error> {
        self.0.wifi.lock().await.scan_n().await
    }

    async fn scan(&mut self) -> Result<alloc::vec::Vec<AccessPointInfo>, Self::Error> {
        self.0.wifi.lock().await.scan().await
    }
}

pub struct EspL3<'a, 'd>(&'a EspWifiDevice<'d>);

impl<'a, 'd> Netif for EspL3<'a, 'd> {
    async fn get_conf(&self) -> Result<Option<NetifConf>, Error> {
        let wifi = self.0.wifi.lock().await;

        Ok(EspMatterNetif::get_netif_conf(wifi.wifi().sta_netif()).ok())
    }

    async fn wait_conf_change(&self) -> Result<(), Error> {
        EspMatterNetif::wait_any_conf_change(&self.0.sysloop)
            .await
            .map_err(|_| ErrorCode::NoNetworkInterface)?; // TODO

        Ok(())
    }
}

impl<'a, 'd> UdpBind for EspL3<'a, 'd> {
    type Error = io::Error;

    type Socket<'t> = UdpSocket where Self: 't;

    async fn bind(&self, local: SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
        Stack::new().bind(local).await
    }
}
