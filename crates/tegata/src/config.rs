//! YAML の設定

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub store: StoreConfig,
    /// 読む役割（VM など）
    #[serde(default)]
    pub roles: BTreeMap<String, Role>,
    /// 外のプロバイダー（random と manual は書かなくてよい）
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    pub resources: BTreeMap<String, Resource>,
}

/// 置き場所（どちらか 1 つ）
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// Google Cloud の Secret Manager
    pub gcp: Option<GcpStoreConfig>,
    /// ディレクトリ（試験と手元）
    pub local: Option<LocalStoreConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GcpStoreConfig {
    pub project: String,
    /// 入れ物を置く場所（user-managed の複製）
    pub location: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalStoreConfig {
    /// この YAML からの相対
    pub dir: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    /// 読む権限を付ける相手（例：`serviceAccount:…`）
    pub member: String,
    /// この役割の雛形のディレクトリ。ここにある名前だけを読める
    pub templates: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// 実行ファイル（`/` を含めばこの YAML からの相対、なければ PATH と tegata の隣）
    pub command: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub credentials: BTreeMap<String, Credential>,
}

/// 鍵の取り方：環境変数、なければ端末で入力を隠して聞く。optional なら、聞かずに渡さない
/// （プロバイダーがほかの方法で認証する。例：Cloudflare は cf のログイン）
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub env: Option<String>,
    pub prompt: Option<String>,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub provider: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub config: serde_json::Value,
    /// 秘密の名前 → プロバイダーの出力の名前
    pub secrets: BTreeMap<String, String>,
    /// rotate のとき、前の値を残す秘密（最初の秘密の値）
    pub previous: Option<String>,
}

impl Resource {
    /// 世代の記録を置く秘密（名前の順で最初のもの）
    pub fn primary(&self) -> &str {
        self.secrets.keys().next().map(String::as_str).unwrap_or("")
    }

    /// このリソースが持つ秘密（previous も）
    pub fn all_secrets(&self) -> Vec<String> {
        let mut names: Vec<String> = self.secrets.keys().cloned().collect();
        names.extend(self.previous.clone());
        names
    }
}

/// 読み込んだ設定と、その置き場所
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub base_dir: PathBuf,
}

impl Loaded {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let config: Config = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("invalid {}", path.display()))?;
        let base_dir = path
            .parent()
            .map(Path::to_path_buf)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from("."));
        let loaded = Self { config, base_dir };
        loaded.validate()?;
        Ok(loaded)
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        let p = Path::new(relative);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base_dir.join(p)
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        let store = &self.config.store;
        anyhow::ensure!(
            store.gcp.is_some() != store.local.is_some(),
            "store: set exactly one of gcp and local"
        );
        let mut owner: BTreeMap<String, &str> = BTreeMap::new();
        for (name, res) in &self.config.resources {
            anyhow::ensure!(
                valid_label(name),
                "resource name {name:?}: use lowercase letters, digits, - and _ (up to 63)"
            );
            anyhow::ensure!(!res.secrets.is_empty(), "resource {name}: secrets is empty");
            anyhow::ensure!(
                matches!(res.provider.as_str(), "random" | "manual" | "value")
                    || self.config.providers.contains_key(&res.provider),
                "resource {name}: unknown provider {}",
                res.provider
            );
            for secret in res.all_secrets() {
                anyhow::ensure!(
                    valid_secret_name(&secret),
                    "resource {name}: invalid secret name {secret:?}"
                );
                if let Some(other) = owner.insert(secret.clone(), name) {
                    anyhow::bail!("secret {secret} is in both {other} and {name}");
                }
            }
        }
        Ok(())
    }

    /// 役割 → その雛形にある名前
    pub fn role_names(&self) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
        self.config
            .roles
            .iter()
            .map(|(role, r)| {
                Ok((
                    role.clone(),
                    crate::template::names_in_dir(&self.path(&r.templates))?,
                ))
            })
            .collect()
    }

    /// 秘密の名前 → リソースの名前
    pub fn owner_of(&self, secret: &str) -> Option<&str> {
        self.config
            .resources
            .iter()
            .find(|(_, r)| r.all_secrets().iter().any(|s| s == secret))
            .map(|(n, _)| n.as_str())
    }
}

/// Secret Manager のラベルの値に使える（英小文字・数字・- と _ の 63 文字まで）
pub fn valid_label(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// 秘密の名前（Secret Manager：英数字・- と _ の 255 文字まで）
pub fn valid_secret_name(s: &str) -> bool {
    (1..=255).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tegata.yaml");
        std::fs::write(
            &path,
            r#"
store: { local: { dir: store } }
resources:
  a: { provider: random, secrets: { A: value }, previous: A_PREV }
  b: { provider: manual, secrets: { B: value } }
"#,
        )
        .unwrap();
        let loaded = Loaded::load(&path).unwrap();
        assert_eq!(loaded.owner_of("A_PREV"), Some("a"));
        assert_eq!(loaded.config.resources["a"].primary(), "A");

        std::fs::write(
            &path,
            "store: { local: { dir: s } }\nresources:\n  a: { provider: random, secrets: { A: value } }\n  b: { provider: manual, secrets: { A: value } }\n",
        )
        .unwrap();
        assert!(format!("{:#}", Loaded::load(&path).unwrap_err()).contains("in both"));
        std::fs::write(
            &path,
            "store: { local: { dir: s } }\nresources:\n  a: { provider: nope, secrets: { A: value } }\n",
        )
        .unwrap();
        assert!(format!("{:#}", Loaded::load(&path).unwrap_err()).contains("unknown provider"));
    }
}
