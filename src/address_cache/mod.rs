pub mod kv_database;
use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
    ops::RangeInclusive,
    str::Split,
    vec,
};

use crate::{
    blockchain::{chainstore::ChainStore, sync::BlockchainSync},
    electrum::electrum_protocol::get_spk_hash,
};
use bitcoin::{
    consensus::deserialize,
    consensus::encode::serialize_hex,
    hash_types::Txid,
    hashes::{
        hex::{FromHex, ToHex},
        sha256::{self, Hash},
        Hash as HashTrait,
    },
    Block, MerkleBlock, Script, Transaction, TxOut,
};
use rustreexo::accumulator::{proof::Proof, stump::Stump};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedTransaction {
    pub tx_hex: String,
    pub height: u32,
    pub merkle_block: Option<MerkleBlock>,
    pub hash: String,
    pub position: u32,
}
impl Default for CachedTransaction {
    fn default() -> Self {
        CachedTransaction {
            tx_hex: sha256::Hash::all_zeros().to_string(),
            height: 0,
            merkle_block: None,
            hash: sha256::Hash::all_zeros().to_string(),
            position: 0,
        }
    }
}
impl std::fmt::Display for CachedTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let merkle_block = if let Some(merkle_block) = &self.merkle_block {
            serialize_hex(merkle_block)
        } else {
            "".to_string()
        };
        write!(
            f,
            "{};{};{};{}",
            self.tx_hex, self.height, self.position, merkle_block
        )
    }
}
/// TODO: Clean this function up
fn get_arg(mut split: Split<char>) -> Result<(&'_ str, Split<char>), crate::error::Error> {
    if let Some(data) = split.next() {
        return Ok((data, split));
    }
    Err(crate::error::Error::DbParseError)
}
impl TryFrom<String> for CachedTransaction {
    type Error = crate::error::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let transaction = value.split(';');

        let (tx_hex, transaction) = get_arg(transaction)?;

        let (height, transaction) = get_arg(transaction)?;
        let (position, transaction) = get_arg(transaction)?;

        let (merkle_block, _) = get_arg(transaction)?;
        let merkle_block = Vec::from_hex(merkle_block)?;
        let merkle_block = deserialize(&merkle_block)?;

        let tx = Vec::from_hex(tx_hex)?;
        let tx = deserialize::<Transaction>(&tx)?;

        Ok(CachedTransaction {
            tx_hex: tx_hex.to_string(),
            height: height.parse::<u32>()?,
            merkle_block: Some(merkle_block),
            hash: tx.txid().to_string(),
            position: position.parse::<u32>()?,
        })
    }
}
impl TryFrom<String> for CachedAddress {
    type Error = crate::error::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let address = value.split(':');
        let (script_hash, address) = get_arg(address)?;
        let script_hash = sha256::Hash::from_hex(script_hash)?;

        let (balance, address) = get_arg(address)?;

        let (script, address) = get_arg(address)?;
        let script = Script::from_hex(script)?;

        let mut transactions = vec![];

        for transaction in address {
            if transaction.is_empty() {
                continue;
            }

            let transaction = transaction.to_string();
            let transaction = CachedTransaction::try_from(transaction)?;

            transactions.push(transaction);
        }

        Ok(CachedAddress {
            balance: balance.parse()?,
            script_hash,
            transactions,
            script,
        })
    }
}
#[derive(Debug, Clone)]
pub struct CachedAddress {
    script_hash: Hash,
    balance: u64,
    transactions: Vec<CachedTransaction>,
    script: Script,
}

