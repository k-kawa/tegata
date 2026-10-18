//! tegata：秘密を作り、置き、雛形を埋める道具

pub mod config;
pub mod engine;
pub mod provider;
pub mod render;
pub mod store;
pub mod template;

use std::path::Path;

use config::Loaded;
use store::Store;

/// 設定の置き場所を開く
pub async fn open_store(loaded: &Loaded) -> anyhow::Result<Box<dyn Store>> {
    let store = &loaded.config.store;
    Ok(match (&store.gcp, &store.local) {
        (Some(g), _) => Box::new(
            store::gcp::GcpStore::new(Some(g.project.clone()), Some(g.location.clone())).await?,
        ),
        (None, Some(l)) => Box::new(store::local::LocalStore::new(loaded.path(&l.dir))),
        (None, None) => anyhow::bail!("store: set gcp or local"),
    })
}

/// render の置き場所：--store-dir があればそのディレクトリ、なければ Secret Manager
pub async fn open_render_store(
    project: Option<String>,
    store_dir: Option<&Path>,
) -> anyhow::Result<Box<dyn Store>> {
    Ok(match store_dir {
        Some(dir) => Box::new(store::local::LocalStore::new(dir.to_path_buf())),
        None => Box::new(store::gcp::GcpStore::new(project, None).await?),
    })
}
