# (WIP) Run [rs-matter](https://github.com/project-chip/rs-matter) on Espressif chips with [ESP IDF](https://github.com/esp-rs/esp-idf-svc)

[![CI](https://github.com/ivmarkov/esp-idf-matter/actions/workflows/ci.yml/badge.svg)](https://github.com/ivmarkov/esp-idf-matter/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/esp-idf-matter.svg)](https://crates.io/crates/esp-idf-matter)
[![Documentation](https://img.shields.io/badge/docs-esp--rs-brightgreen)](https://ivmarkov.github.io/esp-idf-matter/esp_idf_matter/index.html)
[![Matrix](https://img.shields.io/matrix/ivmarkov:matrix.org?label=join%20matrix&color=BEC5C9&logo=matrix)](https://matrix.to/#/#esp-rs:matrix.org)
[![Wokwi](https://img.shields.io/endpoint?url=https%3A%2F%2Fwokwi.com%2Fbadge%2Fclick-to-simulate.json)](https://wokwi.com/projects/332188235906155092)

## Overview

Everything necessary to run [`rs-matter`](https://github.com/project-chip/rs-matter) on the ESP-IDF:
* Bluedroid implementation of `rs-matter`'s `GattPeripheral` for BLE comissioning support.
* [`rs-matter-stack`](https://github.com/ivmarkov/rs-matter-stack) support with `Netif`, `KvBlobStore` and `Modem` implementations.

Since ESP-IDF does support the Rust Standard Library, UDP networking just works.

## Example

(See also [All examples](#all-examples))

```rust
//! An example utilizing the `EspWifiBleMatterStack` struct.
//! As the name suggests, this Matter stack assembly uses Wifi as the main transport,
//! and BLE for commissioning.
//! If you want to use Ethernet, utilize `EspEthMatterStack` instead.
//!
//! The example implements a fictitious Light device (an On-Off Matter cluster).

use core::borrow::Borrow;
use core::pin::pin;

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};

use esp_idf_matter::{init_async_io, EspWifiBleMatterStack, EspKvBlobStore, EspPersist, EspModem};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::task::block_on;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::timer::EspTaskTimerService;

use log::{error, info};

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

fn main() -> Result<(), anyhow::Error> {
    EspLogger::initialize_default();

    info!("Starting...");

    // Run in a higher-prio thread to avoid issues with `async-io` getting
    // confused by the low priority of the ESP IDF main task
    // Also allocate a very large stack (for now) as `rs-matter` futures do occupy quite some space
    let thread = std::thread::Builder::new()
        .stack_size(65 * 1024)
        .spawn(|| {
            // Eagerly initialize `async-io` to minimize the risk of stack blowups later on
            init_async_io()?;

            run()
        })
        .unwrap();

    thread.join().unwrap()
}

#[inline(never)]
#[cold]
fn run() -> Result<(), anyhow::Error> {
    let result = block_on(matter());

    if let Err(e) = &result {
        error!("Matter aborted execution with error: {:?}", e);
    }
    {
        info!("Matter finished execution successfully");
    }

    result
}

async fn matter() -> Result<(), anyhow::Error> {
    // Take the Matter stack (can be done only once),
    // as we'll run it in this thread
    let stack = MATTER_STACK.take();

    // Take some generic ESP-IDF stuff we'll need later
    let sysloop = EspSystemEventLoop::take()?;
    let timers = EspTaskTimerService::new()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let peripherals = Peripherals::take()?;

    // Our "light" on-off cluster.
    // Can be anything implementing `rs_matter::data_model::AsyncHandler`
    let on_off = cluster_on_off::OnOffCluster::new(*stack.matter().borrow());

    // Chain our endpoint clusters with the
    // (root) Endpoint 0 system clusters in the final handler
    let handler = stack
        .root_handler()
        // Our on-off cluster, on Endpoint 1
        .chain(
            LIGHT_ENDPOINT_ID,
            cluster_on_off::ID,
            HandlerCompat(&on_off),
        )
        // Each Endpoint needs a Descriptor cluster too
        // Just use the one that `rs-matter` provides out of the box
        .chain(
            LIGHT_ENDPOINT_ID,
            descriptor::ID,
            HandlerCompat(descriptor::DescriptorCluster::new(*stack.matter().borrow())),
        );

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but saves some memory due to `rustc`
    // not being very intelligent w.r.t. stack usage in async functions
    let mut matter = pin!(stack.run(
        // The Matter stack needs a persister to store its state
        // `EspPersist`+`EspKvBlobStore` saves to a user-supplied NVS partition
        // under namespace `esp-idf-matter`
        EspPersist::new_wifi_ble(EspKvBlobStore::new_default(nvs.clone())?, stack),
        // The Matter stack needs the BT/Wifi modem peripheral - and in general -
        // the Bluetooth / Wifi connections will be managed by the Matter stack itself
        // For finer-grained control, call `MatterStack::is_commissioned`,
        // `MatterStack::commission` and `MatterStack::operate`
        EspModem::new(peripherals.modem, sysloop, timers, nvs, stack),
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
    select(&mut matter, &mut device).coalesce().await?;

    Ok(())
}

/// The Matter stack is allocated statically to avoid
/// program stack blowups.
/// It is also a mandatory requirement when the `WifiBle` stack variation is used.
static MATTER_STACK: ConstStaticCell<EspWifiBleMatterStack<()>> =
    ConstStaticCell::new(EspWifiBleMatterStack::new_default(
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
const NODE: Node = Node {
    id: 0,
    endpoints: &[
        EspWifiBleMatterStack::<()>::root_metadata(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_type: DEV_TYPE_ON_OFF_LIGHT,
            clusters: &[descriptor::CLUSTER, cluster_on_off::CLUSTER],
        },
    ],
};
```

## Future

* Device Attestation data support using secure flash storage
* Setting system time via Matter
* Matter OTA support based on the ESP IDF OTA API
* Thread networking (for ESP32H2 and ESP32C6)
* Wifi Access-Point based commissioning (for ESP32S2 which does not have Bluetooth support)

## Build Prerequisites

Follow the [Prerequisites](https://github.com/esp-rs/esp-idf-template#prerequisites) section in the `esp-idf-template` crate.

## All examples

The examples could be built and flashed conveniently with [`cargo-espflash`](https://github.com/esp-rs/espflash/). To run e.g. `light` on an e.g. ESP32-C3:
(Swap the Rust target and example name with the target corresponding for your ESP32 MCU and with the example you would like to build)

with `cargo-espflash`:
```sh
$ MCU=esp32c3 cargo espflash flash --target riscv32imc-esp-espidf --example light --features examples --monitor
```

| MCU | "--target" |
| --- | ------ |
| esp32c2 | riscv32imc-esp-espidf |
| esp32c3| riscv32imc-esp-espidf |
| esp32c6| riscv32imac-esp-espidf |
| esp32h2 | riscv32imac-esp-espidf |
| esp32p4 | riscv32imafc-esp-espidf |
| esp32 | xtensa-esp32-espidf |
| esp32s2 | xtensa-esp32s2-espidf |
| esp32s3 | xtensa-esp32s3-espidf |
