# (WIP) Run [rs-matter](https://github.com/project-chip/rs-matter) on Espressif chips with [ESP IDF](https://github.com/esp-rs/esp-idf-svc)

[![CI](https://github.com/ivmarkov/esp-idf-matter/actions/workflows/ci.yml/badge.svg)](https://github.com/ivmarkov/esp-idf-matter/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/esp-idf-matter.svg)](https://crates.io/crates/esp-idf-matter)
[![Documentation](https://img.shields.io/badge/docs-esp--rs-brightgreen)](https://ivmarkov.github.io/esp-idf-matter/esp_idf_matter/index.html)
[![Matrix](https://img.shields.io/matrix/ivmarkov:matrix.org?label=join%20matrix&color=BEC5C9&logo=matrix)](https://matrix.to/#/#esp-rs:matrix.org)

## Overview

Everything necessary to run [`rs-matter`](https://github.com/project-chip/rs-matter) on the ESP-IDF:
* Bluedroid implementation of `rs-matter`'s `GattPeripheral` for BLE comissioning support.
* [`rs-matter-stack`](https://github.com/ivmarkov/rs-matter-stack) support with `Netif`, `Ble`, `Wireless` and `KvBlobStore` implementations.

Since ESP-IDF does support the Rust Standard Library, UDP networking just works.

## Example

(See also [All examples](#all-examples))

```rust
//! An example utilizing the `EspWifiNCMatterStack` struct.
//!
//! As the name suggests, this Matter stack assembly uses Wifi as the main transport,
//! (and thus BLE for commissioning), where `NC` stands for non-concurrent commisisoning
//! (i.e., the stack will not run the BLE and Wifi radio simultaneously, which saves memory).
//!
//! If you want to use Ethernet, utilize `EspEthMatterStack` instead.
//! If you want to use concurrent commissioning, utilize `EspWifiMatterStack` instead
//! (Alexa does not work (yet) with non-concurrent commissioning).
//!
//! The example implements a fictitious Light device (an On-Off Matter cluster).

use core::pin::pin;

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};

use esp_idf_matter::matter::data_model::cluster_basic_information::BasicInfoConfig;
use esp_idf_matter::matter::data_model::cluster_on_off;
use esp_idf_matter::matter::data_model::device_types::DEV_TYPE_ON_OFF_LIGHT;
use esp_idf_matter::matter::data_model::objects::{Dataver, Endpoint, HandlerCompat, Node};
use esp_idf_matter::matter::data_model::system_model::descriptor;
use esp_idf_matter::matter::utils::init::InitMaybeUninit;
use esp_idf_matter::matter::utils::select::Coalesce;
use esp_idf_matter::persist;
use esp_idf_matter::stack::test_device::{TEST_BASIC_COMM_DATA, TEST_DEV_ATT, TEST_PID, TEST_VID};
use esp_idf_matter::{init_async_io, EspMatterBle, EspMatterWifi, EspWifiNCMatterStack};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::task::block_on;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::timer::EspTaskTimerService;

use log::{error, info};

use static_cell::StaticCell;

fn main() -> Result<(), anyhow::Error> {
    EspLogger::initialize_default();

    info!("Starting...");

    // Run in a higher-prio thread to avoid issues with `async-io` getting
    // confused by the low priority of the ESP IDF main task
    // Also allocate a very large stack (for now) as `rs-matter` futures do occupy quite some space
    let thread = std::thread::Builder::new()
        .stack_size(75 * 1024)
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
    // Initialize the Matter stack (can be done only once),
    // as we'll run it in this thread
    let stack = MATTER_STACK
        .uninit()
        .init_with(EspWifiNCMatterStack::init_default(
            &BasicInfoConfig {
                vid: TEST_VID,
                pid: TEST_PID,
                hw_ver: 2,
                sw_ver: 1,
                sw_ver_str: "1",
                serial_no: "aabbccdd",
                device_name: "MyLight",
                product_name: "ACME Light",
                vendor_name: "ACME",
            },
            TEST_BASIC_COMM_DATA,
            &TEST_DEV_ATT,
        ));

    // Take some generic ESP-IDF stuff we'll need later
    let sysloop = EspSystemEventLoop::take()?;
    let timers = EspTaskTimerService::new()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let peripherals = Peripherals::take()?;

    // Our "light" on-off cluster.
    // Can be anything implementing `rs_matter::data_model::AsyncHandler`
    let on_off = cluster_on_off::OnOffCluster::new(Dataver::new_rand(stack.matter().rand()));

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
            HandlerCompat(descriptor::DescriptorCluster::new(Dataver::new_rand(
                stack.matter().rand(),
            ))),
        );

    #[cfg(not(esp32c6))]
    let (mut wifi_modem, mut bt_modem) = peripherals.modem.split();

    #[cfg(esp32c6)]
    let (mut wifi_modem, _, mut bt_modem) = peripherals.modem.split();

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but saves some memory due to `rustc`
    // not being very intelligent w.r.t. stack usage in async functions
    let mut matter = pin!(stack.run(
        // The Matter stack needs the Wifi modem peripheral
        EspMatterWifi::new(&mut wifi_modem, sysloop, timers, nvs.clone()),
        // The Matter stack needs the BT modem peripheral
        EspMatterBle::new(&mut bt_modem, nvs.clone(), stack),
        // The Matter stack needs a persister to store its state
        // `EspPersist`+`EspKvBlobStore` saves to a user-supplied NVS partition
        // under namespace `esp-idf-matter`
        persist::new_default(nvs, stack)?,
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
        // No user future to run
        core::future::pending(),
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
static MATTER_STACK: StaticCell<EspWifiNCMatterStack<()>> = StaticCell::new();

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;

/// The Matter Light device Node
const NODE: Node = Node {
    id: 0,
    endpoints: &[
        EspWifiNCMatterStack::<()>::root_metadata(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_types: &[DEV_TYPE_ON_OFF_LIGHT],
            clusters: &[descriptor::CLUSTER, cluster_on_off::CLUSTER],
        },
    ],
};
```

## Future

* Thread networking (for ESP32H2 and ESP32C6)
* Device Attestation data support using secure flash storage
* Setting system time via Matter
* Matter OTA support based on the ESP IDF OTA API

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
