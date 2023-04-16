use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ipfs_sqlite_block_store::{BlockStore as DbBlockStore, Config};
use libipld::cid::Version;
use libipld::store::StoreParams;
use libipld::{Block, Cid, IpldCodec};
use multihash::Code;
use multihash::MultihashDigest;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::Mutex;
use wnfs_common::BlockStore;

/// Default store parameters.
#[derive(Clone, Debug, Default)]
pub struct DefaultParams;

impl StoreParams for DefaultParams {
    const MAX_BLOCK_SIZE: usize = usize::MAX;
    type Codecs = libipld::IpldCodec;
    type Hashes = libipld::multihash::Code;
}

#[derive(Clone)]
pub struct SqliteBlockStore(pub Arc<Mutex<DbBlockStore<DefaultParams>>>);

impl SqliteBlockStore {
    pub fn new(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let store = DbBlockStore::<DefaultParams>::open(path, Config::default())?;
        Ok(Self(Arc::new(Mutex::new(store))))
    }

    pub async fn put_with_alias(
        &mut self,
        name: &str,
        blob: Vec<u8>,
        codec: IpldCodec,
    ) -> anyhow::Result<Cid> {
        let cid = self.put_block(blob, codec).await?;
        let mut store = self.0.lock().await;
        store.alias(name.as_bytes(), Some(&cid))?;
        Ok(cid)
    }

    pub async fn put_serializable_with_alias<V: Serialize>(
        &mut self,
        name: &str,
        value: &V,
    ) -> anyhow::Result<Cid> {
        let bytes = serde_ipld_dagcbor::to_vec(value)?;
        let cid = self.put_block(bytes, IpldCodec::DagCbor).await?;
        self.0.lock().await.alias(name.as_bytes(), Some(&cid))?;
        Ok(cid)
    }

    pub async fn get_from_alias<'b>(&self, name: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let mut store = self.0.lock().await;
        match store.resolve(name.as_bytes())? {
            None => Ok(None),
            Some(cid) => store.get_block(&cid).map_err(|err| err.into()),
        }
    }

    pub async fn get_deserializable_from_alias<V: DeserializeOwned>(
        &self,
        name: &str,
    ) -> anyhow::Result<V> {
        let cid = self
            .resolve_alias(&name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Not found"))?;
        self.get_deserializable(&cid).await
    }

    pub async fn resolve_alias<'b>(&self, name: &str) -> anyhow::Result<Option<Cid>> {
        let mut store = self.0.lock().await;
        let maybe_cid = store.resolve(name.as_bytes())?;
        Ok(maybe_cid)
    }
}

#[async_trait(?Send)]
impl wnfs_common::BlockStore for SqliteBlockStore {
    async fn get_block<'a>(&'a self, cid: &Cid) -> anyhow::Result<Cow<'a, Vec<u8>>> {
        let mut store = self.0.lock().await;
        let block = store
            .get_block(&cid)?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        Ok(Cow::Owned(block))
    }

    async fn put_block(&mut self, bytes: Vec<u8>, codec: IpldCodec) -> anyhow::Result<Cid> {
        let hash = Code::Blake3_256.digest(&bytes);
        let cid = Cid::new(Version::V1, codec.into(), hash)?;
        let block = Block::new(cid, bytes)?;
        let blocks = vec![block];
        let mut store = self.0.lock().await;
        store.put_blocks(blocks, None)?;
        Ok(cid)
    }
}
