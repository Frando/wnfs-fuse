use std::{path::Path, rc::Rc};

use chrono::Utc;
use futures::StreamExt;
use libipld::Cid;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use wnfs::private::{PrivateDirectory, PrivateForest, PrivateNode, RevisionRef};
use wnfs_namefilter::Namefilter;

use crate::SqliteBlockStore;
use wnfs_common::{BlockStore, Metadata};

pub struct Wnfs {
    store: SqliteBlockStore,
    // signing_key: SigningKey,
    name: String,
    forest: Rc<PrivateForest>,
    private_dir: Rc<PrivateDirectory>,
}

const PRIVATE_ROOT_PREFIX: &str = "private-root:";
// const KEYPAIR_PREFIX: &str = "keypair:";

#[derive(Debug, Serialize, Deserialize)]
struct PrivateRoot {
    forest_cid: Cid,
    revision_ref: RevisionRef,
}

impl Wnfs {
    pub async fn open_from_path(db_path: impl AsRef<Path>, name: String) -> anyhow::Result<Self> {
        let mut store = SqliteBlockStore::new(db_path)?;
        let private_root_alias = format!("{}{}", PRIVATE_ROOT_PREFIX, name);
        // let keypair_alias = format!("{}{}", KEYPAIR_PREFIX, name);
        // let signing_key = {
        //     match store.get_from_alias(&keypair_alias).await? {
        //         Some(bytes) => {
        //             let keypair = SigningKey::from_bytes(
        //                 &bytes
        //                     .try_into()
        //                     .map_err(|_| anyhow::anyhow!("Failed to parse keypair"))?,
        //             );
        //             keypair
        //         }
        //         None => {
        //             let mut rng = rand::rngs::OsRng;
        //             let keypair = SigningKey::generate(&mut rng);
        //             let buf = keypair.to_bytes();
        //             store
        //                 .put_with_alias(&keypair_alias, buf.into(), IpldCodec::Raw)
        //                 .await?;
        //             keypair
        //         }
        //     }
        // };
        let private_root: PrivateRoot = {
            match store
                .get_deserializable_from_alias::<PrivateRoot>(&private_root_alias)
                .await
            {
                Err(_err) => {
                    let mut rng = rand::rngs::OsRng;
                    let root = create_private_dir(&mut store, &mut rng).await?;
                    store
                        .put_serializable_with_alias(&private_root_alias, &root)
                        .await?;
                    tracing::debug!("created private root");
                    root
                }
                Ok(root) => {
                    tracing::debug!("loaded private root");
                    root
                }
            }
        };
        tracing::debug!("load private root: {private_root:?}");
        let private_forest = store
            .get_deserializable::<PrivateForest>(&private_root.forest_cid)
            .await?;
        let node = private_forest
            .get_multivalue(&private_root.revision_ref, &store)
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("Failed to load private forest: {private_forest:?}"))??;
        let private_dir = node
            .search_latest(&private_forest, &store)
            .await?
            .as_dir()?;

        Ok(Self {
            private_dir,
            forest: Rc::new(private_forest),
            // signing_key,
            name,
            store,
        })
    }

    pub async fn flush(&mut self) -> anyhow::Result<()> {
        let mut rng = rand::rngs::OsRng;
        // let forest = self.private_forest.clone();
        let private_ref = self
            .private_dir
            .store(&mut self.forest, &mut self.store, &mut rng)
            .await
            .unwrap();

        // Persist encoded private forest to the block store.
        let forest_cid = self
            .store
            .put_async_serializable(&self.forest)
            .await
            .unwrap();
        let root = PrivateRoot {
            revision_ref: private_ref.as_revision_ref(),
            forest_cid,
        };
        tracing::debug!("persist private root: {root:?}");
        let private_root_alias = format!("{}{}", PRIVATE_ROOT_PREFIX, self.name);
        let _cid = self
            .store
            .put_serializable_with_alias(&private_root_alias, &root)
            .await?;
        Ok(())
    }

    pub async fn mkdir(&mut self, path_segments: &[String]) -> anyhow::Result<()> {
        let mut rng = rand::rngs::OsRng;
        self.private_dir
            .mkdir(
                path_segments,
                true,
                Utc::now(),
                &self.forest,
                &self.store,
                &mut rng,
            )
            .await?;
        self.flush().await?;
        Ok(())
    }

    pub async fn write_file(
        &mut self,
        path_segments: &[String],
        content: Vec<u8>,
    ) -> anyhow::Result<()> {
        let mut rng = rand::rngs::OsRng;
        self.private_dir
            .write(
                path_segments,
                true,
                Utc::now(),
                content,
                &mut self.forest,
                &mut self.store,
                &mut rng,
            )
            .await?;
        self.flush().await?;
        Ok(())
    }

    pub async fn read_file(&self, path_segments: &[String]) -> anyhow::Result<Vec<u8>> {
        self.private_dir
            .read(path_segments, true, &self.forest, &self.store)
            .await
    }

    pub async fn read_file_chunk(
        &self,
        path_segments: &[String],
        offset: usize,
        size: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let node = self.get_node(&path_segments).await?;
        match node {
            None => Err(anyhow::anyhow!("Not found")),
            Some(PrivateNode::Dir(_)) => Err(anyhow::anyhow!("Is a directory, not a file")),
            Some(PrivateNode::File(file)) => {
                file.read_chunk(offset, size, &self.forest, &self.store)
                    .await
            }
        }
    }

    pub async fn ls(&self, path_segments: &[String]) -> anyhow::Result<Vec<(String, Metadata)>> {
        self.private_dir
            .ls(path_segments, false, &self.forest, &self.store)
            .await
    }

    pub fn private_root(&self) -> Rc<PrivateDirectory> {
        Rc::clone(&self.private_dir)
    }

    pub async fn get_node(&self, path_segments: &[String]) -> anyhow::Result<Option<PrivateNode>> {
        self.private_dir
            .get_node(path_segments, false, &self.forest, &self.store)
            .await
    }
}

async fn create_private_dir(
    store: &mut impl BlockStore,
    rng: &mut impl RngCore,
) -> anyhow::Result<PrivateRoot> {
    // Create the private forest (a HAMT), a map-like structure where file and directory ciphertexts are stored.
    let forest = &mut Rc::new(PrivateForest::new());

    // Create a new directory.
    let dir = &mut Rc::new(PrivateDirectory::new(
        Namefilter::default(),
        Utc::now(),
        rng,
    ));

    let private_ref = dir.store(forest, store, rng).await?;

    // Persist encoded private forest to the block store.
    let forest_cid = store.put_async_serializable(forest).await?;
    Ok(PrivateRoot {
        revision_ref: private_ref.as_revision_ref(),
        forest_cid,
    })
}
