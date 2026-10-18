//! Installing providers from releases (`tegata init`), the lock file (`tegata.lock`), and finding
//! installed providers.
//!
//! A provider with a `source` (a GitHub repository) and a `version` requirement is installed from
//! that repository's releases. A release holds plain executables named
//! `tegata-provider-<name>_<version>_<os>_<arch>`, a `SHA256SUMS` file, and its minisign signature
//! `SHA256SUMS.minisig`. `tegata init` checks the signature, then each file against `SHA256SUMS`,
//! and records the chosen version and the hashes for every platform in `tegata.lock`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::config::{Loaded, ProviderConfig};

/// The repository of the official providers
pub const OFFICIAL_SOURCE: &str = "github.com/k-kawa/tegata";

/// The minisign public key that signs the official releases (key ID 8DE281298F0646BE). The same
/// key is in README.md and SECURITY.md. `None` would mean "write the key in the configuration"
pub const OFFICIAL_PUBLIC_KEY: Option<&str> =
    Some("RWS+RgaPKYHijXCDQ/JCehzLyohjXERYxShqSb6lS71/MHaCS3agTxkB");

/// The lock file, next to the configuration
pub const LOCK_FILE: &str = "tegata.lock";

const SUMS: &str = "SHA256SUMS";
const SUMS_SIG: &str = "SHA256SUMS.minisig";

/// `tegata.lock`
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Lock {
    pub version: u32,
    #[serde(default)]
    pub providers: BTreeMap<String, Locked>,
}

/// One installed provider in the lock file
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Locked {
    pub source: String,
    pub version: String,
    /// The version requirement in the configuration when this version was chosen
    pub constraint: String,
    /// The minisign public key that signed the release
    pub public_key: String,
    /// Platform (`linux_x86_64`, ...) → SHA-256 (hex) of the executable
    pub hashes: BTreeMap<String, String>,
}

impl Lock {
    pub fn load(path: &Path) -> anyhow::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                Ok(Some(serde_json::from_str(&text).with_context(|| {
                    format!("{} is broken", path.display())
                })?))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let text = serde_json::to_string_pretty(self)? + "\n";
        std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
    }
}

/// This machine's platform in release file names: `<os>_<arch>`
pub fn platform() -> anyhow::Result<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        other => anyhow::bail!("providers are not released for {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => anyhow::bail!("providers are not released for {other}"),
    };
    Ok(format!("{os}_{arch}"))
}

/// The executable's name for a provider: `tegata-provider-<name>`
pub fn binary_name(provider: &str) -> String {
    format!("tegata-provider-{provider}")
}

fn asset_name(provider: &str, version: &str, platform: &str) -> String {
    format!("{}_{version}_{platform}", binary_name(provider))
}

/// Where installed providers are kept: `TEGATA_CACHE_DIR`, or `$XDG_CACHE_HOME/tegata`, or
/// `~/.cache/tegata`
pub fn cache_dir() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("TEGATA_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(dir).join("tegata"));
    }
    let home = std::env::var_os("HOME").context("set HOME or TEGATA_CACHE_DIR")?;
    Ok(PathBuf::from(home).join(".cache").join("tegata"))
}

fn installed_path(provider: &str, locked: &Locked, platform: &str) -> anyhow::Result<PathBuf> {
    Ok(cache_dir()?
        .join("providers")
        .join(&locked.source)
        .join(&locked.version)
        .join(asset_name(provider, &locked.version, platform)))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The public key to check a provider's releases with: the one in the configuration, or the
/// official key for the official source
pub fn public_key(name: &str, p: &ProviderConfig) -> anyhow::Result<String> {
    if let Some(key) = &p.public_key {
        return Ok(key.clone());
    }
    let source = p.source.as_deref().unwrap_or_default();
    match (source == OFFICIAL_SOURCE, OFFICIAL_PUBLIC_KEY) {
        (true, Some(key)) => Ok(key.to_owned()),
        (true, None) => anyhow::bail!(
            "provider {name}: the official releases have no published key yet; set public_key"
        ),
        (false, _) => {
            anyhow::bail!("provider {name}: set public_key (the minisign public key of {source})")
        }
    }
}

/// Where releases come from
#[async_trait::async_trait]
pub trait Releases: Send + Sync {
    /// Versions released in `source`
    async fn versions(&self, source: &str) -> anyhow::Result<Vec<Version>>;
    /// One file of a release
    async fn file(&self, source: &str, version: &Version, name: &str) -> anyhow::Result<Vec<u8>>;
}

/// A local copy of releases (`TEGATA_PROVIDER_MIRROR`): `<dir>/<source>/<version>/<file>`. For
/// machines without internet access, and for tests
pub struct Mirror(pub PathBuf);

#[async_trait::async_trait]
impl Releases for Mirror {
    async fn versions(&self, source: &str) -> anyhow::Result<Vec<Version>> {
        let dir = self.0.join(source);
        let entries =
            std::fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))?;
        let mut versions = Vec::new();
        for entry in entries {
            let name = entry?.file_name();
            if let Ok(v) = Version::parse(&name.to_string_lossy()) {
                versions.push(v);
            }
        }
        Ok(versions)
    }

    async fn file(&self, source: &str, version: &Version, name: &str) -> anyhow::Result<Vec<u8>> {
        let path = self.0.join(source).join(version.to_string()).join(name);
        std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))
    }
}