impl CachedAddress {
    pub fn _new(
        script_hash: Hash,
        balance: u64,
        transactions: Vec<CachedTransaction>,
        script: Script,
    ) -> CachedAddress {
        CachedAddress {
            script_hash,
            balance,
            transactions,
            script,
        }
    }
}
pub trait AddressCacheDatabase {
    /// Saves a new address to the database. If the address already exists, `update` should
    /// be used instead
    fn save(&self, address: &CachedAddress);
    /// Loads all addresses we have cached so far
    fn load<E>(&self) -> Result<Vec<CachedAddress>, E>
    where
        E: From<crate::error::Error> + Into<crate::error::Error> + std::convert::From<kv::Error>;
    /// Updates an address, probably because a new transaction arrived
    fn update(&self, address: &CachedAddress);
    /// TODO: Maybe turn this into another db
    /// Returns the height of the last block we filtered
    fn get_cache_height(&self) -> Result<u32, crate::error::Error>;
    /// Saves the height of the last block we filtered
    fn set_cache_height(&self, height: u32) -> Result<(), crate::error::Error>;
    /// Saves the descriptor of associated cache
    fn desc_save(&self, descriptor: String) -> Result<(), crate::error::Error>;
    /// Get associated descriptor
    fn desc_get(&self) -> Result<String, crate::error::Error>;
}
/// Holds all addresses and associated transactions. We need a database with some basic
/// methods, to store all data
pub struct AddressCache<D: AddressCacheDatabase, S: ChainStore> {
    /// A database that will be used to persist all needed to get our address history
    database: D,
    /// Maps a hash to a cached address struct, this is basically an in-memory version
    /// of our database, used for speeding up processing a block. This hash is the electrum's
    /// script hash.
    address_map: HashMap<Hash, CachedAddress>,
    /// Holds all scripts we are interested in.
    script_set: HashSet<Script>,
    /// Maps transaction ids to a script hash and the position of this transaction in a block
    tx_index: HashMap<Txid, (Hash, usize)>,
    /// Our utreexo accumulator
    acc: Stump,
    /// Since address_cache hold an acc and might need some other blockchain related data
    /// it's nice to give it a chainstore.
    chain_store: S,
}
impl<D: AddressCacheDatabase, S: ChainStore> AddressCache<D, S> {
    /// Iterates through a block, finds transactions destined to ourselves.
    /// Returns all transactions we found.
    pub fn block_process(
        &mut self,
        block: &Block,
        height: u32,
        proof: Proof,
        del_hashes: Vec<sha256::Hash>,
    ) -> Vec<(Transaction, TxOut)> {
        let mut my_transactions = vec![];
        self.acc = BlockchainSync::update_acc(&self.acc, block, height, proof, del_hashes)
            .unwrap_or_else(|_| panic!("Could not update the accumulator at {height}"));

        for (position, transaction) in block.txdata.iter().enumerate() {
            for output in transaction.output.iter() {
                if self.script_set.contains(&output.script_pubkey) {
                    my_transactions.push((transaction.clone(), output.clone()));
                    let my_txid = transaction.txid();
                    let merkle_block =
                        MerkleBlock::from_block_with_predicate(block, |txid| *txid == my_txid);
                    self.cache_transaction(
                        transaction,
                        height,
                        output,
                        merkle_block,
                        position as u32,
                    );
                }
            }
        }
        my_transactions
    }
    pub fn save_acc(&self) {
        let mut acc = String::new();
        acc.write_fmt(format_args!("{} ", self.acc.leafs))
            .expect("String formatting should not err");
        for root in self.acc.roots.iter() {
            acc.write_fmt(format_args!("{root}"))
                .expect("String formatting should not err");
        }

        self.chain_store
            .save_roots(acc)
            .expect("Chain store is not working");
    }

