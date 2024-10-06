use rs_matter_stack::{persist::KvBlobBuf, Eth, MatterStack};

/// A type alias for an ESP-IDF Matter stack running over an Ethernet network (or any other network not managed by Matter).
pub type EspEthMatterStack<'a, E> = MatterStack<'a, EspEth<E>>;

/// A type alias for an ESP-IDF implementation of the `Network` trait for a Matter stack running over
/// an Ethernet network (or any other network not managed by Matter).
pub type EspEth<E> = Eth<KvBlobBuf<E>>;
