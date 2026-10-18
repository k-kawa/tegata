//! GitHub Actions secrets of a repository or an environment (REST API).
//!
//! Values are encrypted with the destination's public key as a libsodium sealed box before they
//! are sent. The token is `TEGATA_GITHUB_TOKEN`, or else `gh auth token`; it needs the "Secrets"
//! permission (repositories) or "Environments" (environments) with write access.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Mutex, OnceCell};

use crate::config::GithubDestination;

/// How to reach GitHub: the API URL and where the token comes from
#[derive(Debug, Clone)]
pub struct GithubAccess {
    pub api: String,
    /// The token; `None` means run `gh auth token` when it is first needed
    pub token: Option<String>,
}

impl GithubAccess {
    /// `TEGATA_GITHUB_API` (default `https://api.github.com`) and `TEGATA_GITHUB_TOKEN`
    pub fn from_env() -> Self {
        Self {
            api: std::env::var("TEGATA_GITHUB_API")
                .unwrap_or_else(|_| "https://api.github.com".to_owned()),
            token: std::env::var("TEGATA_GITHUB_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
        }
    }
}

/// The token, read once per run and shared by all destinations
pub struct Token {
    access: GithubAccess,
    cached: OnceCell<String>,
}

impl Token {
    pub fn new(access: GithubAccess) -> Self {
        Self {
            access,
            cached: OnceCell::new(),
        }
    }

    async fn get(&self) -> anyhow::Result<&str> {
        self.cached
            .get_or_try_init(|| async {
                if let Some(t) = &self.access.token {
                    return Ok(t.clone());
                }
                let out = tokio::process::Command::new("gh")
                    .args(["auth", "token"])
                    .stdin(std::process::Stdio::null())
                    .output()
                    .await
                    .context("set TEGATA_GITHUB_TOKEN, or log in with `gh auth login`")?;
                let token = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                anyhow::ensure!(
                    out.status.success() && !token.is_empty(),
                    "set TEGATA_GITHUB_TOKEN, or log in with `gh auth login`"
                );
                Ok(token)
            })
            .await
            .map(String::as_str)
    }
}

pub struct Github<'a> {
    http: reqwest::Client,
    api: String,
    token: &'a Token,
    to: GithubDestination,
    /// key_id and public key
    key: Mutex<Option<(String, crypto_box::PublicKey)>>,
}

#[derive(Deserialize)]
struct PublicKey {
    key_id: String,
    key: String,
}

#[derive(Deserialize)]
struct SecretList {
    #[serde(default)]
    secrets: Vec<SecretEntry>,
}

#[derive(Deserialize)]
struct SecretEntry {
    name: String,
    updated_at: String,
}

impl<'a> Github<'a> {
    pub fn new(token: &'a Token, to: &GithubDestination) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("tegata/", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("the HTTP client settings are valid"),
            api: token.access.api.clone(),
            token,
            to: to.clone(),
            key: Mutex::new(None),
        }
    }

    /// `…/repos/{owner}/{repo}/actions/secrets[/{rest}]`, or for an environment
    /// `…/repos/{owner}/{repo}/environments/{env}/secrets[/{rest}]`, with each part escaped
    fn url(&self, rest: &[&str]) -> anyhow::Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.api).context("TEGATA_GITHUB_API is not a URL")?;
        let (owner, repo) = self.to.repo.split_once('/').context("repo is owner/repo")?;
        {
            let mut p = url
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("TEGATA_GITHUB_API is not a URL with a path"))?;
            p.pop_if_empty().extend(["repos", owner, repo]);
            match &self.to.environment {
                Some(env) => p.extend(["environments", env]),
                None => p.push("actions"),
            };
            p.push("secrets").extend(rest);
        }
        Ok(url)
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
        let res = req
            .bearer_auth(self.token.get().await?)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .with_context(|| format!("cannot reach GitHub ({})", self.to))?;
        Ok(res)
    }

    async fn ok(
        &self,
        req: reqwest::RequestBuilder,
        what: &str,
    ) -> anyhow::Result<reqwest::Response> {
        let res = self.send(req).await?;
        let status = res.status();
        if status.is_success() {
            return Ok(res);
        }
        let body: serde_json::Value = res.json().await.unwrap_or_default();
        anyhow::bail!(
            "GitHub returned {status} for {what} ({}): {}",
            self.to,
            body["message"].as_str().unwrap_or("")
        )
    }

    async fn public_key(&self) -> anyhow::Result<(String, crypto_box::PublicKey)> {
        let mut key = self.key.lock().await;
        if let Some(k) = key.as_ref() {
            return Ok(k.clone());
        }
        let url = self.url(&["public-key"])?;
        let pk: PublicKey = self
            .ok(self.http.get(url), "the public key")
            .await?
            .json()
            .await?;
        let bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
            .decode(&pk.key)
            .ok()
            .and_then(|b| b.try_into().ok())
            .context("GitHub returned a public key that is not 32 bytes of base64")?;
        let k = (pk.key_id, crypto_box::PublicKey::from(bytes));
        *key = Some(k.clone());
        Ok(k)
    }
}

/// Encrypts `value` for `key` as a libsodium sealed box, in base64
pub fn seal(key: &crypto_box::PublicKey, value: &str) -> anyhow::Result<String> {
    let sealed = key
        .seal(&mut crypto_box::aead::OsRng, value.as_bytes())
        .map_err(|_| anyhow::anyhow!("cannot encrypt the value"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(sealed))
}

#[async_trait::async_trait]
impl super::Destination for Github<'_> {
    async fn list(&self) -> anyhow::Result<BTreeMap<String, String>> {
        let mut all = BTreeMap::new();
        // repositories and environments hold at most 100 secrets
        let mut url = self.url(&[])?;
        url.query_pairs_mut().append_pair("per_page", "100");
        let list: SecretList = self
            .ok(self.http.get(url), "the list of secrets")
            .await?
            .json()
            .await?;
        for s in list.secrets {
            all.insert(s.name, s.updated_at);
        }
        Ok(all)
    }

    async fn put(&self, name: &str, value: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            value.len() <= 48 * 1024,
            "{name} is larger than 48 KB, the limit of GitHub secrets"
        );
        let (key_id, key) = self.public_key().await?;
        let body = json!({ "encrypted_value": seal(&key, value)?, "key_id": key_id });
        let url = self.url(&[name])?;
        self.ok(self.http.put(url).json(&body), &format!("writing {name}"))
            .await?;
        Ok(())
    }

    async fn delete(&self, name: &str) -> anyhow::Result<()> {
        let url = self.url(&[name])?;
        let res = self.send(self.http.delete(url)).await?;
        let status = res.status();
        anyhow::ensure!(
            status.is_success() || status == reqwest::StatusCode::NOT_FOUND,
            "GitHub returned {status} when deleting {name} ({})",
            self.to
        );
        Ok(())
    }
}
