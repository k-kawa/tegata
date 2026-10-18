//! YAML の設定
//!
//! The file is read in one of two forms, and both become the same [`Config`]:
//!
//! - the current form: `stores`, `identities`, and `resources.*.stores.<store>` entries with
//!   `readers`;
//! - the old form (before 0.2): `store`, `roles` (one `member` and `templates` each), and
//!   `resources.*.secrets`. Read access then comes from the templates, as it did.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// The configuration, in one shape whichever form the file was written in
#[derive(Debug, Clone)]
pub struct Config {
    /// The store's name in `stores` (`main` for the old form)
    pub store_name: String,
    pub store: StoreConfig,
    /// Who reads secrets: a name → IAM members
    pub identities: BTreeMap<String, Identity>,
    /// 外のプロバイダー（random と manual は書かなくてよい）
    pub providers: BTreeMap<String, ProviderConfig>,
    pub resources: BTreeMap<String, Resource>,
    /// Where copies of secrets are written (GitHub Actions secrets). The store stays the source
    pub destinations: BTreeMap<String, Destination>,
    /// Read access comes from the templates (the old form) instead of `readers`
    pub readers_from_templates: bool,
}

/// One or more IAM members that read secrets
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub members: Vec<String>,
    /// The old form only: the role's templates, which decide what it can read
    pub old_templates: Option<String>,
}

/// A place that receives copies of secrets. tegata writes there but never reads values back
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub github: GithubDestination,
    /// The secrets to deliver: a list (same names), or a map of name there → secret name here
    pub secrets: DeliveredSecrets,
}

