use esp_idf_svc::{nvs::{EspNvs, NvsPartitionId}, sys::EspError};

use log::info;

use rs_matter::Matter;

pub struct Psm<'a, T> 
where 
    T: NvsPartitionId,
{
    matter: &'a Matter<'a>,
    nvs: EspNvs<T>,
}

impl<'a, T> Psm<'a, T> 
where 
    T: NvsPartitionId,
{
    #[inline(always)]
    pub fn new(matter: &'a Matter<'a>, nvs: EspNvs<T>, buf: &mut [u8]) -> Result<Self, EspError> {
        Ok(Self {
            matter,
            nvs,
        })
    }

    pub async fn run(&mut self) -> Result<(), EspError> {
        loop {
            self.matter.wait_changed().await;

            if self.matter.is_changed() {
                if let Some(data) = self.matter.store_acls(&mut self.buf)? {
                    Self::store(&self.dir, "acls", data)?;
                }

                if let Some(data) = self.matter.store_fabrics(&mut self.buf)? {
                    Self::store(&self.dir, "fabrics", data)?;
                }
            }
        }
    }

    fn load<'b>(dir: &Path, key: &str, buf: &'b mut [u8]) -> Result<Option<&'b [u8]>, EspError> {
        let path = dir.join(key);

        match fs::File::open(path) {
            Ok(mut file) => {
                let mut offset = 0;

                loop {
                    if offset == buf.len() {
                        Err(ErrorCode::NoSpace)?;
                    }

                    let len = file.read(&mut buf[offset..])?;

                    if len == 0 {
                        break;
                    }

                    offset += len;
                }

                let data = &buf[..offset];

                info!("Key {}: loaded {} bytes {:?}", key, data.len(), data);

                Ok(Some(data))
            }
            Err(_) => Ok(None),
        }
    }

    fn store(dir: &Path, key: &str, data: &[u8]) -> Result<(), EspError> {
        let path = dir.join(key);

        let mut file = fs::File::create(path)?;

        file.write_all(data)?;

        info!("Key {}: stored {} bytes {:?}", key, data.len(), data);

        Ok(())
    }
}
