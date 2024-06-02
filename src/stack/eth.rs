use rs_matter_stack::{persist::KvBlobBuf, Eth, MatterStack};

pub type EspEthMatterStack<'a, E> = MatterStack<'a, EspEth<E>>;
pub type EspEth<E> = Eth<KvBlobBuf<E>>;