/// GitHub releases. Tags are `v<version>`. Without a token, files come from the public download
/// URLs (`github.com/<owner>/<repo>/releases/download/…`), which do not count against the API's
/// rate limit for unauthenticated requests (60 an hour); only the list of releases uses the API.
/// `GITHUB_TOKEN`, if set, is sent to the API, and files are downloaded through the API (needed for
/// private repositories). What was fetched is kept for the run, so several providers from one
/// release share the requests
pub struct GitHub {
    http: reqwest::Client,
    api: String,
    download: String,
    token: Option<String>,
    versions: Mutex<BTreeMap<String, Vec<Version>>>,
    releases: Mutex<BTreeMap<(String, Version), Release>>,
    /// `SHA256SUMS` and its signature, by source and version
    sums: Mutex<BTreeMap<(String, Version, String), Vec<u8>>>,
}

impl GitHub {
    /// `TEGATA_GITHUB_API` (default `https://api.github.com`), `TEGATA_GITHUB_DOWNLOAD` (default
    /// `https://github.com`) and `GITHUB_TOKEN`
    pub fn new() -> Self {
        Self::with(
            std::env::var("TEGATA_GITHUB_API")
                .unwrap_or_else(|_| "https://api.github.com".to_owned()),
            std::env::var("TEGATA_GITHUB_DOWNLOAD")
                .unwrap_or_else(|_| "https://github.com".to_owned()),
            std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()),
        )
    }

    pub fn with(api: String, download: String, token: Option<String>) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("tegata/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("an HTTP client"),
            api,
            download,
            token,
            versions: Mutex::new(BTreeMap::new()),
            releases: Mutex::new(BTreeMap::new()),
            sums: Mutex::new(BTreeMap::new()),
        }
    }

    fn repo(source: &str) -> anyhow::Result<&str> {
        source
            .strip_prefix("github.com/")
            .filter(|r| r.split('/').count() == 2)
            .with_context(|| format!("{source}: sources look like github.com/<owner>/<repo>"))
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        let req = self.http.get(url);
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    async fn ok(
        &self,
        req: reqwest::RequestBuilder,
        url: &str,
    ) -> anyhow::Result<reqwest::Response> {
        let res = req.send().await?;
        let status = res.status();
        if status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            let hint = if self.token.is_none() {
                " (GitHub's rate limit for requests without a token; set GITHUB_TOKEN, for example GITHUB_TOKEN=$(gh auth token))"
            } else {
                ""
            };
            anyhow::bail!("GET {url}: {status}{hint}");
        }
        anyhow::ensure!(status.is_success(), "GET {url}: {status}");
        Ok(res)
    }

    async fn release(&self, source: &str, version: &Version) -> anyhow::Result<Release> {
        let key = (source.to_owned(), version.clone());
        if let Some(r) = self.releases.lock().await.get(&key) {
            return Ok(r.clone());
        }
        let repo = Self::repo(source)?;
        let url = format!("{}/repos/{repo}/releases/tags/v{version}", self.api);
        let release: Release = self.ok(self.get(&url), &url).await?.json().await?;
        self.releases.lock().await.insert(key, release.clone());
        Ok(release)
    }
}

impl Default for GitHub {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize, Clone)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize, Clone)]
struct Asset {
    name: String,
    url: String,
}

