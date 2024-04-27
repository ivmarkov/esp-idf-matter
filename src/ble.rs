use core::borrow::Borrow;
use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;

use enumset::enum_set;

use esp_idf_svc::bt::ble::gap::{BleGapEvent, EspBleGap};
use esp_idf_svc::bt::ble::gatt::server::{ConnectionId, EspGatts, GattsEvent, TransferId};
use esp_idf_svc::bt::ble::gatt::{
    AutoResponse, GattCharacteristic, GattDescriptor, GattId, GattInterface, GattResponse,
    GattServiceId, GattStatus, Handle, Permission, Property,
};
use esp_idf_svc::bt::{BdAddr, BleEnabled, BtDriver, BtStatus, BtUuid};
use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;
use esp_idf_svc::sys::{EspError, ESP_FAIL};

use log::warn;

use rs_matter::error::ErrorCode;
use rs_matter::transport::network::btp::{
    AdvData, GattPeripheral, GattPeripheralEvent, C1_CHARACTERISTIC_UUID, C1_MAX_LEN,
    C2_CHARACTERISTIC_UUID, C2_MAX_LEN, MATTER_BLE_SERVICE_UUID16, MAX_BTP_SESSIONS,
};
use rs_matter::transport::network::BtAddr;

use crate::error::Error;

const MAX_CONNECTIONS: usize = MAX_BTP_SESSIONS;

struct ConnState {
    peer: BdAddr,
    conn_id: Handle,
    subscribed: bool,
    mtu: Option<u16>,
}

struct GattsState {
    gatt_if: Option<GattInterface>,
    service_handle: Option<Handle>,
    c1_handle: Option<Handle>,
    c2_handle: Option<Handle>,
    c2_cccd_handle: Option<Handle>,
    connections: heapless::Vec<ConnState, MAX_CONNECTIONS>,
}

pub struct BtpGattContext(Mutex<EspRawMutex, RefCell<GattsState>>);

impl BtpGattContext {
    pub const fn new() -> Self {
        Self(Mutex::new(RefCell::new(GattsState {
            gatt_if: None,
            service_handle: None,
            c1_handle: None,
            c2_handle: None,
            c2_cccd_handle: None,
            connections: heapless::Vec::new(),
        })))
    }

    pub fn reset(&self) {
        self.0.lock(|state| {
            let mut state = state.borrow_mut();

            state.gatt_if = None;
            state.service_handle = None;
            state.c1_handle = None;
            state.c2_handle = None;
            state.c2_cccd_handle = None;
        });
    }
}

/// Implements the `GattPeripheral` trait using the ESP-IDF Bluedroid GATT stack.
struct PeripheralState<'a, 'd, M, T>
where
    T: Borrow<BtDriver<'d, M>>,
    M: BleEnabled,
{
    gap: &'a EspBleGap<'d, M, T>,
    gatts: &'a EspGatts<'d, M, T>,
    state: &'a BtpGattContext,
}

