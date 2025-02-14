use super::{AddressCacheDatabase, CachedAddress};
use bitcoin::hashes::hex::ToHex;
use kv::{Bucket, Config, Store};

pub struct KvDatabase(Store, Bucket<'static, String, String>);
impl KvDatabase {
    pub fn new(datadir: String) -> Result<KvDatabase, kv::Error> {
        // Configure the database
        let cfg = Config::new(datadir);

        // Open the key/value store
        let store = Store::new(cfg)?;
        let bucket = store.bucket::<String, String>(Some("addresses"))?;
        Ok(KvDatabase(store, bucket))
    }
}
impl AddressCacheDatabase for KvDatabase {
    fn load<E>(&self) -> Result<Vec<super::CachedAddress>, E>
    where
        E: From<crate::error::Error> + std::convert::From<kv::Error>,
    {
        let mut addresses = vec![];
        for item in self.1.iter() {
            let item = item?;
            let key = item.key::<String>()?;
            if *"height" == key || *"desc" == key {
                continue;
            }
            let value: String = item.value().unwrap();
            let value = CachedAddress::try_from(value)?;
            addresses.push(value);
        }
        Ok(addresses)
    }
    fn save(&self, address: &super::CachedAddress) {
        let key = address.script_hash.to_string();
        let mut transactions = String::new();
        for transaction in address.transactions.iter() {
            let tx = transaction.to_string() + ":";
            transactions.extend(tx.chars().into_iter());
        }
        let value = format!(
            "{}:{}:{}:{transactions}",
            address.script_hash,
            address.balance,
            address.script.to_hex(),
        );

        self.1
            .set(&key, &value)
            .expect("Fatal: Database isn't working");
        self.1.flush().expect("Could not write to disk");
    }
    fn update(&self, address: &super::CachedAddress) {
        self.save(address);
    }
    fn get_cache_height(&self) -> Result<u32, crate::error::Error> {
        self.0.bucket::<String, String>(Some("meta"))?;
        let height = self.1.get(&"height".to_string())?;
        if let Some(height) = height {
            return Ok(height.parse::<u32>()?);
        }
        Err(crate::error::Error::WalletNotInitialized)
    }
    fn set_cache_height(&self, height: u32) -> Result<(), crate::error::Error> {
        self.0.bucket::<String, String>(Some("meta"))?;
        self.1.set(&"height".to_string(), &height.to_string())?;
        self.1.flush()?;
        Ok(())
    }

    fn desc_save(&self, descriptor: String) -> Result<(), crate::error::Error> {
        self.0.bucket::<String, String>(Some("meta"))?;
        self.1.set(&"desc".to_string(), &descriptor)?;
        self.1.flush()?;

        Ok(())
    }

    fn desc_get(&self) -> Result<String, crate::error::Error> {
        self.0.bucket::<String, String>(Some("meta"))?;
        let res = self.1.get(&"desc".to_string())?;
        if let Some(res) = res {
            return Ok(res);
        }
        Err(crate::error::Error::WalletNotInitialized)
    }
}
