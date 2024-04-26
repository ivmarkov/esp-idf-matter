# Run [rs-matter](https://github.com/project-chip/rs-matter) on top of the [Rust ESP IDF SDK wrappers](https://github.com/esp-rs/esp-idf-svc)

[![CI](https://github.com/ivmarkov/esp-idf-matter/actions/workflows/ci.yml/badge.svg)](https://github.com/ivmarkov/esp-idf-matter/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/esp-idf-matter.svg)](https://crates.io/crates/esp-idf-matter)
[![Documentation](https://img.shields.io/badge/docs-esp--rs-brightgreen)](https://ivmarkov.github.io/esp-idf-matter/esp_idf_matter/index.html)
[![Matrix](https://img.shields.io/matrix/ivmarkov:matrix.org?label=join%20matrix&color=BEC5C9&logo=matrix)](https://matrix.to/#/#esp-rs:matrix.org)
[![Wokwi](https://img.shields.io/endpoint?url=https%3A%2F%2Fwokwi.com%2Fbadge%2Fclick-to-simulate.json)](https://wokwi.com/projects/332188235906155092)

## Highlights

This boring crate provides the necessary glue to operate the [`rs-matter` Rust Matter stack]() on Espressif chips with the ESP IDF SDK.

In particular:
* [Bluetooth commissioning support]() with the ESP IDF Bluedroid stack
* WiFi provisioning support via an [ESP IDF specific Matter Network Commissioning Cluster implementation]()
* [Non-volatile storage for Matter data (fabrics and ACLs)]() on top of the ESP IDF NVS flash API
* mDNS:
  * Optional [Matter mDNS responder implementation]() based on the ESP IDF mDNS responder (use if you need to register other services besides Matter in mDNS)
  * [UDP-multicast workarounds]() for `rs-matter`'s built-in mDNS responder, specifc to the Rust wrappers of ESP IDF

For enabling UDP and TCP networking in `rs-matter`, just use the [`async-io`]() crate which has support for ESP IDF out of the box.

## Build Prerequisites

Follow the [Prerequisites](https://github.com/esp-rs/esp-idf-template#prerequisites) section in the `esp-idf-template` crate.

## Examples

The examples could be built and flashed conveniently with [`cargo-espflash`](https://github.com/esp-rs/espflash/). To run e.g. `wifi` on an e.g. ESP32-C3:
(Swap the Rust target and example name with the target corresponding for your ESP32 MCU and with the example you would like to build)

with `cargo-espflash`:
```sh
$ MCU=esp32c3 cargo espflash flash --target riscv32imc-esp-espidf --example wifi --monitor
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
