#![cfg(all(feature = "std", any(feature = "async-io", feature = "async-io-mini")))]

use core::net::{Ipv4Addr, Ipv6Addr};

use std::{io, net::UdpSocket};

#[cfg(feature = "async-io-mini")]
use async_io_mini as async_io;

use log::info;

/// Join an IPV6 multicast group on a specific interface
pub fn join_multicast_v6(
    socket: &async_io::Async<UdpSocket>,
    multiaddr: Ipv6Addr,
    interface: u32,
) -> Result<(), io::Error> {
    #[cfg(not(target_os = "espidf"))]
    socket.as_ref().join_multicast_v6(&multiaddr, interface)?;

    // join_multicast_v6() is broken for ESP-IDF due to mismatch w.r.t. sizes
    // (u8 expected but u32 passed to setsockopt() and sometimes the other way around)
    #[cfg(target_os = "espidf")]
    {
        let mreq = esp_idf_svc::sys::ipv6_mreq {
            ipv6mr_multiaddr: esp_idf_svc::sys::in6_addr {
                un: esp_idf_svc::sys::in6_addr__bindgen_ty_1 {
                    u8_addr: multiaddr.octets(),
                },
            },
            ipv6mr_interface: interface,
        };

        esp_setsockopt(
            socket,
            esp_idf_svc::sys::IPPROTO_IPV6,
            esp_idf_svc::sys::IPV6_ADD_MEMBERSHIP,
            mreq,
        )?;
    }

    info!("Joined IPV6 multicast {}/{}", multiaddr, interface);

    Ok(())
}

/// Join an IPV4 multicast group on a specific interface
pub fn join_multicast_v4(
    socket: &async_io::Async<UdpSocket>,
    multiaddr: Ipv4Addr,
    interface: Ipv4Addr,
) -> Result<(), io::Error> {
    #[cfg(not(target_os = "espidf"))]
    self.socket
        .as_ref()
        .join_multicast_v4(multiaddr, interface)?;

    // join_multicast_v4() is broken for ESP-IDF, most likely due to wrong `ip_mreq` signature in the `libc` crate
    // Note that also most *_multicast_v4 and *_multicast_v6 methods are broken as well in Rust STD for the ESP-IDF
    // due to mismatch w.r.t. sizes (u8 expected but u32 passed to setsockopt() and sometimes the other way around)
    #[cfg(target_os = "espidf")]
    {
        let mreq = esp_idf_svc::sys::ip_mreq {
            imr_multiaddr: esp_idf_svc::sys::in_addr {
                s_addr: u32::from_ne_bytes(multiaddr.octets()),
            },
            imr_interface: esp_idf_svc::sys::in_addr {
                s_addr: u32::from_ne_bytes(interface.octets()),
            },
        };

        esp_setsockopt(
            socket,
            esp_idf_svc::sys::IPPROTO_IP,
            esp_idf_svc::sys::IP_ADD_MEMBERSHIP,
            mreq,
        )?;
    }

    info!("Joined IP multicast {}/{}", multiaddr, interface);

    Ok(())
}

// Most *_multicast_v4 and *_multicast_v6 methods are broken in Rust STD for the ESP-IDF
// due to mismatch w.r.t. sizes (u8 expected but u32 passed to setsockopt() and sometimes the other way around)
#[cfg(target_os = "espidf")]
fn esp_setsockopt<T>(
    socket: &async_io::Async<UdpSocket>,
    proto: u32,
    option: u32,
    value: T,
) -> Result<(), io::Error> {
    use std::os::fd::AsRawFd;

    esp_idf_svc::sys::esp!(unsafe {
        esp_idf_svc::sys::lwip_setsockopt(
            socket.as_raw_fd(),
            proto as _,
            option as _,
            &value as *const _ as *const _,
            core::mem::size_of::<T>() as _,
        )
    })
    .map_err(|_| io::ErrorKind::Other)?;

    Ok(())
}