#[async_trait::async_trait]
impl Releases for GitHub {
    async fn versions(&self, source: &str) -> anyhow::Result<Vec<Version>> {
        if let Some(v) = self.versions.lock().await.get(source) {
            return Ok(v.clone());
        }
        let repo = Self::repo(source)?;
        let url = format!("{}/repos/{repo}/releases?per_page=100", self.api);
        let releases: Vec<Release> = self.ok(self.get(&url), &url).await?.json().await?;
        let versions: Vec<Version> = releases
            .iter()
            .filter(|r| !r.draft)
            .filter_map(|r| Version::parse(r.tag_name.strip_prefix('v')?).ok())
            .collect();
        self.versions
            .lock()
            .await
            .insert(source.to_owned(), versions.clone());
        Ok(versions)
    }

    async fn file(&self, source: &str, version: &Version, name: &str) -> anyhow::Result<Vec<u8>> {
        let small = name == SUMS || name == SUMS_SIG;
        let key = (source.to_owned(), version.clone(), name.to_owned());
        if small {
            if let Some(b) = self.sums.lock().await.get(&key) {
                return Ok(b.clone());
            }
        }
        let repo = Self::repo(source)?;
        let bytes = if self.token.is_none() {
            let url = format!(
                "{}/{repo}/releases/download/v{version}/{name}",
                self.download
            );
            self.ok(self.http.get(&url), &url)
                .await?
                .bytes()
                .await?
                .to_vec()
        } else {
            let release = self.release(source, version).await?;
            let asset = release
                .assets
                .iter()
                .find(|a| a.name == name)
                .with_context(|| format!("{source} v{version} has no {name}"))?;
            let req = self
                .get(&asset.url)
                .header(reqwest::header::ACCEPT, "application/octet-stream");
            self.ok(req, &asset.url).await?.bytes().await?.to_vec()
        };
        if small {
            self.sums.lock().await.insert(key, bytes.clone());
        }
        Ok(bytes)
    }
}

/// Checks the signature of `SHA256SUMS` and returns its entries (file name → SHA-256)
fn verified_sums(
    public_key: &str,
    sums: &[u8],
    sig: &[u8],
) -> anyhow::Result<BTreeMap<String, String>> {
    let pk = minisign_verify::PublicKey::from_base64(public_key)
        .map_err(|e| anyhow::anyhow!("public key: {e}"))?;
    let sig = minisign_verify::Signature::decode(
        std::str::from_utf8(sig).context("the signature is not text")?,
    )
    .map_err(|e| anyhow::anyhow!("signature: {e}"))?;
    pk.verify(sums, &sig, false)
        .map_err(|e| anyhow::anyhow!("{SUMS} is not signed by the expected key: {e}"))?;
    let text = std::str::from_utf8(sums).context("SHA256SUMS is not text")?;
    let mut entries = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (hash, name) = line
            .split_once(char::is_whitespace)
            .with_context(|| format!("{SUMS}: cannot read {line:?}"))?;
        let name = name.trim_start().trim_start_matches('*');
        entries.insert(name.to_owned(), hash.to_lowercase());
    }
    Ok(entries)
}

