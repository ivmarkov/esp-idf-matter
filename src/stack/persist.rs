#![cfg(esp_idf_comp_nvs_flash_enabled)]

use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsPartitionId};
use esp_idf_svc::sys::EspError;

use log::info;

use rs_matter::error::{Error, ErrorCode};

use rs_matter_stack::persist::{KvBlobStore, KvPersist};

pub type EspPersist<'a, T, const N: usize, M> = KvPersist<'a, EspKvBlobStore<T>, N, M>;

/// A `KvBlobStore`` implementation that uses the ESP IDF NVS API
/// to store and load the BLOBs.
///
/// NOTE: Not async (yet)
pub struct EspKvBlobStore<T>(EspNvs<T>)
where
    T: NvsPartitionId;

impl<T> EspKvBlobStore<T>
where
    T: NvsPartitionId,
{
    /// Create a new PSM instance that would persist in namespace `esp-idf-matter`.
    pub fn new_default(nvs: EspNvsPartition<T>) -> Result<Self, EspError> {
        Self::new(nvs, "esp-idf-matter")
    }

    /// Create a new PSM instance.
    pub fn new(nvs: EspNvsPartition<T>, namespace: &str) -> Result<Self, EspError> {
        Ok(Self(EspNvs::new(nvs, namespace, true)?))
    }

    fn load_blob<'b>(&self, key: &str, buf: &'b mut [u8]) -> Result<Option<&'b [u8]>, EspError> {
        // TODO: Not really async

        let data = self.0.get_blob(key, buf)?;
        info!(
            "Blob {key}: loaded {:?} bytes {data:?}",
            data.map(|data| data.len())
        );

        Ok(data)
    }

    fn store_blob(&mut self, key: &str, data: &[u8]) -> Result<(), EspError> {
        // TODO: Not really async

        self.0.set_blob(key, data)?;

        info!("Blob {key}: stored {} bytes {data:?}", data.len());

        Ok(())
    }

    fn remove_blob(&mut self, key: &str) -> Result<(), EspError> {
        // TODO: Not really async

        self.0.remove(key)?;

        info!("Blob {key}: removed");

        Ok(())
    }
}

impl<T> KvBlobStore for EspKvBlobStore<T>
where
    T: NvsPartitionId,
{
    async fn load<'a>(&mut self, key: &str, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
        Ok(self
            .load_blob(key, buf)
            .map_err(|_| ErrorCode::StdIoError)?)
    }

    async fn store(&mut self, key: &str, value: &[u8]) -> Result<(), Error> {
        self.store_blob(key, value)
            .map_err(|_| ErrorCode::StdIoError)?;

        Ok(())
    }

    async fn remove(&mut self, key: &str) -> Result<(), Error> {
        self.remove_blob(key).map_err(|_| ErrorCode::StdIoError)?;

        Ok(())
    }
}
