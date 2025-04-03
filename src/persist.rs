use core::fmt::Write;

use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsPartitionId};
use esp_idf_svc::sys::EspError;

use log::info;

use rs_matter::error::Error;

use rs_matter_stack::persist::KvBlobStore;

use crate::error::to_persist_error;

type SKey = heapless::String<5>;

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

    fn load<F>(&self, key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(Option<&[u8]>) -> Result<(), Error>,
    {
        // TODO: Not really async

        let mut skey = SKey::new();

        let data = self
            .0
            .get_blob(Self::skey(&mut skey, key), buf)
            .map_err(to_persist_error)?;

        info!(
            "Blob {key}: loaded {:?} bytes {data:?}",
            data.map(|data| data.len())
        );

        cb(data)
    }

    fn store<F>(&mut self, key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, Error>,
    {
        // TODO: Not really async

        let len = cb(buf)?;
        let data = &buf[..len];

        let mut skey = SKey::new();

        self.0
            .set_blob(Self::skey(&mut skey, key), data)
            .map_err(to_persist_error)?;

        info!("Blob {key}: stored {} bytes {data:?}", data.len());

        Ok(())
    }

    fn remove(&mut self, key: u16, _buf: &mut [u8]) -> Result<(), Error> {
        // TODO: Not really async

        let mut skey = SKey::new();

        self.0
            .remove(Self::skey(&mut skey, key))
            .map_err(to_persist_error)?;

        info!("Blob {key}: removed");

        Ok(())
    }

    fn skey(skey: &mut SKey, key: u16) -> &str {
        skey.clear();
        write!(skey, "{:04x}", key).unwrap();

        skey.as_str()
    }
}

impl<T> KvBlobStore for EspKvBlobStore<T>
where
    T: NvsPartitionId,
{
    async fn load<F>(&mut self, key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(Option<&[u8]>) -> Result<(), Error>,
    {
        EspKvBlobStore::load(self, key, buf, cb)
    }

    async fn store<F>(&mut self, key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, Error>,
    {
        EspKvBlobStore::store(self, key, buf, cb)
    }

    async fn remove(&mut self, key: u16, buf: &mut [u8]) -> Result<(), Error> {
        EspKvBlobStore::remove(self, key, buf)
    }
}
