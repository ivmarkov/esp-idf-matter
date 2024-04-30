use rs_matter::{
    attribute_enum, command_enum,
    data_model::objects::*,
    error::{Error, ErrorCode},
    tlv::{TLVElement, TagType},
    transport::exchange::Exchange,
    utils::rand::Rand,
};

use log::info;

use rs_matter_macros::{FromTLV, ToTLV};
use strum::{EnumDiscriminants, FromRepr};

pub const ID: u32 = 0x0036;

#[derive(FromRepr, EnumDiscriminants)]
#[repr(u16)]
pub enum Attributes {
    Bssid = 0x00,
    SecurityType(AttrType<WiFiSecurity>) = 0x01,
    WifiVersion(AttrType<WiFiVersion>) = 0x02,
    ChannelNumber(AttrType<u16>) = 0x03,
    Rssi(AttrType<i8>) = 0x04,
    BeaconLostCount(AttrType<u32>) = 0x05,
    BeaconRxCount(AttrType<u32>) = 0x06,
    PacketMulticastRxCount(AttrType<u32>) = 0x07,
    PacketMulticastTxCount(AttrType<u32>) = 0x08,
    PacketUnicastRxCount(AttrType<u32>) = 0x09,
    PacketUnicastTxCount(AttrType<u32>) = 0x0a,
    CurrentMaxRate(AttrType<u64>) = 0x0b,
    OverrunCount(AttrType<u64>) = 0x0c,
}

attribute_enum!(Attributes);

#[derive(FromRepr, EnumDiscriminants)]
#[repr(u32)]
pub enum Commands {
    ResetCounts = 0x0,
}

command_enum!(Commands);

pub const CLUSTER: Cluster<'static> = Cluster {
    id: ID as _,
    feature_map: 0,
    attributes: &[
        FEATURE_MAP,
        ATTRIBUTE_LIST,
        Attribute::new(
            AttributesDiscriminants::Bssid as u16,
            Access::RV,
            Quality::NONE,
        ),
        Attribute::new(
            AttributesDiscriminants::SecurityType as u16,
            Access::RV,
            Quality::FIXED,
        ),
        Attribute::new(
            AttributesDiscriminants::WifiVersion as u16,
            Access::RV,
            Quality::FIXED,
        ),
        Attribute::new(
            AttributesDiscriminants::ChannelNumber as u16,
            Access::RV,
            Quality::FIXED,
        ),
        Attribute::new(
            AttributesDiscriminants::Rssi as u16,
            Access::RV,
            Quality::FIXED,
        ),
    ],
    commands: &[CommandsDiscriminants::ResetCounts as _],
};

#[derive(Debug, Copy, Clone, Eq, PartialEq, FromTLV, ToTLV, FromRepr)]
#[repr(u8)]
pub enum WiFiSecurity {
    Unspecified = 0,
    Unencrypted = 1,
    Wep = 2,
    WpaPersonal = 3,
    Wpa2Personal = 4,
    Wpa3Personal = 5,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, FromTLV, ToTLV, FromRepr)]
#[repr(u8)]
pub enum WiFiVersion {
    A = 0,
    B = 1,
    G = 2,
    N = 3,
    AC = 4,
    AX = 5,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, FromTLV, ToTLV, FromRepr)]
#[repr(u8)]
pub enum AssociationFailure {
    Unknown = 0,
    AssociationFailed = 1,
    AuthenticationFailed = 2,
    SsidNotFound = 3,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, FromTLV, ToTLV, FromRepr)]
#[repr(u8)]
pub enum ConnectionStatus {
    Connected = 0,
    NotConnected = 1,
}

pub struct WifiNwDiagCluster {
    data_ver: Dataver,
}

impl WifiNwDiagCluster {
    pub fn new(rand: Rand) -> Self {
        Self {
            data_ver: Dataver::new(rand),
        }
    }

    pub fn read(&self, attr: &AttrDetails, encoder: AttrDataEncoder) -> Result<(), Error> {
        if let Some(mut writer) = encoder.with_dataver(self.data_ver.get())? {
            if attr.is_system() {
                CLUSTER.read(attr.attr_id, writer)
            } else {
                // TODO
                match attr.attr_id.try_into()? {
                    Attributes::Bssid => writer.str8(TagType::Anonymous, &[0; 6]),
                    Attributes::SecurityType(codec) => {
                        codec.encode(writer, WiFiSecurity::Unspecified)
                    }
                    Attributes::WifiVersion(codec) => codec.encode(writer, WiFiVersion::B),
                    Attributes::ChannelNumber(codec) => codec.encode(writer, 20),
                    Attributes::Rssi(codec) => codec.encode(writer, 0),
                    _ => Err(ErrorCode::AttributeNotFound.into()),
                }
            }
        } else {
            Ok(())
        }
    }

    pub fn write(&self, _attr: &AttrDetails, data: AttrData) -> Result<(), Error> {
        let _data = data.with_dataver(self.data_ver.get())?;

        self.data_ver.changed();

        Ok(())
    }

    pub fn invoke(
        &self,
        _exchange: &Exchange,
        cmd: &CmdDetails,
        _data: &TLVElement,
        _encoder: CmdDataEncoder,
    ) -> Result<(), Error> {
        match cmd.cmd_id.try_into()? {
            Commands::ResetCounts => {
                info!("ResetCounts: Not yet supported");
            }
        }

        self.data_ver.changed();

        Ok(())
    }
}

impl Handler for WifiNwDiagCluster {
    fn read(&self, attr: &AttrDetails, encoder: AttrDataEncoder) -> Result<(), Error> {
        WifiNwDiagCluster::read(self, attr, encoder)
    }

    fn write(&self, attr: &AttrDetails, data: AttrData) -> Result<(), Error> {
        WifiNwDiagCluster::write(self, attr, data)
    }

    fn invoke(
        &self,
        exchange: &Exchange,
        cmd: &CmdDetails,
        data: &TLVElement,
        encoder: CmdDataEncoder,
    ) -> Result<(), Error> {
        WifiNwDiagCluster::invoke(self, exchange, cmd, data, encoder)
    }
}

// TODO: Might be removed once the `on` member is externalized
impl NonBlockingHandler for WifiNwDiagCluster {}
