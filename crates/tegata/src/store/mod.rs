//! 秘密の置き場所（Secret Manager、試験用のディレクトリ）

use std::collections::{BTreeMap, BTreeSet};

pub mod gcp;
pub mod local;

/// 秘密の入れ物の情報
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecretMeta {
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
}

/// 版
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub number: u64,
    pub enabled: bool,
}

#[async_trait::async_trait]
pub trait Store: Send + Sync {
    /// すべての秘密
    async fn list(&self) -> anyhow::Result<BTreeMap<String, SecretMeta>>;
    /// 秘密の入れ物（なければ None）
    async fn get(&self, name: &str) -> anyhow::Result<Option<SecretMeta>>;
    async fn create(&self, name: &str, labels: &BTreeMap<String, String>) -> anyhow::Result<()>;
    /// ラベルと注記を置き換える
    async fn update_meta(&self, name: &str, meta: &SecretMeta) -> anyhow::Result<()>;
    /// 新しい版を足し、その番号を返す
    async fn add_version(&self, name: &str, value: &str) -> anyhow::Result<u64>;
    /// 版の値（None は最新の有効な版）。秘密・版がない、無効、読めないなら None
    async fn access(&self, name: &str, version: Option<u64>) -> anyhow::Result<Option<String>>;
    /// 版（消したものを除く）
    async fn versions(&self, name: &str) -> anyhow::Result<Vec<Version>>;
    async fn disable(&self, name: &str, version: u64) -> anyhow::Result<()>;
    /// 読む権限のある相手
    async fn readers(&self, name: &str) -> anyhow::Result<BTreeSet<String>>;
    /// 読む権限のある相手を、これだけにする
    async fn set_readers(&self, name: &str, members: &BTreeSet<String>) -> anyhow::Result<()>;
}

/// 最新の有効な版の番号
pub async fn latest_enabled(store: &dyn Store, name: &str) -> anyhow::Result<Option<u64>> {
    Ok(store
        .versions(name)
        .await?
        .into_iter()
        .filter(|v| v.enabled)
        .map(|v| v.number)
        .max())
}
