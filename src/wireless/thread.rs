use rs_matter_stack::matter::dm::networks::wireless::Wifi;

use super::EspWirelessMatterStack;

/// A type alias for an ESP-IDF Matter stack running over Thread (and BLE, during commissioning).
pub type EspThreadMatterStack<'a, E> = EspWirelessMatterStack<'a, Thread, E>;