impl<'a, 'd, M, T> PeripheralState<'a, 'd, M, T>
where
    T: Borrow<BtDriver<'d, M>> + Clone,
    M: BleEnabled,
{
    fn new(
        gap: &'a EspBleGap<'d, M, T>,
        gatts: &'a EspGatts<'d, M, T>,
        state: &'a BtpGattContext,
    ) -> Self {
        Self { gap, gatts, state }
    }

    fn indicate(&self, data: &[u8], address: BtAddr) -> Result<bool, EspError> {
        let conn = self.state.0.lock(|state| {
            let state = state.borrow();

            let gatts_if = state.gatt_if?;
            let c2_handle = state.c2_handle?;

            let conn = state
                .connections
                .iter()
                .find(|conn| conn.peer.addr() == address.0 && conn.subscribed)?;

            Some((gatts_if, conn.conn_id, c2_handle))
        });

        if let Some((gatts_if, conn_id, attr_handle)) = conn {
            self.gatts.indicate(gatts_if, conn_id, attr_handle, data)?;

            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn on_gap_event(&self, event: BleGapEvent) -> Result<(), EspError> {
        if let BleGapEvent::RawAdvertisingConfigured(status) = event {
            self.check_bt_status(status)?;
            self.gap.start_advertising()?;
        }

        Ok(())
    }

    fn on_gatts_event<F>(
        &self,
        service_name: &str,
        service_adv_data: &AdvData,
        gatt_if: GattInterface,
        event: GattsEvent,
        mut callback: F,
    ) -> Result<(), EspError>
    where
        F: FnMut(GattPeripheralEvent),
    {
        match event {
            GattsEvent::ServiceRegistered { status, .. } => {
                self.check_gatt_status(status)?;
                self.create_service(gatt_if, service_name, service_adv_data)?;
            }
            GattsEvent::ServiceCreated {
                status,
                service_handle,
                ..
            } => {
                self.check_gatt_status(status)?;
                self.configure_and_start_service(service_handle)?;
            }
            GattsEvent::CharacteristicAdded {
                status,
                attr_handle,
                service_handle,
                char_uuid,
            } => {
                self.check_gatt_status(status)?;
                self.register_characteristic(service_handle, attr_handle, char_uuid)?;
            }
            GattsEvent::DescriptorAdded {
                status,
                attr_handle,
                service_handle,
                descr_uuid,
            } => {
                self.check_gatt_status(status)?;
                self.register_cccd_descriptor(service_handle, attr_handle, descr_uuid)?;
            }
            GattsEvent::ServiceDeleted {
                status,
                service_handle,
            } => {
                self.check_gatt_status(status)?;
                self.delete_service(service_handle)?;
            }
            GattsEvent::ServiceUnregistered {
                status,
                service_handle,
                ..
            } => {
                self.check_gatt_status(status)?;
                self.unregister_service(service_handle)?;
            }
            GattsEvent::Mtu { conn_id, mtu } => {
                self.register_conn_mtu(conn_id, mtu)?;
            }
            GattsEvent::PeerConnected { conn_id, addr, .. } => {
                self.create_conn(conn_id, addr, &mut callback)?;
            }
            GattsEvent::PeerDisconnected { addr, .. } => {
                self.delete_conn(addr, &mut callback)?;
            }
            GattsEvent::Write {
                conn_id,
                trans_id,
                addr,
                handle,
                offset,
                need_rsp,
                is_prep,
                value,
            } => {
                self.write(
                    gatt_if,
                    conn_id,
                    trans_id,
                    addr,
                    handle,
                    offset,
                    need_rsp,
                    is_prep,
                    value,
                    &mut callback,
                )?;
            }
            _ => (),
        }

        Ok(())
    }

    fn check_esp_status(&self, status: Result<(), EspError>) {
        if let Err(e) = status {
            warn!("Got status: {:?}", e);
        }
    }

    fn check_bt_status(&self, status: BtStatus) -> Result<(), EspError> {
        if !matches!(status, BtStatus::Success) {
            warn!("Got status: {:?}", status);
            Err(EspError::from_infallible::<ESP_FAIL>())
        } else {
            Ok(())
        }
    }

    fn check_gatt_status(&self, status: GattStatus) -> Result<(), EspError> {
        if !matches!(status, GattStatus::Ok) {
            warn!("Got status: {:?}", status);
            Err(EspError::from_infallible::<ESP_FAIL>())
        } else {
            Ok(())
        }
    }

    fn create_service(
        &self,
        gatt_if: GattInterface,
        service_name: &str,
        service_adv_data: &AdvData,
    ) -> Result<(), EspError> {
        self.state.0.lock(|state| {
            state.borrow_mut().gatt_if = Some(gatt_if);
        });

        self.gap.set_device_name(service_name)?;
        self.gap.set_raw_adv_conf(
            &service_adv_data
                .service_payload_iter()
                .collect::<heapless::Vec<_, 32>>(),
        )?;
        self.gatts.create_service(
            gatt_if,
            &GattServiceId {
                id: GattId {
                    uuid: BtUuid::uuid16(MATTER_BLE_SERVICE_UUID16),
                    inst_id: 0,
                },
                is_primary: true,
            },
            8,
        )?;

        Ok(())
    }

    fn delete_service(&self, service_handle: Handle) -> Result<(), EspError> {
        self.state.0.lock(|state| {
            if state.borrow().service_handle == Some(service_handle) {
                state.borrow_mut().c1_handle = None;
                state.borrow_mut().c2_handle = None;
                state.borrow_mut().c2_cccd_handle = None;
            }
        });

        Ok(())
    }

    fn unregister_service(&self, service_handle: Handle) -> Result<(), EspError> {
        self.state.0.lock(|state| {
            if state.borrow().service_handle == Some(service_handle) {
                state.borrow_mut().gatt_if = None;
                state.borrow_mut().service_handle = None;
            }
        });

        Ok(())
    }

    fn configure_and_start_service(&self, service_handle: Handle) -> Result<(), EspError> {
        self.state.0.lock(|state| {
            state.borrow_mut().service_handle = Some(service_handle);
        });

        self.gatts.start_service(service_handle)?;
        self.add_characteristics(service_handle)?;

        Ok(())
    }

    fn add_characteristics(&self, service_handle: Handle) -> Result<(), EspError> {
        self.gatts.add_characteristic(
            service_handle,
            &GattCharacteristic {
                uuid: BtUuid::uuid128(C1_CHARACTERISTIC_UUID),
                permissions: enum_set!(Permission::Write),
                properties: enum_set!(Property::Write),
                max_len: C1_MAX_LEN,
                auto_rsp: AutoResponse::ByGatt,
            },
            &[],
        )?;

        self.gatts.add_characteristic(
            service_handle,
            &GattCharacteristic {
                uuid: BtUuid::uuid128(C2_CHARACTERISTIC_UUID),
                permissions: enum_set!(Permission::Write | Permission::Read),
                properties: enum_set!(Property::Indicate),
                max_len: C2_MAX_LEN,
                auto_rsp: AutoResponse::ByGatt,
            },
            &[],
        )?;

        Ok(())
    }

    fn register_characteristic(
        &self,
        service_handle: Handle,
        attr_handle: Handle,
        char_uuid: BtUuid,
    ) -> Result<(), EspError> {
        let c2 = self.state.0.lock(|state| {
            if state.borrow().service_handle != Some(service_handle) {
                return false;
            }

            if char_uuid == BtUuid::uuid128(C1_CHARACTERISTIC_UUID) {
                state.borrow_mut().c1_handle = Some(attr_handle);

                false
            } else if char_uuid == BtUuid::uuid128(C2_CHARACTERISTIC_UUID) {
                state.borrow_mut().c2_handle = Some(attr_handle);

                true
            } else {
                false
            }
        });

        if c2 {
            self.gatts.add_descriptor(
                attr_handle,
                &GattDescriptor {
                    uuid: BtUuid::uuid16(0x2902), // CCCD
                    permissions: enum_set!(Permission::Read | Permission::Write),
                },
            )?;
        }

        Ok(())
    }

    fn register_cccd_descriptor(
        &self,
        service_handle: Handle,
        attr_handle: Handle,
        descr_uuid: BtUuid,
    ) -> Result<(), EspError> {
        self.state.0.lock(|state| {
            if descr_uuid == BtUuid::uuid16(0x2902) && state.borrow().c2_handle == Some(attr_handle)
            {
                state.borrow_mut().c2_cccd_handle = Some(service_handle);
            }
        });

        Ok(())
    }

    fn register_conn_mtu(&self, conn_id: ConnectionId, mtu: u16) -> Result<(), EspError> {
        self.state.0.lock(|state| {
            let mut state = state.borrow_mut();
            if let Some(conn) = state
                .connections
                .iter_mut()
                .find(|conn| conn.conn_id == conn_id)
            {
                conn.mtu = Some(mtu);
            }
        });

        Ok(())
    }

    fn create_conn<F>(
        &self,
        conn_id: ConnectionId,
        addr: BdAddr,
        callback: &mut F,
    ) -> Result<(), EspError>
    where
        F: FnMut(GattPeripheralEvent),
    {
        let added = self.state.0.lock(|state| {
            let mut state = state.borrow_mut();
            if state.connections.len() < MAX_CONNECTIONS {
                state
                    .connections
                    .push(ConnState {
                        peer: addr,
                        conn_id,
                        subscribed: false,
                        mtu: None,
                    })
                    .map_err(|_| ())
                    .unwrap();

                true
            } else {
                false
            }
        });

        if added {
            self.gap.set_conn_params_conf(addr, 10, 20, 0, 400)?;

            callback(GattPeripheralEvent::NotifySubscribed(BtAddr(addr.into())));
        }

        Ok(())
    }

    fn delete_conn<F>(&self, addr: BdAddr, callback: &mut F) -> Result<(), EspError>
    where
        F: FnMut(GattPeripheralEvent),
    {
        self.state.0.lock(|state| {
            let mut state = state.borrow_mut();
            if let Some(index) = state
                .connections
                .iter()
                .position(|ConnState { peer, .. }| *peer == addr)
            {
                state.connections.swap_remove(index);
            }
        });

        callback(GattPeripheralEvent::NotifyUnsubscribed(BtAddr(addr.into())));

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn write<F>(
        &self,
        gatt_if: GattInterface,
        conn_id: ConnectionId,
        trans_id: TransferId,
        addr: BdAddr,
        handle: Handle,
        offset: u16,
        need_rsp: bool,
        is_prep: bool,
        value: &[u8],
        callback: &mut F,
    ) -> Result<(), EspError>
    where
        F: FnMut(GattPeripheralEvent),
    {
        let event = self.state.0.lock(|state| {
            let mut state = state.borrow_mut();
            let c2_handle = state.c2_handle;
            let c2_cccd_handle = state.c2_cccd_handle;

            let conn = state
                .connections
                .iter_mut()
                .find(|conn| conn.conn_id == conn_id)?;

            if c2_cccd_handle == Some(handle) {
                // TODO: What if this needs a response?

                if !is_prep && offset == 0 && value.len() == 2 {
                    let value = u16::from_le_bytes([value[0], value[1]]);
                    if value == 0x02 {
                        if !conn.subscribed {
                            conn.subscribed = true;
                            return Some(GattPeripheralEvent::NotifySubscribed(BtAddr(
                                addr.into(),
                            )));
                        }
                    } else if conn.subscribed {
                        conn.subscribed = false;
                        return Some(GattPeripheralEvent::NotifyUnsubscribed(BtAddr(addr.into())));
                    }
                }
            } else if c2_handle == Some(handle) && offset == 0 {
                // TODO: Is it safe to report the write before it was confirmed?
                return Some(GattPeripheralEvent::Write {
                    address: BtAddr(addr.into()),
                    data: value,
                });
            }

            None
        });

        if let Some(event) = event {
            if matches!(event, GattPeripheralEvent::Write { .. }) && need_rsp {
                let response = if is_prep {
                    // TODO: Do not allocate on-stack
                    let mut response = GattResponse::new();

                    response
                        .attr_handle(handle)
                        .auth_req(0)
                        .offset(0)
                        .value(value)?;

                    Some(response)
                } else {
                    None
                };

                self.gatts.send_response(
                    gatt_if,
                    conn_id,
                    trans_id,
                    GattStatus::Ok,
                    response.as_ref(),
                )?;
            }

            callback(event);
        }

        Ok(())
    }
}

pub struct BluedroidGattPeripheral<'a, 'd, M>
where
    M: BleEnabled,
{
    context: &'a BtpGattContext,
    driver: &'a BtDriver<'d, M>,
}

impl<'a, 'd, M> BluedroidGattPeripheral<'a, 'd, M>
where
    M: BleEnabled,
{
    /// Create a new instance.
    pub fn new(context: &'a BtpGattContext, driver: &'a BtDriver<'d, M>) -> Self {
        Self { context, driver }
    }

    pub fn run<F>(
        &self,
        service_name: &str,
        service_adv_data: &AdvData,
        mut callback: F,
    ) -> Result<(), Error>
    where
        F: FnMut(GattPeripheralEvent) + Send + 'd,
    {
        let _pin = service_adv_data.pin();

        let gap = EspBleGap::new(self.driver)?;
        let gatts = EspGatts::new(self.driver)?;

        unsafe {
            gap.subscribe_nonstatic(|event| {
                let state = PeripheralState::new(&gap, &gatts, &self.context);

                state.check_esp_status(state.on_gap_event(event));
            })?;
        }

        let adv_data = service_adv_data.clone();
        let service_name = service_name.to_owned();

        unsafe {
            gatts.subscribe_nonstatic(|(gatt_if, event)| {
                let state = PeripheralState::new(&gap, &gatts, &self.context);

                state.check_esp_status(state.on_gatts_event(
                    &service_name,
                    &adv_data,
                    gatt_if,
                    event,
                    &mut callback,
                ))
            })?;
        }

        Ok(())
    }

    /// Indicate new data on characteristic `C2` to a remote peer.
    pub fn indicate(&self, data: &[u8], address: BtAddr) -> Result<bool, Error> {
        //Ok(self.0.indicate(data, address)?)
        todo!()
    }
}

impl<'a, 'd, M> GattPeripheral for BluedroidGattPeripheral<'a, 'd, M>
where
    M: BleEnabled,
{
    async fn run<F>(
        &self,
        service_name: &str,
        adv_data: &AdvData,
        callback: F,
    ) -> Result<(), rs_matter::error::Error>
    where
        F: FnMut(GattPeripheralEvent) + Send + Clone + 'static,
    {
        BluedroidGattPeripheral::run(self, service_name, adv_data, callback)
            .map_err(|_| ErrorCode::BtpError)?;

        core::future::pending().await
    }

    async fn indicate(&self, data: &[u8], address: BtAddr) -> Result<(), rs_matter::error::Error> {
        // TODO: Is indicate blocking?
        if BluedroidGattPeripheral::indicate(self, data, address)
            .map_err(|_| ErrorCode::BtpError)?
        {
            Ok(())
        } else {
            Err(ErrorCode::NoNetworkInterface.into())
        }
    }
}
