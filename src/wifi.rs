use core::cell::RefCell;

use embassy_sync::blocking_mutex::{self, raw::RawMutex};
use embassy_time::{Duration, Timer};

use log::{info, warn};

use rs_matter::data_model::sdm::nw_commissioning::NetworkCommissioningStatus;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{self, FromTLV, TLVList, TLVWriter, TagType, ToTLV};
use rs_matter::utils::notification::Notification;
use rs_matter::utils::writebuf::WriteBuf;

pub mod comm;
pub mod mgmt;

#[derive(Debug, Clone, ToTLV, FromTLV)]
struct WifiCredentials {
    ssid: heapless::String<32>,
    password: heapless::String<64>,
}

struct WifiStatus {
    ssid: heapless::String<32>,
    status: NetworkCommissioningStatus,
    value: i32,
}

struct WifiState<const N: usize> {
    networks: heapless::Vec<WifiCredentials, N>,
    connected_once: bool,
    connect_requested: Option<heapless::String<32>>,
    status: Option<WifiStatus>,
    changed: bool,
}

impl<const N: usize> WifiState<N> {
    pub(crate) fn get_next_network(&mut self, last_ssid: Option<&str>) -> Option<WifiCredentials> {
        // Return the requested network with priority
        if let Some(ssid) = self.connect_requested.take() {
            let creds = self.networks.iter().find(|creds| creds.ssid == ssid);

            if let Some(creds) = creds {
                info!("Trying with requested network first - SSID: {}", creds.ssid);

                return Some(creds.clone());
            }
        }

        if let Some(last_ssid) = last_ssid {
            info!("Looking for network after the one with SSID: {}", last_ssid);

            // Return the network positioned after the last one used

            let mut networks = self.networks.iter();

            for network in &mut networks {
                if network.ssid.as_str() == last_ssid {
                    break;
                }
            }

            let creds = networks.next();
            if let Some(creds) = creds {
                info!("Trying with next network - SSID: {}", creds.ssid);

                return Some(creds.clone());
            }
        }

        // Wrap over
        info!("Wrapping over");

        self.networks.first().cloned()
    }

    fn reset(&mut self) {
        self.networks.clear();
        self.connected_once = false;
        self.connect_requested = None;
        self.status = None;
        self.changed = false;
    }

    fn load(&mut self, data: &[u8]) -> Result<(), Error> {
        let root = TLVList::new(data).iter().next().ok_or(ErrorCode::Invalid)?;

        tlv::from_tlv(&mut self.networks, &root)?;

        self.changed = false;

        Ok(())
    }

    fn store<'m>(&mut self, buf: &'m mut [u8]) -> Result<Option<&'m [u8]>, Error> {
        if !self.changed {
            return Ok(None);
        }

        let mut wb = WriteBuf::new(buf);
        let mut tw = TLVWriter::new(&mut wb);

        self.networks
            .as_slice()
            .to_tlv(&mut tw, TagType::Anonymous)?;

        self.changed = false;

        let len = tw.get_tail();

        Ok(Some(&buf[..len]))
    }
}

/// The `'static` state of the Wifi module.
/// Isolated as a separate struct to allow for `const fn` construction
/// and static allocation.
pub struct WifiContext<const N: usize, M>
where
    M: RawMutex,
{
    state: blocking_mutex::Mutex<M, RefCell<WifiState<N>>>,
    network_connect_requested: Notification<M>,
}

impl<const N: usize, M> WifiContext<N, M>
where
    M: RawMutex,
{
    /// Create a new instance.
    pub const fn new() -> Self {
        Self {
            state: blocking_mutex::Mutex::new(RefCell::new(WifiState {
                networks: heapless::Vec::new(),
                connected_once: false,
                connect_requested: None,
                status: None,
                changed: false,
            })),
            network_connect_requested: Notification::new(),
        }
    }

    /// Reset the state.
    pub fn reset(&self) {
        self.state.lock(|state| state.borrow_mut().reset());
    }

    /// Load the state from a byte slice.
    pub fn load(&self, data: &[u8]) -> Result<(), Error> {
        self.state.lock(|state| state.borrow_mut().load(data))
    }

    /// Store the state into a byte slice.
    pub fn store<'m>(&self, buf: &'m mut [u8]) -> Result<Option<&'m [u8]>, Error> {
        self.state.lock(|state| state.borrow_mut().store(buf))
    }

    /// Wait until signalled by the Matter stack that a network connect request is issued during commissioning.
    ///
    /// Typically, this is a signal that the BLE/BTP transport should be teared down and
    /// the Wifi transport should be brought up.
    pub async fn wait_network_connect(&self) -> Result<(), crate::error::Error> {
        loop {
            if self
                .state
                .lock(|state| state.borrow().connect_requested.is_some())
            {
                break;
            }

            self.network_connect_requested.wait().await;
        }

        warn!(
            "Giving BLE/BTP extra 4 seconds for any outstanding messages before switching to Wifi"
        );

        Timer::after(Duration::from_secs(4)).await;

        Ok(())
    }
}

impl<const N: usize, M> Default for WifiContext<N, M>
where
    M: RawMutex,
{
    fn default() -> Self {
        Self::new()
    }
}