/// GitHub Actions secrets of a repository, or of one of its environments
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct GithubDestination {
    /// `owner/repo`
    pub repo: String,
    /// An environment of the repository (its secrets instead of the repository's)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

impl std::fmt::Display for GithubDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.environment {
            Some(env) => write!(f, "github {} (environment {env})", self.repo),
            None => write!(f, "github {}", self.repo),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DeliveredSecrets {
    Same(Vec<String>),
    Renamed(BTreeMap<String, String>),
}

impl Destination {
    /// Name there → secret name here
    pub fn secrets(&self) -> BTreeMap<String, String> {
        match &self.secrets {
            DeliveredSecrets::Same(names) => names.iter().map(|n| (n.clone(), n.clone())).collect(),
            DeliveredSecrets::Renamed(map) => map.clone(),
        }
    }
}

/// A GitHub Actions secret name: letters, digits and `_`, not starting with a digit or `GITHUB_`
pub fn valid_github_secret_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !s.as_bytes()[0].is_ascii_digit()
        && !s.to_ascii_uppercase().starts_with("GITHUB_")
}

/// 置き場所（どちらか 1 つ）
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// Google Cloud の Secret Manager
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcp: Option<GcpStoreConfig>,
    /// ディレクトリ（試験と手元）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalStoreConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GcpStoreConfig {
    pub project: String,
    /// 入れ物を置く場所（user-managed の複製）
    pub location: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalStoreConfig {
    /// この YAML からの相対
    pub dir: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// The executable (relative to this YAML if it has a `/`, otherwise looked up in PATH and next
    /// to tegata). Set this or `source`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// A GitHub repository whose releases hold the provider (`github.com/<owner>/<repo>`),
    /// installed by `tegata init`. Set this or `command`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The version requirement for `source` (Cargo syntax: `0.3`, `^1.2`, `~1.2.3`, `=1.2.3`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The minisign public key that signs `source`'s releases (not needed for the official source)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub config: serde_json::Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub credentials: BTreeMap<String, Credential>,
}

/// 鍵の取り方：環境変数、なければ端末で入力を隠して聞く。optional なら、聞かずに渡さない
/// （プロバイダーがほかの方法で認証する。例：Cloudflare は cf のログイン）
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub struct Resource {
    pub provider: String,
    pub kind: String,
    pub config: serde_json::Value,
    /// 秘密の名前 → プロバイダーの出力の名前
    pub secrets: BTreeMap<String, String>,
    /// rotate のとき、前の値を残す秘密（最初の秘密の値）
    pub previous: Option<String>,
    /// Secret name → the identities that read it (the current form; `previous` included)
    pub readers: BTreeMap<String, BTreeSet<String>>,
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

// --- The file as written ---

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    /// The old form: one store
    #[serde(default)]
    store: Option<StoreConfig>,
    #[serde(default)]
    stores: BTreeMap<String, StoreConfig>,
    /// The old form: `member` and `templates` per role
    #[serde(default)]
    roles: BTreeMap<String, OldRole>,
    /// Name → IAM members
    #[serde(default)]
    identities: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
    resources: BTreeMap<String, RawResource>,
    #[serde(default)]
    destinations: BTreeMap<String, Destination>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OldRole {
    member: String,
    templates: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawResource {
    provider: String,
    #[serde(default, rename = "type", skip_serializing_if = "String::is_empty")]
    kind: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    config: serde_json::Value,
    /// The old form: secret name → provider output
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secrets: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stores: Option<BTreeMap<String, Vec<StoredSecret>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<RawPrevious>,
}

/// One secret of a resource in one store
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSecret {
    /// The provider output it holds
    #[serde(default = "default_output", skip_serializing_if = "is_default_output")]
    output: String,
    name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    readers: Vec<String>,
}

fn default_output() -> String {
    "value".to_owned()
}

fn is_default_output(s: &str) -> bool {
    s == "value"
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum RawPrevious {
    /// The old form
    Name(String),
    Full {
        name: String,
    },
}

impl RawPrevious {
    fn name(&self) -> &str {
        match self {
            RawPrevious::Name(n) | RawPrevious::Full { name: n } => n,
        }
    }
}

/// The current form, as `tegata migrate` prints it
#[derive(Serialize)]
struct CurrentForm<'a> {
    stores: BTreeMap<&'a str, &'a StoreConfig>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    identities: BTreeMap<&'a str, &'a Vec<String>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    providers: &'a BTreeMap<String, ProviderConfig>,
    resources: BTreeMap<&'a str, RawResource>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    destinations: &'a BTreeMap<String, Destination>,
}

impl RawConfig {
    /// Into one shape. Mixing the two forms is an error, so that it is always clear where read
    /// access comes from
    fn into_config(self) -> anyhow::Result<(Config, Vec<String>)> {
        let old = self.store.is_some();
        let mut warnings = Vec::new();
        let (store_name, store) = if old {
            anyhow::ensure!(
                self.stores.is_empty(),
                "set stores or store (the old form), not both"
            );
            anyhow::ensure!(
                self.identities.is_empty(),
                "identities go with stores; the old form (store) uses roles"
            );
            warnings.push(
                "this file uses the old form (store, roles, resources.*.secrets); \
                 `tegata migrate` prints it in the current form"
                    .to_owned(),
            );
            ("main".to_owned(), self.store.expect("old"))
        } else {
            anyhow::ensure!(
                self.roles.is_empty(),
                "roles go with the old form (store); with stores, use identities"
            );
            let mut stores = self.stores.into_iter();
            let (name, store) = stores
                .next()
                .ok_or_else(|| anyhow::anyhow!("set stores (a name → a store)"))?;
            anyhow::ensure!(
                stores.next().is_none(),
                "stores: only one store is supported for now"
            );
            (name, store)
        };
        let identities = if old {
            self.roles
                .into_iter()
                .map(|(n, r)| {
                    (
                        n,
                        Identity {
                            members: vec![r.member],
                            old_templates: Some(r.templates),
                        },
                    )
                })
                .collect()
        } else {
            self.identities
                .into_iter()
                .map(|(n, members)| {
                    let identity = Identity {
                        members,
                        old_templates: None,
                    };
                    (n, identity)
                })
                .collect()
        };
        let mut resources = BTreeMap::new();
        for (name, r) in self.resources {
            let (secrets, readers) = match (old, r.secrets, r.stores) {
                (true, Some(secrets), None) => (secrets, BTreeMap::new()),
                (true, _, _) => anyhow::bail!(
                    "resource {name}: the old form (store) maps secrets with `secrets`, not `stores`"
                ),
                (false, None, Some(stores)) => {
                    let mut secrets = BTreeMap::new();
                    let mut readers = BTreeMap::new();
                    for (s, entries) in stores {
                        anyhow::ensure!(
                            s == store_name,
                            "resource {name}: no store {s} (stores has {store_name})"
                        );
                        for e in entries {
                            anyhow::ensure!(
                                secrets.insert(e.name.clone(), e.output).is_none(),
                                "resource {name}: {} is listed twice",
                                e.name
                            );
                            readers.insert(e.name, e.readers.into_iter().collect());
                        }
                    }
                    (secrets, readers)
                }
                (false, Some(_), _) => anyhow::bail!(
                    "resource {name}: with stores, list the secrets under `stores.<store>`, not `secrets`"
                ),
                (false, None, None) => anyhow::bail!(
                    "resource {name}: list its secrets under `stores.{store_name}`"
                ),
            };
            let previous = r.previous.map(|p| p.name().to_owned());
            let mut readers: BTreeMap<String, BTreeSet<String>> = readers;
            if let (Some(prev), Some(primary)) = (&previous, secrets.keys().next()) {
                // previous is read by whoever reads the value it copies
                let of_primary = readers.get(primary).cloned().unwrap_or_default();
                readers.insert(prev.clone(), of_primary);
            }
            resources.insert(
                name,
                Resource {
                    provider: r.provider,
                    kind: r.kind,
                    config: r.config,
                    secrets,
                    previous,
                    readers,
                },
            );
        }
        Ok((
            Config {
                store_name,
                store,
                identities,
                providers: self.providers,
                resources,
                destinations: self.destinations,
                readers_from_templates: old,
            },
            warnings,
        ))
    }
}

/// 読み込んだ設定と、その置き場所
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub base_dir: PathBuf,
    /// Things to tell the user about the file (the old form)
    pub warnings: Vec<String>,
}

impl Loaded {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let raw: RawConfig = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("invalid {}", path.display()))?;
        let (config, warnings) = raw
            .into_config()
            .with_context(|| format!("invalid {}", path.display()))?;
        let base_dir = path
            .parent()
            .map(Path::to_path_buf)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from("."));
        let loaded = Self {
            config,
            base_dir,
            warnings,
        };
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
        let store_name = &self.config.store_name;
        anyhow::ensure!(
            store.gcp.is_some() != store.local.is_some(),
            "store {store_name}: set exactly one of gcp and local"
        );
        for (name, identity) in &self.config.identities {
            anyhow::ensure!(
                !identity.members.is_empty(),
                "identity {name}: list at least one member"
            );
            for m in &identity.members {
                anyhow::ensure!(
                    m.split_once(':').is_some_and(|(k, v)| !k.is_empty() && !v.is_empty()),
                    "identity {name}: {m:?} is not an IAM member (serviceAccount:..., user:..., group:...)"
                );
            }
        }
        for (name, p) in &self.config.providers {
            match (&p.command, &p.source) {
                (Some(_), None) => anyhow::ensure!(
                    p.version.is_none() && p.public_key.is_none(),
                    "provider {name}: version and public_key go with source, not command"
                ),
                (None, Some(source)) => {
                    anyhow::ensure!(
                        source
                            .strip_prefix("github.com/")
                            .is_some_and(|r| r.split('/').count() == 2),
                        "provider {name}: source looks like github.com/<owner>/<repo>"
                    );
                    let version = p.version.as_deref().ok_or_else(|| {
                        anyhow::anyhow!("provider {name}: set version with source")
                    })?;
                    semver::VersionReq::parse(version).map_err(|e| {
                        anyhow::anyhow!("provider {name}: version {version:?}: {e}")
                    })?;
                }
                _ => anyhow::bail!("provider {name}: set exactly one of command and source"),
            }
        }
        let mut owner: BTreeMap<String, &str> = BTreeMap::new();
        for (name, res) in &self.config.resources {
            anyhow::ensure!(
                valid_label(name),
                "resource name {name:?}: use lowercase letters, digits, - and _ (up to 63)"
            );
            anyhow::ensure!(!res.secrets.is_empty(), "resource {name}: no secrets");
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
            for (secret, ids) in &res.readers {
                for id in ids {
                    anyhow::ensure!(
                        self.config.identities.contains_key(id),
                        "resource {name}: {secret} is read by {id}, which is not in identities"
                    );
                }
            }
        }
        for (name, d) in &self.config.destinations {
            let gh = &d.github;
            anyhow::ensure!(
                gh.repo.split('/').count() == 2 && gh.repo.split('/').all(|p| !p.is_empty()),
                "destination {name}: repo looks like owner/repo"
            );
            anyhow::ensure!(
                gh.environment.as_deref() != Some(""),
                "destination {name}: environment is empty"
            );
            let secrets = d.secrets();
            anyhow::ensure!(!secrets.is_empty(), "destination {name}: secrets is empty");
            let mut seen = BTreeSet::new();
            for (there, here) in &secrets {
                anyhow::ensure!(
                    valid_github_secret_name(there),
                    "destination {name}: {there:?} is not a valid GitHub secret name \
                     (letters, digits and _, not starting with a digit or GITHUB_; rename it with a map)"
                );
                anyhow::ensure!(
                    seen.insert(there.to_ascii_uppercase()),
                    "destination {name}: {there} is listed twice (GitHub secret names ignore case)"
                );
                anyhow::ensure!(
                    owner.contains_key(here),
                    "destination {name}: no resource makes {here}"
                );
            }
        }
        Ok(())
    }

    /// The old form: role → the names in its templates (empty for the current form)
    pub fn old_template_names(&self) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
        self.config
            .identities
            .iter()
            .filter_map(|(id, i)| i.old_templates.as_ref().map(|t| (id, t)))
            .map(|(id, t)| Ok((id.clone(), crate::template::names_in_dir(&self.path(t))?)))
            .collect()
    }

    /// Secret → the identities that read it: from `readers`, or from the templates in the old form
    pub fn readers(&self) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
        let mut readers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        if self.config.readers_from_templates {
            for (id, names) in self.old_template_names()? {
                for n in names {
                    readers.entry(n).or_default().insert(id.clone());
                }
            }
        } else {
            for res in self.config.resources.values() {
                for (secret, ids) in res.readers.iter().filter(|(_, ids)| !ids.is_empty()) {
                    readers
                        .entry(secret.clone())
                        .or_default()
                        .extend(ids.iter().cloned());
                }
            }
        }
        Ok(readers)
    }

    /// 秘密の名前 → リソースの名前
    pub fn owner_of(&self, secret: &str) -> Option<&str> {
        self.config
            .resources
            .iter()
            .find(|(_, r)| r.all_secrets().iter().any(|s| s == secret))
            .map(|(n, _)| n.as_str())
    }

    /// The configuration in the current form, with readers taken from the templates for the
    /// old form (what `apply` grants today), so that `apply` after migrating changes nothing.
    /// Also the `tegata check` commands that compare the old roles' templates, as before
    pub fn migrate(&self) -> anyhow::Result<(String, Vec<String>)> {
        let readers = self.readers()?;
        let c = &self.config;
        let identities = c
            .identities
            .iter()
            .map(|(n, i)| (n.as_str(), &i.members))
            .collect();
        let checks = c
            .identities
            .iter()
            .filter_map(|(n, i)| {
                i.old_templates
                    .as_ref()
                    .map(|t| format!("tegata check {n} {t}"))
            })
            .collect();
        let mut resources = BTreeMap::new();
        for (name, r) in &c.resources {
            let entries = r
                .secrets
                .iter()
                .map(|(secret, output)| StoredSecret {
                    output: output.clone(),
                    name: secret.clone(),
                    readers: readers
                        .get(secret)
                        .map(|ids| ids.iter().cloned().collect())
                        .unwrap_or_default(),
                })
                .collect();
            if let Some(prev) = &r.previous {
                let mine = readers.get(prev).cloned().unwrap_or_default();
                let of_primary = readers.get(r.primary()).cloned().unwrap_or_default();
                anyhow::ensure!(
                    mine == of_primary,
                    "resource {name}: {prev} is read by {} but {} by {}; in the current form \
                     previous is read by the readers of {}, so make the templates agree first",
                    names_or_nobody(&mine),
                    r.primary(),
                    names_or_nobody(&of_primary),
                    r.primary()
                );
            }
            resources.insert(
                name.as_str(),
                RawResource {
                    provider: r.provider.clone(),
                    kind: r.kind.clone(),
                    config: r.config.clone(),
                    secrets: None,
                    stores: Some(BTreeMap::from([(c.store_name.clone(), entries)])),
                    previous: r.previous.clone().map(|name| RawPrevious::Full { name }),
                },
            );
        }
        let form = CurrentForm {
            stores: BTreeMap::from([(c.store_name.as_str(), &c.store)]),
            identities,
            providers: &c.providers,
            resources,
            destinations: &c.destinations,
        };
        Ok((serde_yaml_ng::to_string(&form)?, checks))
    }
}

