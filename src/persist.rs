use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsPartitionId};
use esp_idf_svc::sys::EspError;

use log::info;

use rs_matter::error::Error;

use rs_matter_stack::persist::{Key, KvBlobStore, KvPersist};

use crate::error::to_persist_error;

/// A type alias for a `KvPersist` instance that uses the ESP IDF NVS API
pub type EspMatterPersist<'a, T, C> = KvPersist<'a, EspKvBlobStore<T>, C>;

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
    /// Create a new KV BLOB store instance that would persist in namespace `esp-idf-matter`.
    pub fn new_default(nvs: EspNvsPartition<T>) -> Result<Self, EspError> {
        Self::new(nvs, "esp-idf-matter")
    }

    /// Create a new KV BLOB store instance.
    pub fn new(nvs: EspNvsPartition<T>, namespace: &str) -> Result<Self, EspError> {
        Ok(Self(EspNvs::new(nvs, namespace, true)?))
    }

    fn load<F>(&self, key: Key, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(Option<&[u8]>) -> Result<(), Error>,
    {
        // TODO: Not really async

        let data = self
            .0
            .get_blob(key.as_ref(), buf)
            .map_err(to_persist_error)?;

        info!(
            "Blob {key}: loaded {:?} bytes {data:?}",
            data.map(|data| data.len())
        );

        cb(data)
    }

    fn store<F>(&mut self, key: Key, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, Error>,
    {
        // TODO: Not really async

        let len = cb(buf)?;
        let data = &buf[..len];

        self.0
            .set_blob(key.as_ref(), data)
            .map_err(to_persist_error)?;

        info!("Blob {key}: stored {} bytes {data:?}", data.len());

        Ok(())
    }

    fn remove(&mut self, key: Key, _buf: &mut [u8]) -> Result<(), Error> {
        // TODO: Not really async

        self.0.remove(key.as_ref()).map_err(to_persist_error)?;

        info!("Blob {key}: removed");

        Ok(())
    }
}

impl<T> KvBlobStore for EspKvBlobStore<T>
where
    T: NvsPartitionId,
{
    async fn load<F>(&mut self, key: Key, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(Option<&[u8]>) -> Result<(), Error>,
    {
        EspKvBlobStore::load(self, key, buf, cb)
    }

    async fn store<F>(&mut self, key: Key, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, Error>,
    {
        EspKvBlobStore::store(self, key, buf, cb)
    }

    async fn remove(&mut self, key: Key, buf: &mut [u8]) -> Result<(), Error> {
        EspKvBlobStore::remove(self, key, buf)
    }
}
