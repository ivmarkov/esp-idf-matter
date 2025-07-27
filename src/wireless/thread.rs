use alloc::sync::Arc;

use esp_idf_svc::bt::{self, BtDriver};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::into_ref;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::peripheral::{Peripheral, PeripheralRef};
use esp_idf_svc::io::vfs::MountedEventfs;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::thread::EspThread;

use rs_matter_stack::matter::dm::networks::wireless::Thread;
use rs_matter_stack::matter::error::Error;

use rs_matter_stack::mdns::BuiltinMdns;
use rs_matter_stack::network::{Embedding, Network};
use rs_matter_stack::wireless::{Gatt, GattTask, ThreadCoex, ThreadCoexTask, ThreadTask};

use crate::ble::{EspBtpGattContext, EspBtpGattPeripheral};
use crate::error::to_net_error;
use crate::netif::EspMatterNetStack;
use crate::thread::{EspMatterThreadCtl, EspMatterThreadNotif};

use super::{EspWirelessMatterStack, GATTS_APP_ID};

extern crate alloc;

/// A type alias for an ESP-IDF Matter stack running over Thread (and BLE, during commissioning).
pub type EspThreadMatterStack<'a, E> = EspWirelessMatterStack<'a, Thread, E>;

/// A `Thread` trait implementation via ESP-IDF's Thread/BT modem
// TODO: mDNS via Thread (SRP)
// TODO: How to run the openthread event loop
pub struct EspMatterThread<'a, 'd> {
    modem: PeripheralRef<'d, Modem>,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    mounted_event_fs: Arc<MountedEventfs>,
    ble_context: &'a EspBtpGattContext,
}

impl<'a, 'd> EspMatterThread<'a, 'd> {
    /// Create a new instance of the `EspMatterThread` type.
    pub fn new<E>(
        modem: impl Peripheral<P = Modem> + 'd,
        sysloop: EspSystemEventLoop,
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
            nvs,
            mounted_event_fs,
            stack.network().embedding().context(),
        )
    }

    /// Wrap existing parts into a new instance of the `EspMatterThread` type.
    pub fn wrap(
        modem: impl Peripheral<P = Modem> + 'd,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        mounted_event_fs: Arc<MountedEventfs>,
        ble_context: &'a EspBtpGattContext,
    ) -> Self {
        into_ref!(modem);

        Self {
            modem,
            sysloop,
            nvs,
            mounted_event_fs,
            ble_context,
        }
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

impl rs_matter_stack::wireless::Thread for EspMatterThread<'_, '_> {
    async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
    where
        A: ThreadTask,
    {
        let thread = EspThread::new(
            &mut self.modem,
            self.sysloop.clone(),
            self.nvs.clone(),
            self.mounted_event_fs.clone(),
        )
        .map_err(to_net_error)?;

        let thread = EspMatterThreadCtl::new(thread, self.sysloop.clone());

        task.run(
            EspMatterNetStack::new(),
            EspMatterThreadNotif::new(&thread),
            &thread,
            BuiltinMdns, // TODO XXX FIXME
        )
        .await
    }
}

impl ThreadCoex for EspMatterThread<'_, '_> {
    async fn run<A>(&mut self, mut task: A) -> Result<(), Error>
    where
        A: ThreadCoexTask,
    {
        #[cfg(not(esp32c6))]
        let (thread_p, bt_p) = self.modem.split_ref();

        #[cfg(esp32c6)]
        let (_, thread_p, bt_p) = self.modem.split_ref();

        let thread = EspThread::new(
            thread_p,
            self.sysloop.clone(),
            self.nvs.clone(),
            self.mounted_event_fs.clone(),
        )
        .map_err(to_net_error)?;

        let thread = EspMatterThreadCtl::new(thread, self.sysloop.clone());

        let bt = BtDriver::new(bt_p, Some(self.nvs.clone())).unwrap();

        let mut peripheral =
            EspBtpGattPeripheral::<bt::Ble>::new(GATTS_APP_ID, bt, self.ble_context).unwrap();

        task.run(
            EspMatterNetStack::new(),
            EspMatterThreadNotif::new(&thread),
            &thread,
            BuiltinMdns, // TODO XXX FIXME
            &mut peripheral,
        )
        .await
    }
}
