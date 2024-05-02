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

pub struct Eth(());

impl Network for Eth {
    const INIT: Self = Self(());
}

impl<'a> MatterStack<'a, Eth> {
    pub const fn root_metadata() -> Endpoint<'static> {
        Endpoint {
            id: 0,
            device_type: rs_matter::data_model::device_types::DEV_TYPE_ROOT_NODE,
            clusters: &CLUSTERS,
        }
    }

    pub fn root_handler(&self) -> EthRootEndpointHandler<'_> {
        handler(
            0,
            self.matter(),
            HandlerCompat(EthNwCommCluster::new(*self.matter().borrow())),
            ethernet_nw_diagnostics::ID,
            HandlerCompat(EthNwDiagCluster::new(*self.matter().borrow())),
        )
    }

    pub async fn run<'d, T, P, E>(
        &self,
        sysloop: EspSystemEventLoop,
        nvs: EspNvsPartition<P>,
        eth: E,
        dev_comm: CommissioningData,
        handler: T,
    ) -> Result<(), Error>
    where
        T: AsyncHandler + AsyncMetadata,
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