fn names_or_nobody(ids: &BTreeSet<String>) -> String {
    if ids.is_empty() {
        "nobody".to_owned()
    } else {
        ids.iter().cloned().collect::<Vec<_>>().join(", ")
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

    fn load(yaml: &str) -> anyhow::Result<Loaded> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tegata.yaml");
        std::fs::write(&path, yaml).unwrap();
        Loaded::load(&path)
    }

    fn err(yaml: &str) -> String {
        format!("{:#}", load(yaml).unwrap_err())
    }

    #[test]
    fn loads_and_validates() {
        let loaded = load(
            r#"
store: { local: { dir: store } }
resources:
  a: { provider: random, secrets: { A: value }, previous: A_PREV }
  b: { provider: manual, secrets: { B: value } }
"#,
        )
        .unwrap();
        assert_eq!(loaded.owner_of("A_PREV"), Some("a"));
        assert_eq!(loaded.config.resources["a"].primary(), "A");

        assert!(err("store: { local: { dir: s } }\nresources:\n  a: { provider: random, secrets: { A: value } }\n  b: { provider: manual, secrets: { A: value } }\n").contains("in both"));
        assert!(err("store: { local: { dir: s } }\nresources:\n  a: { provider: nope, secrets: { A: value } }\n").contains("unknown provider"));
    }

    #[test]
    fn reads_the_current_form() {
        let loaded = load(
            r#"
stores:
  main: { local: { dir: store } }
identities:
  app: [serviceAccount:a@x, serviceAccount:b@x]
  edge: [group:edge@x]
resources:
  key:
    provider: random
    stores:
      main:
        - { name: KEY, readers: [app] }
    previous: { name: KEY_PREV }
  token:
    provider: cloudflare
    type: api_token
    stores:
      main:
        - { output: id, name: TOKEN_ID, readers: [app, edge] }
        - { output: value, name: TOKEN }
providers:
  cloudflare: { command: tegata-provider-cloudflare }
"#,
        )
        .unwrap();
        let c = &loaded.config;
        assert!(loaded.warnings.is_empty());
        assert_eq!(c.store_name, "main");
        assert_eq!(c.identities["app"].members.len(), 2);
        let key = &c.resources["key"];
        assert_eq!(key.secrets["KEY"], "value", "output defaults to value");
        assert_eq!(key.previous.as_deref(), Some("KEY_PREV"));
        let readers = loaded.readers().unwrap();
        assert_eq!(
            readers["KEY_PREV"], readers["KEY"],
            "previous: the readers of KEY"
        );
        assert_eq!(
            readers["TOKEN_ID"],
            BTreeSet::from(["app".to_owned(), "edge".to_owned()])
        );
        assert!(!readers.contains_key("TOKEN"));
        assert_eq!(c.resources["token"].secrets["TOKEN_ID"], "id");
    }

    #[test]
    fn refuses_mixed_or_wrong_forms() {
        let base = "stores: { main: { local: { dir: s } } }\n";
        assert!(err(&format!(
            "{base}resources:\n  a: {{ provider: random, stores: {{ other: [{{ name: A }}] }} }}\n"
        ))
        .contains("no store other"));
        assert!(err(&format!(
            "{base}resources:\n  a: {{ provider: random, stores: {{ main: [{{ name: A, readers: [nobody] }}] }} }}\n"
        ))
        .contains("not in identities"));
        assert!(err(&format!(
            "{base}resources:\n  a: {{ provider: random, secrets: {{ A: value }} }}\n"
        ))
        .contains("not `secrets`"));
        assert!(err(&format!(
            "{base}roles: {{ app: {{ member: user:a@x, templates: t }} }}\nresources: {{}}\n"
        ))
        .contains("use identities"));
        assert!(err(&format!(
            "{base}identities: {{ app: [] }}\nresources: {{}}\n"
        ))
        .contains("at least one member"));
        assert!(err(&format!(
            "{base}identities: {{ app: [someone] }}\nresources: {{}}\n"
        ))
        .contains("not an IAM member"));
        assert!(err(
            "stores: { a: { local: { dir: s } }, b: { local: { dir: t } } }\nresources: {}\n"
        )
        .contains("only one store"));
        assert!(err(
            "store: { local: { dir: s } }\nresources:\n  a: { provider: random, stores: { main: [{ name: A }] } }\n"
        )
        .contains("not `stores`"));
    }

    #[test]
    fn migrates_the_old_form() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("tpl")).unwrap();
        std::fs::write(dir.path().join("tpl/a.env"), "K=__KEY__\nP=__KEY_PREV__\n").unwrap();
        let path = dir.path().join("tegata.yaml");
        std::fs::write(
            &path,
            r#"
store: { local: { dir: store } }
roles: { app: { member: "serviceAccount:a@x", templates: tpl } }
resources:
  key: { provider: random, secrets: { KEY: value }, previous: KEY_PREV }
  other: { provider: manual, secrets: { OTHER: value } }
"#,
        )
        .unwrap();
        let old = Loaded::load(&path).unwrap();
        assert_eq!(old.warnings.len(), 1);
        let (migrated, checks) = old.migrate().unwrap();
        assert_eq!(checks, vec!["tegata check app tpl"]);
        std::fs::write(&path, &migrated).unwrap();
        let new = Loaded::load(&path).unwrap();
        assert!(new.warnings.is_empty(), "{migrated}");
        assert_eq!(new.readers().unwrap(), old.readers().unwrap(), "{migrated}");
        assert_eq!(
            new.config.identities["app"].members,
            vec!["serviceAccount:a@x"]
        );
        assert!(!migrated.contains("output: value"), "{migrated}");
    }
}
