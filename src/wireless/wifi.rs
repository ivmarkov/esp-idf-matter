use esp_idf_svc::bt::{self, BtDriver};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::into_ref;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::peripheral::{Peripheral, PeripheralRef};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{AsyncWifi, EspWifi};

use rs_matter_stack::matter::dm::networks::wireless::Wifi;
use rs_matter_stack::matter::error::Error;

use rs_matter_stack::mdns::{BuiltinMdns, Mdns};
use rs_matter_stack::network::{Embedding, Network};
use rs_matter_stack::wireless::{Gatt, GattTask, WifiCoex, WifiCoexTask, WifiTask};

use crate::ble::{EspBtpGattContext, EspBtpGattPeripheral};
use crate::error::to_net_error;
use crate::netif::EspMatterNetStack;
use crate::wifi::{EspMatterWifiCtl, EspMatterWifiNotif};

use super::{EspWirelessMatterStack, GATTS_APP_ID};

/// A type alias for an ESP-IDF Matter stack running over Wifi (and BLE, during commissioning).
pub type EspWifiMatterStack<'a, E> = EspWirelessMatterStack<'a, Wifi, E>;

/// A `Wifi` trait implementation via ESP-IDF's Wifi/BT modem
pub struct EspMatterWifi<'a, 'd, M = BuiltinMdns> {
    modem: PeripheralRef<'d, Modem>,
    sysloop: EspSystemEventLoop,
    timer: EspTaskTimerService,
    nvs: EspDefaultNvsPartition,
    mdns: M,
    ble_context: &'a EspBtpGattContext,
}

impl<'a, 'd> EspMatterWifi<'a, 'd, BuiltinMdns> {
    /// Create a new instance of the `EspMatterWifi` type .
    pub fn new_with_builtin_mdns<E>(
        modem: impl Peripheral<P = Modem> + 'd,
        sysloop: EspSystemEventLoop,
        timer: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        stack: &'a EspWifiMatterStack<E>,
    ) -> Self
    where
        E: Embedding + 'static,
    {
        Self::new(modem, sysloop, timer, nvs, stack, BuiltinMdns)
    }
}

impl<'a, 'd, M> EspMatterWifi<'a, 'd, M>
where
    M: Mdns,
{
    /// Create a new instance of the `EspMatterWifi` type.
    pub fn new<E>(
        modem: impl Peripheral<P = Modem> + 'd,
        sysloop: EspSystemEventLoop,
        timer: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        stack: &'a EspWifiMatterStack<E>,
        mdns: M,
    ) -> Self
    where
        E: Embedding + 'static,
    {
        Self::wrap(
            modem,
            sysloop,
            timer,
            nvs,
            mdns,
            stack.network().embedding().context(),
        )
    }

    /// Wrap existing parts into a new instance of the `EspMatterWifi` type.
    pub fn wrap(
        modem: impl Peripheral<P = Modem> + 'd,
        sysloop: EspSystemEventLoop,
        timer: EspTaskTimerService,
        nvs: EspDefaultNvsPartition,
        mdns: M,
        ble_context: &'a EspBtpGattContext,
    ) -> Self {
        into_ref!(modem);

        Self {
            modem,
            sysloop,
            timer,
            nvs,
            mdns,
            ble_context,
        }
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

impl rs_matter_stack::wireless::Wifi for EspMatterWifi<'_, '_> {
    async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
    where
        A: WifiTask,
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

        let wifi = EspMatterWifiCtl::new(wifi, self.sysloop.clone());

        task.run(
            EspMatterNetStack::new(),
            EspMatterWifiNotif::new(&wifi),
            &wifi,
            &mut self.mdns,
        )
        .await
    }
}

impl WifiCoex for EspMatterWifi<'_, '_> {
    async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
    where
        A: WifiCoexTask,
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

        let wifi = EspMatterWifiCtl::new(wifi, self.sysloop.clone());

        let bt = BtDriver::new(bt_p, Some(self.nvs.clone())).unwrap();

        let mut peripheral =
            EspBtpGattPeripheral::<bt::Ble>::new(GATTS_APP_ID, bt, self.ble_context).unwrap();

        task.run(
            EspMatterNetStack::new(),
            EspMatterWifiNotif::new(&wifi),
            &wifi,
            &mut self.mdns,
            &mut peripheral,
        )
        .await
    }
}
