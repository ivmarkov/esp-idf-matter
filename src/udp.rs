#![cfg(all(feature = "std", any(feature = "async-io", feature = "async-io-mini")))]

//! UDP transport implementation for async-io and async-io-mini

use std::net::UdpSocket;

#[cfg(feature = "async-io-mini")]
use async_io_mini as async_io;

use rs_matter::error::{Error, ErrorCode};
use rs_matter::transport::network::{Address, NetworkReceive, NetworkSend};

pub struct Udp<'a>(pub &'a async_io::Async<UdpSocket>);

impl NetworkSend for Udp<'_> {
    async fn send_to(&mut self, data: &[u8], addr: Address) -> Result<(), Error> {
        async_io::Async::<UdpSocket>::send_to(
            self.0,
            data,
            addr.udp().ok_or(ErrorCode::NoNetworkInterface)?,
        )
        .await?;

        Ok(())
    }
}

impl NetworkReceive for Udp<'_> {
    async fn wait_available(&mut self) -> Result<(), Error> {
        async_io::Async::<UdpSocket>::readable(self.0).await?;

        Ok(())
    }

    async fn recv_from(&mut self, buffer: &mut [u8]) -> Result<(usize, Address), Error> {
        let (len, addr) = async_io::Async::<UdpSocket>::recv_from(self.0, buffer).await?;

        Ok((len, Address::Udp(addr)))
    }
}
