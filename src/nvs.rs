use embassy_sync::blocking_mutex::raw::RawMutex;

use esp_idf_svc::nvs::{EspNvs, NvsPartitionId};
use esp_idf_svc::sys::EspError;

use log::info;

use rs_matter::Matter;

use crate::{error::Error, wifi::WifiContext};

pub enum Network<'a, const N: usize, M>
where
    M: RawMutex,
{
    None,
    Wifi(&'a WifiContext<N, M>),
}

impl<'a, const N: usize, M> Network<'a, N, M>
where
    M: RawMutex,
{
    const fn key(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Wifi(_) => Some("wifi"),
        }
    }
}

pub struct Psm<'a, T, const N: usize, M>
where
    T: NvsPartitionId,
    M: RawMutex,
{
    matter: &'a Matter<'a>,
    network: Network<'a, N, M>,
    nvs: EspNvs<T>,
    buf: &'a mut [u8],
}

impl<'a, T, const N: usize, M> Psm<'a, T, N, M>
where
    T: NvsPartitionId,
    M: RawMutex,
{
    #[inline(always)]
    pub fn new(
        matter: &'a Matter<'a>,
        network: Network<'a, N, M>,
        nvs: EspNvs<T>,
        buf: &'a mut [u8],
    ) -> Result<Self, Error> {
        Ok(Self {
            matter,
            network,
            nvs,
            buf,
        })
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        self.load().await?;

        loop {
            self.matter.wait_changed().await;
            self.store().await?;
        }
    }

    pub async fn reset(&mut self) -> Result<(), Error> {
        Self::remove_blob(&mut self.nvs, "acls").await?;
        Self::remove_blob(&mut self.nvs, "fabrics").await?;

        if let Some(nw_key) = self.network.key() {
            Self::remove_blob(&mut self.nvs, nw_key).await?;
        }

        // TODO: Reset the Matter state

        Ok(())
    }

    pub async fn load(&mut self) -> Result<(), Error> {
        if let Some(data) = Self::load_blob(&mut self.nvs, "acls", self.buf).await? {
            self.matter.load_acls(data)?;
        }

        if let Some(data) = Self::load_blob(&mut self.nvs, "fabrics", self.buf).await? {
            self.matter.load_fabrics(data)?;
        }

        if let Network::Wifi(wifi_comm) = self.network {
            if let Some(data) =
                Self::load_blob(&mut self.nvs, self.network.key().unwrap(), self.buf).await?
            {
                wifi_comm.load(data)?;
            }
        }

        Ok(())
    }

    pub async fn store(&mut self) -> Result<(), Error> {
        if self.matter.is_changed() {
            if let Some(data) = self.matter.store_acls(self.buf)? {
                Self::store_blob(&mut self.nvs, "acls", data).await?;
            }

            if let Some(data) = self.matter.store_fabrics(self.buf)? {
                Self::store_blob(&mut self.nvs, "fabrics", data).await?;
            }
        }

        if let Network::Wifi(wifi_comm) = self.network {
            if let Some(data) = wifi_comm.store(self.buf)? {
                Self::store_blob(&mut self.nvs, self.network.key().unwrap(), data).await?;
            }
        }

        Ok(())
    }

    async fn load_blob<'b>(
        nvs: &mut EspNvs<T>,
        key: &str,
        buf: &'b mut [u8],
    ) -> Result<Option<&'b [u8]>, EspError> {
        // TODO: Not really async

        let data = nvs.get_blob(key, buf)?;
        info!(
            "Blob {key}: loaded {:?} bytes {data:?}",
            data.map(|data| data.len())
        );

        Ok(data)
    }

    async fn store_blob(nvs: &mut EspNvs<T>, key: &str, data: &[u8]) -> Result<(), EspError> {
        // TODO: Not really async

        nvs.set_blob(key, data)?;

        info!("Blob {key}: stored {} bytes {data:?}", data.len());

        Ok(())
    }

    async fn remove_blob(nvs: &mut EspNvs<T>, key: &str) -> Result<(), EspError> {
        // TODO: Not really async

        nvs.remove(key)?;

        info!("Blob {key}: removed");

        Ok(())
    }
}