/// `tegata init`: installs the providers that have a `source`, and writes `tegata.lock`. Returns
/// one line per provider for the output
pub async fn init(
    loaded: &Loaded,
    upgrade: bool,
    releases: &dyn Releases,
) -> anyhow::Result<Vec<String>> {
    let lock_path = loaded.base_dir.join(LOCK_FILE);
    let old = Lock::load(&lock_path)?.unwrap_or_default();
    let platform = platform()?;
    let mut lock = Lock {
        version: 1,
        providers: BTreeMap::new(),
    };
    let mut lines = Vec::new();
    for (name, p) in &loaded.config.providers {
        let Some(source) = &p.source else { continue };
        let constraint = p.version.clone().unwrap_or_default();
        let req = VersionReq::parse(&constraint)
            .with_context(|| format!("provider {name}: version {constraint:?}"))?;
        let key = public_key(name, p)?;
        let kept = old.providers.get(name).filter(|l| {
            !upgrade
                && l.source == *source
                && l.public_key == key
                && Version::parse(&l.version).is_ok_and(|v| req.matches(&v))
        });
        let version = match kept {
            Some(l) => Version::parse(&l.version)?,
            None => releases
                .versions(source)
                .await?
                .into_iter()
                .filter(|v| req.matches(v))
                .max()
                .with_context(|| {
                    format!("provider {name}: no release of {source} matches {constraint}")
                })?,
        };
        let sums = releases.file(source, &version, SUMS).await?;
        let sig = releases.file(source, &version, SUMS_SIG).await?;
        let entries =
            verified_sums(&key, &sums, &sig).with_context(|| format!("provider {name}"))?;
        let prefix = format!("{}_{version}_", binary_name(name));
        let hashes: BTreeMap<String, String> = entries
            .iter()
            .filter_map(|(file, hash)| Some((file.strip_prefix(&prefix)?.to_owned(), hash.clone())))
            .collect();
        if let Some(l) = kept {
            anyhow::ensure!(
                l.hashes == hashes,
                "provider {name}: the files of {source} v{version} differ from {LOCK_FILE}; \
                 the release may have been replaced (run `tegata init --upgrade` only if you trust the new files)"
            );
        }
        let hash = hashes.get(&platform).with_context(|| {
            format!("provider {name}: {source} v{version} has no build for {platform}")
        })?;
        let locked = Locked {
            source: source.clone(),
            version: version.to_string(),
            constraint: constraint.clone(),
            public_key: key,
            hashes: hashes.clone(),
        };
        let path = installed_path(name, &locked, &platform)?;
        let have = std::fs::read(&path).ok().map(|b| sha256_hex(&b));
        if have.as_deref() == Some(hash.as_str()) {
            lines.push(format!("{name}: {source} v{version} (installed)"));
        } else {
            let bytes = releases
                .file(
                    source,
                    &version,
                    &asset_name(name, &locked.version, &platform),
                )
                .await?;
            anyhow::ensure!(
                sha256_hex(&bytes) == *hash,
                "provider {name}: the downloaded file does not match {SUMS}"
            );
            write_executable(&path, &bytes)?;
            lines.push(format!("{name}: {source} v{version} (downloaded)"));
        }
        lock.providers.insert(name.clone(), locked);
    }
    if lock.providers.is_empty() && old.providers.is_empty() {
        return Ok(lines);
    }
    if lock != old {
        lock.save(&lock_path)?;
        lines.push(format!("wrote {LOCK_FILE}"));
    }
    Ok(lines)
}

fn write_executable(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().context("no parent directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp = path.with_extension("download");
    std::fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))
}

/// The installed executable of a provider with a `source`. Fails (asking to run `tegata init`)
/// when the lock file does not match the configuration or the file is missing or changed
pub fn installed(loaded: &Loaded, name: &str, p: &ProviderConfig) -> anyhow::Result<PathBuf> {
    let run_init = || format!("run `tegata init` (provider {name})");
    let source = p.source.as_deref().unwrap_or_default();
    let lock = Lock::load(&loaded.base_dir.join(LOCK_FILE))?.with_context(run_init)?;
    let locked = lock.providers.get(name).with_context(run_init)?;
    let req = VersionReq::parse(p.version.as_deref().unwrap_or_default())?;
    anyhow::ensure!(
        locked.source == source && Version::parse(&locked.version).is_ok_and(|v| req.matches(&v)),
        "{LOCK_FILE} does not match the configuration; {}",
        run_init()
    );
    let platform = platform()?;
    let hash = locked
        .hashes
        .get(&platform)
        .with_context(|| format!("{LOCK_FILE} has no {platform} build of provider {name}"))?;
    let path = installed_path(name, locked, &platform)?;
    let bytes = std::fs::read(&path).with_context(run_init)?;
    anyhow::ensure!(
        sha256_hex(&bytes) == *hash,
        "{} differs from {LOCK_FILE}; {}",
        path.display(),
        run_init()
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_official_key_is_a_valid_minisign_key() {
        let key = OFFICIAL_PUBLIC_KEY.expect("the official key");
        minisign_verify::PublicKey::from_base64(key).unwrap();
        let p = ProviderConfig {
            command: None,
            source: Some(OFFICIAL_SOURCE.to_owned()),
            version: Some("^0.1".to_owned()),
            public_key: None,
            config: Default::default(),
            credentials: Default::default(),
        };
        assert_eq!(public_key("cloudflare", &p).unwrap(), key);
        let other = ProviderConfig {
            source: Some("github.com/someone/else".to_owned()),
            ..p
        };
        let err = public_key("x", &other).unwrap_err().to_string();
        assert!(err.contains("set public_key"), "{err}");
    }

    /// The signing script and SECURITY.md carry the same key as the code
    #[test]
    fn the_official_key_is_the_same_everywhere() {
        let key = OFFICIAL_PUBLIC_KEY.expect("the official key");
        for (name, text) in [
            (
                "tools/sign-release.sh",
                include_str!("../../../tools/sign-release.sh"),
            ),
            ("SECURITY.md", include_str!("../../../SECURITY.md")),
        ] {
            assert!(text.contains(key), "{name} does not have the official key");
        }
    }
}
