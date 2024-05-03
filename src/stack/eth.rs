use core::borrow::Borrow;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::{EspNvsPartition, NvsPartitionId};

use log::info;

use rs_matter::data_model::cluster_basic_information;
use rs_matter::data_model::objects::{
    AsyncHandler, AsyncMetadata, Cluster, Endpoint, HandlerCompat,
};
use rs_matter::data_model::sdm::ethernet_nw_diagnostics::EthNwDiagCluster;
use rs_matter::data_model::sdm::nw_commissioning::EthNwCommCluster;
use rs_matter::data_model::sdm::{
    admin_commissioning, ethernet_nw_diagnostics, general_commissioning, general_diagnostics,
    group_key_management, noc, nw_commissioning,
};
use rs_matter::data_model::system_model::{access_control, descriptor};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::CommissioningData;

use crate::error::Error;
use crate::netif::NetifAccess;
use crate::{handler, MatterStack, Network, RootEndpointHandler};

/// An implementation of the `Network` trait for Ethernet.
///
/// Note that "Ethernet" - in the context of this crate - means
/// not just the Ethernet transport, but also any other IP-based transport
/// (like Wifi or Thread), where the Matter stack would not be concerned
/// with the management of the network transport (as in re-connecting to the
/// network on lost signal, managing network credentials and so on).
///
/// The expectation is nevertheless that for production use-cases
/// the `Eth` network would really only be used for Ethernet.
pub struct Eth(());

impl Network for Eth {
    const INIT: Self = Self(());
}

pub type EthMatterStack<'a> = MatterStack<'a, Eth>;

/// A specialization of the `MatterStack` for Ethernet.
impl<'a> MatterStack<'a, Eth> {
    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for Ethernet network.
    pub const fn root_metadata() -> Endpoint<'static> {
        Endpoint {
            id: 0,
            device_type: rs_matter::data_model::device_types::DEV_TYPE_ROOT_NODE,
            clusters: &CLUSTERS,
        }
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for Ethernet network.
    pub fn root_handler(&self) -> EthRootEndpointHandler<'_> {
        handler(
            self.matter(),
            HandlerCompat(EthNwCommCluster::new(*self.matter().borrow())),
            ethernet_nw_diagnostics::ID,
            HandlerCompat(EthNwDiagCluster::new(*self.matter().borrow())),
        )
    }

    /// Resets the Matter instance to the factory defaults putting it into a
    /// Commissionable mode.
    pub fn reset(&self) -> Result<(), Error> {
        // TODO: Reset fabrics and ACLs

        Ok(())
    }

    /// Run the Matter stack for Ethernet network.
    pub async fn run<'d, H, P, E>(
        &self,
        sysloop: EspSystemEventLoop,
        nvs: EspNvsPartition<P>,
        eth: E,
        dev_comm: CommissioningData,
        handler: H,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: NvsPartitionId,
        E: NetifAccess,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        self.run_with_netif(
            sysloop,
            nvs,
            eth,
            Some((dev_comm, DiscoveryCapabilities::new(true, false, false))),
            handler,
        )
        .await
    }
}

/// The type of the handler for the root (Endpoint 0) of the Matter Node
/// when configured for Ethernet network.
pub type EthRootEndpointHandler<'a> = RootEndpointHandler<
    'a,
    HandlerCompat<nw_commissioning::EthNwCommCluster>,
    HandlerCompat<ethernet_nw_diagnostics::EthNwDiagCluster>,
>;

const CLUSTERS: [Cluster<'static>; 10] = [
    descriptor::CLUSTER,
    cluster_basic_information::CLUSTER,
    general_commissioning::CLUSTER,
    nw_commissioning::ETH_CLUSTER,
    admin_commissioning::CLUSTER,
    noc::CLUSTER,
    access_control::CLUSTER,
    general_diagnostics::CLUSTER,
    ethernet_nw_diagnostics::CLUSTER,
    group_key_management::CLUSTER,
];
