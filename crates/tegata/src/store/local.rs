//! ディレクトリに置く（試験と手元。1 つの秘密を 1 つの JSON のファイルにし、0600 で書く）

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::{SecretMeta, Store, Version};

pub struct LocalStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

#[derive(Default, Serialize, Deserialize)]
struct Secret {
    meta: SecretMeta,
    readers: BTreeSet<String>,
    versions: Vec<StoredVersion>,
}

#[derive(Serialize, Deserialize)]
struct StoredVersion {
    number: u64,
    enabled: bool,
    value: String,
    /// Versions written before tegata recorded the time have none
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<chrono::DateTime<chrono::Utc>>,
}

impl LocalStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.json"))
    }

    fn load(&self, name: &str) -> anyhow::Result<Option<Secret>> {
        match std::fs::read(self.path(name)) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, name: &str, secret: &Secret) -> anyhow::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.path(name).with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&tmp)?
            .write_all(&serde_json::to_vec_pretty(secret)?)?;
        std::fs::rename(tmp, self.path(name))?;
        Ok(())
    }

    fn must_load(&self, name: &str) -> anyhow::Result<Secret> {
        self.load(name)?
            .ok_or_else(|| anyhow::anyhow!("secret {name} does not exist"))
    }
}

#[async_trait::async_trait]
impl Store for LocalStore {
    async fn list(&self) -> anyhow::Result<BTreeMap<String, SecretMeta>> {
        let mut all = BTreeMap::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Ok(all);
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_owned();
                if let Some(s) = self.load(&name)? {
                    all.insert(name, s.meta);
                }
            }
        }
        Ok(all)
    }

    async fn get(&self, name: &str) -> anyhow::Result<Option<SecretMeta>> {
        Ok(self.load(name)?.map(|s| s.meta))
    }

    async fn create(&self, name: &str, labels: &BTreeMap<String, String>) -> anyhow::Result<()> {
        let _g = self.lock.lock().await;
        anyhow::ensure!(self.load(name)?.is_none(), "secret {name} already exists");
        self.save(
            name,
            &Secret {
                meta: SecretMeta {
                    labels: labels.clone(),
                    annotations: BTreeMap::new(),
                },
                ..Default::default()
            },
        )
    }

    async fn update_meta(&self, name: &str, meta: &SecretMeta) -> anyhow::Result<()> {
        let _g = self.lock.lock().await;
        let mut s = self.must_load(name)?;
        s.meta = meta.clone();
        self.save(name, &s)
    }

    async fn add_version(&self, name: &str, value: &str) -> anyhow::Result<u64> {
        let _g = self.lock.lock().await;
        let mut s = self.must_load(name)?;
        let number = s.versions.iter().map(|v| v.number).max().unwrap_or(0) + 1;
        s.versions.push(StoredVersion {
            number,
            enabled: true,
            value: value.to_owned(),
            created: Some(crate::age::now()),
        });
        self.save(name, &s)?;
        Ok(number)
    }

    async fn access(&self, name: &str, version: Option<u64>) -> anyhow::Result<Option<String>> {
        let Some(s) = self.load(name)? else {
            return Ok(None);
        };
        let v = match version {
            Some(n) => s.versions.iter().find(|v| v.number == n),
            None => s
                .versions
                .iter()
                .filter(|v| v.enabled)
                .max_by_key(|v| v.number),
        };
        Ok(v.filter(|v| v.enabled).map(|v| v.value.clone()))
    }

    async fn versions(&self, name: &str) -> anyhow::Result<Vec<Version>> {
        Ok(self
            .load(name)?
            .map(|s| {
                s.versions
                    .iter()
                    .map(|v| Version {
                        number: v.number,
                        enabled: v.enabled,
                        created: v.created,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn disable(&self, name: &str, version: u64) -> anyhow::Result<()> {
        let _g = self.lock.lock().await;
        let mut s = self.must_load(name)?;
        for v in s.versions.iter_mut().filter(|v| v.number == version) {
            v.enabled = false;
        }
        self.save(name, &s)
    }

    async fn readers(&self, name: &str) -> anyhow::Result<BTreeSet<String>> {
        Ok(self.load(name)?.map(|s| s.readers).unwrap_or_default())
    }

    async fn set_readers(&self, name: &str, members: &BTreeSet<String>) -> anyhow::Result<()> {
        let _g = self.lock.lock().await;
        let mut s = self.must_load(name)?;
        s.readers = members.clone();
        self.save(name, &s)
    }
}