    fn load_acc(chain_store: &S) -> Stump {
        let acc = chain_store.load_roots().expect("Could not load roots");
        if let Some(acc) = acc {
            let acc = acc.split(' ').collect::<Vec<_>>();
            let leaves = acc.first().expect("Missing leaves count");

            let leaves = leaves
                .parse::<u64>()
                .expect("Invalid number, maybe the accumulator got corrupted?");
            let acc = acc.get(1);
            let mut roots = vec![];

            if let Some(acc) = acc {
                let mut acc = acc.to_string();
                while acc.len() >= 64 {
                    let hash = acc.drain(0..64).collect::<String>();
                    let hash =
                        sha256::Hash::from_hex(hash.as_str()).expect("Invalid hash provided");
                    roots.push(hash);
                }
            }

            Stump {
                leafs: leaves,
                roots,
            }
        } else {
            Stump::new()
        }
    }
    pub fn bump_height(&self, height: u32) {
        self.database
            .set_cache_height(height)
            .expect("Database is not working");
    }
    pub fn new(database: D, chain_store: S) -> AddressCache<D, S> {
        let scripts = database
            .load::<crate::error::Error>()
            .expect("Could not load database");

        let mut address_map = HashMap::new();
        let mut script_set = HashSet::new();
        let mut tx_index = HashMap::new();
        for address in scripts {
            for (pos, tx) in address.transactions.iter().enumerate() {
                let txid = Txid::from_hex(&tx.hash).expect("Cached an invalid txid");
                tx_index.insert(txid, (address.script_hash, pos));
            }
            script_set.insert(address.script.clone());
            address_map.insert(address.script_hash, address);
        }

        let acc = AddressCache::<D, S>::load_acc(&chain_store);
        AddressCache {
            database,
            chain_store,
            address_map,
            script_set,
            tx_index,
            acc,
        }
    }
    fn get_transaction(&self, txid: &Txid) -> Option<CachedTransaction> {
        if let Some((address, idx)) = self.tx_index.get(txid) {
            if let Some(address) = self.address_map.get(address) {
                if let Some(tx) = address.transactions.get(*idx) {
                    return Some(tx.clone());
                }
            }
        }
        None
    }
    /// Returns all transactions this address has, both input and outputs
    pub fn get_address_history(&self, script_hash: &sha256::Hash) -> Vec<CachedTransaction> {
        if let Some(cached_script) = self.address_map.get(script_hash) {
            return cached_script.transactions.clone();
        }
        vec![]
    }
    /// Returns the balance of this address, debts (spends) are taken in account
    pub fn get_address_balance(&self, script_hash: &sha256::Hash) -> u64 {
        if let Some(cached_script) = self.address_map.get(script_hash) {
            return cached_script.balance;
        }

        0
    }
    /// Returns the Merkle Proof for a given address
    pub fn get_merkle_proof(&self, txid: &Txid) -> Option<(Vec<String>, u32)> {
        let mut hashes = vec![];
        if let Some(tx) = self.get_transaction(txid) {
            for hash in tx.merkle_block.unwrap().txn.hashes() {
                // Rust Bitcoin (and Bitcoin Core) includes the target hash, but Electrum
                // doesn't like this.
                if hash.as_hash() != txid.as_hash() {
                    hashes.push(hash.to_hex());
                }
            }

            return Some((hashes, tx.position));
        }

        None
    }
    pub fn get_height(&self, txid: &Txid) -> Option<u32> {
        if let Some(tx) = self.get_transaction(txid) {
            return Some(tx.height);
        }

        None
    }
    pub fn get_sync_limits(
        &self,
        current_hight: u32,
    ) -> Result<RangeInclusive<u32>, crate::error::Error> {
        let height = self.database.get_cache_height()?;
        Ok((height + 1)..=current_hight)
    }
    pub fn get_cached_transaction(&self, txid: &Txid) -> Option<String> {
        if let Some(tx) = self.get_transaction(txid) {
            return Some(tx.tx_hex);
        }
        None
    }
    pub fn cache_address(&mut self, script_pk: Script) {
        let hash = get_spk_hash(&script_pk);
        let new_address = CachedAddress {
            balance: 0,
            script_hash: hash,
            transactions: vec![],
            script: script_pk.clone(),
        };
        self.database.save(&new_address);

        self.address_map.insert(hash, new_address);
        self.script_set.insert(script_pk);
    }
    /// Setup is the first command that should be executed. In a new cache. It sets our wallet's
    /// state, like the height we should start scanning and the wallet's descriptor.
    pub fn setup(&self, descriptor: String) -> Result<(), crate::error::Error> {
        self.database.set_cache_height(0)?;
        self.database.desc_save(descriptor)
    }
    /// Caches a new transaction. This method may be called for addresses we don't follow yet,
    /// this automatically makes we follow this address.
    pub fn cache_transaction(
        &mut self,
        transaction: &Transaction,
        height: u32,
        out: &TxOut,
        merkle_block: MerkleBlock,
        position: u32,
    ) {
        let transaction_to_cache = CachedTransaction {
            height,
            merkle_block: Some(merkle_block),
            tx_hex: serialize_hex(transaction),
            hash: transaction.txid().to_string(),
            position,
        };
        let hash = get_spk_hash(&out.script_pubkey);
        if let Some(address) = self.address_map.get_mut(&hash) {
            if address.transactions.contains(&transaction_to_cache) {
                return;
            }
            self.tx_index.insert(
                transaction.txid(),
                (address.script_hash, address.transactions.len()),
            );
            address.transactions.push(transaction_to_cache);
            self.database.update(address);
        } else {
            // This means `cache_transaction` have been called with an address we don't
            // follow. This may be useful for caching new addresses without re-scanning.
            // We can track this address from now onwards, but the past history is only
            // available with full rescan
            let new_address = CachedAddress {
                balance: 0,
                script_hash: hash,
                transactions: vec![transaction_to_cache],
                script: out.script_pubkey.clone(),
            };
            self.database.save(&new_address);

            self.address_map.insert(hash, new_address);
            self.script_set.insert(out.script_pubkey.clone());
        }
    }
}

#[cfg(test)]
mod test {
    use super::{kv_database::KvDatabase, AddressCache};
    use crate::{blockchain::chainstore::KvChainStore, electrum::electrum_protocol::get_spk_hash};
    use bitcoin::{hashes::hex::FromHex, Script};

    #[test]
    fn test_create_cache() {
        // None of this should fail
        let database = KvDatabase::new("/tmp/utreexo/".into()).unwrap();
        let chain_store = KvChainStore::new("/tmp/utreexo/".to_owned()).unwrap();
        let _ = AddressCache::new(database, chain_store);
    }
    #[test]
    fn cache_address() {
        let database = KvDatabase::new("/tmp/utreexo/".into()).unwrap();
        let chain_store = KvChainStore::new("/tmp/utreexo/".to_owned()).unwrap();

        let mut cache = AddressCache::new(database, chain_store);
        let script_pk = Script::from_hex("00").unwrap();
        let hash = &get_spk_hash(&script_pk);

        cache.cache_address(script_pk);
        assert_eq!(cache.address_map.len(), 1);
        assert_eq!(cache.get_address_balance(hash), 0);
    }
    #[test]
    fn test_persistency() {
        {
            let database = KvDatabase::new("/tmp/utreexo/".into()).unwrap();
            let chain_store = KvChainStore::new("/tmp/utreexo/".to_owned()).unwrap();

            let mut cache = AddressCache::new(database, chain_store);
            let script_pk = Script::from_hex("4104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac").unwrap();
            cache.cache_address(script_pk);
        }
        let database = KvDatabase::new("/tmp/utreexo/".into()).unwrap();
        let chain_store = KvChainStore::new("/tmp/utreexo/".to_owned()).unwrap();

        let cache = AddressCache::new(database, chain_store);
        assert_eq!(cache.script_set.len(), 1);
    }
}
