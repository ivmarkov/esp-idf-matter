//! An example utilizing the `MatterStack<WifiBle>` struct.
//! As the name suggests, this Matter stack assembly uses Wifi as the main transport, and BLE for commissioning.
//! If use want to use Ethernet, utilize `MatterStack<Eth>` instead.
//!
//! The example implements a fictitious Light device (an on-off cluster).

use core::borrow::Borrow;
use core::pin::pin;

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};

use esp_idf_matter::{Error, MatterStack, WifiBle};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::timer::EspTaskTimerService;

use log::info;

use rs_matter::data_model::cluster_basic_information::BasicInfoConfig;
use rs_matter::data_model::cluster_on_off;
use rs_matter::data_model::device_types::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter::data_model::objects::{Endpoint, HandlerCompat, Node};
use rs_matter::data_model::system_model::descriptor;
use rs_matter::secure_channel::spake2p::VerifierData;
use rs_matter::utils::select::Coalesce;
use rs_matter::CommissioningData;

use static_cell::ConstStaticCell;

#[path = "dev_att/dev_att.rs"]
mod dev_att;

fn main() -> Result<(), Error> {
    // Take the Matter stack (can be done only once),
    // as we'll run it in this thread
    let stack = MATTER_STACK.take();

    // Our "light" on-off cluster.
    // Can be anything implementing `rs_matter::data_model::AsyncHandler`
    let on_off = cluster_on_off::OnOffCluster::new(*stack.matter().borrow());

    // Chain our endpoint clusters with the
    // (root) Endpoint 0 system clusters in the final handler
    let handler = HandlerCompat(
        stack
            .root_handler()
            // Our on-off cluster, on Endpoint 1
            .chain(LIGHT_ENDPOINT_ID, cluster_on_off::ID, &on_off)
            // Each Endpoint needs a Descriptor cluster too
            // Just use the one that `rs-matter` provides out of the box
            .chain(
                LIGHT_ENDPOINT_ID,
                descriptor::ID,
                descriptor::DescriptorCluster::new(*stack.matter().borrow()),
            ),
    );

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but saves some memory due to `rustc`
    // not being very intelligent w.r.t. stack usage in async functions
    let mut matter = pin!(stack.run(
        // The Matter stack needs (a clone of) the system event loop
        EspSystemEventLoop::take()?,
        // The Matter stack needs (a clone of) the timer service
        EspTaskTimerService::new()?,
        // The Matter stack needs (a clone of) the default ESP IDF NVS partition
        EspDefaultNvsPartition::take()?,
        // The Matter stack needs the BT/Wifi modem peripheral - and in general -
        // the Bluetooth / Wifi connections will be managed by the Matter stack itself
        // For finer-grained control, call `MatterStack::is_commissioned`,
        // `MatterStack::commission` and `MatterStack::operate`
        Peripherals::take()?.modem,
        // Hard-coded for demo purposes
        CommissioningData {
            verifier: VerifierData::new_with_pw(123456, *stack.matter().borrow()),
            discriminator: 250,
        },
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
    ));

    // Just for demoing purposes:
    //
    // Run a sample loop that simulates state changes triggered by the HAL
    // Changes will be properly communicated to the Matter controllers
    // (i.e. Google Home, Alexa) and other Matter devices thanks to subscriptions
    let mut device = pin!(async {
        loop {
            // Simulate user toggling the light with a physical switch every 5 seconds
            Timer::after(Duration::from_secs(5)).await;

            // Toggle
            on_off.set(!on_off.get());

            // Let the Matter stack know that we have changed
            // the state of our Light device
            stack.notify_changed();

            info!("Light toggled");
        }
    });

    // Schedule the Matter run & the device loop together
    esp_idf_svc::hal::task::block_on(select(&mut matter, &mut device).coalesce())?;

    Ok(())
}

/// The Matter stack is allocated statically to avoid
/// program stack blowups.
/// It is also a mandatory requirement when the `WifiBle` stack variation is used.
static MATTER_STACK: ConstStaticCell<MatterStack<WifiBle>> =
    ConstStaticCell::new(MatterStack::new(
        &BasicInfoConfig {
            vid: 0xFFF1,
            pid: 0x8000,
            hw_ver: 2,
            sw_ver: 1,
            sw_ver_str: "1",
            serial_no: "aabbccdd",
            device_name: "MyLight",
            product_name: "ACME Light",
            vendor_name: "ACME",
        },
        &dev_att::HardCodedDevAtt::new(),
    ));

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;

/// The Matter Light device Node
const NODE: Node<'static> = Node {
    id: 0,
    endpoints: &[
        MatterStack::<WifiBle>::root_metadata(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_type: DEV_TYPE_ON_OFF_LIGHT,
            clusters: &[descriptor::CLUSTER, cluster_on_off::CLUSTER],
        },
    ],
};
