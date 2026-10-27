//! Google Cloud の Secret Manager（REST）
//!
//! 鍵は、`TEGATA_GCP_ACCESS_TOKEN` があればそれ、GCP の VM ならメタデータのサーバー（VM のサービスアカウント）、
//! なければ `gcloud auth print-access-token`（運営者の手元）から取る。試験では `TEGATA_GCP_API` と
//! `TEGATA_GCP_METADATA` で、Secret Manager とメタデータのサーバーの場所を替える

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::Context;
use base64::Engine as _;
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::{SecretMeta, Store, Version};

const ACCESSOR: &str = "roles/secretmanager.secretAccessor";

pub struct GcpStore {
    project: String,
    location: Option<String>,
    api: String,
    http: reqwest::Client,
    auth: Auth,
}

enum Auth {
    Static(String),
    Metadata {
        base: String,
        cached: Mutex<Option<String>>,
    },
    Gcloud {
        cached: Mutex<Option<String>>,
    },
}

fn metadata_base() -> String {
    std::env::var("TEGATA_GCP_METADATA").unwrap_or_else(|_| "http://169.254.169.254".to_owned())
}

/// GCP の VM の上か（試験では TEGATA_GCP_METADATA があれば）
fn on_gce() -> bool {
    std::env::var("TEGATA_GCP_METADATA").is_ok()
        || std::fs::read_to_string("/sys/class/dmi/id/product_name")
            .is_ok_and(|s| s.contains("Google"))
}

fn client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("the HTTP client settings are valid")
}

/// メタデータのサーバーに聞く
async fn metadata(http: &reqwest::Client, base: &str, path: &str) -> anyhow::Result<String> {
    let res = http
        .get(format!("{base}/computeMetadata/v1/{path}"))
        .header("Metadata-Flavor", "Google")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("cannot reach the metadata server")?;
    anyhow::ensure!(
        res.status().is_success(),
        "the metadata server returned {} for {path}",
        res.status()
    );
    Ok(res.text().await?)
}

impl GcpStore {
    /// project がなければ、メタデータのサーバーに聞く（VM の上の render）
    pub async fn new(project: Option<String>, location: Option<String>) -> anyhow::Result<Self> {
        let http = client();
        let auth = if let Ok(t) = std::env::var("TEGATA_GCP_ACCESS_TOKEN") {
            Auth::Static(t)
        } else if on_gce() {
            Auth::Metadata {
                base: metadata_base(),
                cached: Mutex::new(None),
            }
        } else {
            Auth::Gcloud {
                cached: Mutex::new(None),
            }
        };
        let project = match project {
            Some(p) => p,
            None => metadata(&http, &metadata_base(), "project/project-id")
                .await
                .context("pass --project, or run on a GCP VM")?,
        };
        Ok(Self {
            project,
            location,
            api: std::env::var("TEGATA_GCP_API")
                .unwrap_or_else(|_| "https://secretmanager.googleapis.com".to_owned()),
            http,
            auth,
        })
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    async fn token(&self) -> anyhow::Result<String> {
        match &self.auth {
            Auth::Static(t) => Ok(t.clone()),
            Auth::Metadata { base, cached } => {
                let mut c = cached.lock().await;
                if let Some(t) = c.as_ref() {
                    return Ok(t.clone());
                }
                #[derive(Deserialize)]
                struct Token {
                    access_token: String,
                }
                let body =
                    metadata(&self.http, base, "instance/service-accounts/default/token").await?;
                let t: Token = serde_json::from_str(&body)?;
                *c = Some(t.access_token.clone());
                Ok(t.access_token)
            }
            Auth::Gcloud { cached } => {
                let mut c = cached.lock().await;
                if let Some(t) = c.as_ref() {
                    return Ok(t.clone());
                }
                let out = tokio::process::Command::new("gcloud")
                    .args(["auth", "print-access-token"])
                    .stderr(std::process::Stdio::inherit())
                    .output()
                    .await
                    .context("gcloud is required (or set TEGATA_GCP_ACCESS_TOKEN)")?;
                anyhow::ensure!(
                    out.status.success(),
                    "gcloud auth print-access-token failed (run gcloud auth login)"
                );
                let t = String::from_utf8(out.stdout)?.trim().to_owned();
                *c = Some(t.clone());
                Ok(t)
            }
        }
    }

    fn secret_url(&self, name: &str) -> String {
        format!("{}/v1/projects/{}/secrets/{name}", self.api, self.project)
    }

    /// 呼んで、状態と JSON を返す
    async fn call(
        &self,
        method: Method,
        url: &str,
        body: Option<Value>,
    ) -> anyhow::Result<(StatusCode, Value)> {
        let mut req = self
            .http
            .request(method.clone(), url)
            .bearer_auth(self.token().await?);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req
            .send()
            .await
            .with_context(|| format!("cannot reach Secret Manager ({method} {url})"))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        let value = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok((status, value))
    }

    /// 成功でなければ失敗にする
    async fn ok(&self, method: Method, url: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let (status, value) = self.call(method.clone(), url, body).await?;
        anyhow::ensure!(
            status.is_success(),
            "Secret Manager returned {status} for {method} {url}: {}",
            value["error"]["message"].as_str().unwrap_or("")
        );
        Ok(value)
    }

    fn meta_of(v: &Value) -> SecretMeta {
        let map = |key: &str| -> BTreeMap<String, String> {
            v[key]
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned()))
                        .collect()
                })
                .unwrap_or_default()
        };
        SecretMeta {
            labels: map("labels"),
            annotations: map("annotations"),
        }
    }

    async fn policy(&self, name: &str) -> anyhow::Result<Value> {
        self.ok(
            Method::GET,
            &format!("{}:getIamPolicy", self.secret_url(name)),
            None,
        )
        .await
    }
}

#[async_trait::async_trait]
impl Store for GcpStore {
    async fn list(&self) -> anyhow::Result<BTreeMap<String, SecretMeta>> {
        let mut all = BTreeMap::new();
        let mut page = String::new();
        loop {
            let url = format!(
                "{}/v1/projects/{}/secrets?pageSize=250&pageToken={page}",
                self.api, self.project
            );
            let v = self.ok(Method::GET, &url, None).await?;
            for s in v["secrets"].as_array().into_iter().flatten() {
                let name = s["name"]
                    .as_str()
                    .and_then(|n| n.rsplit('/').next())
                    .unwrap_or_default()
                    .to_owned();
                all.insert(name, Self::meta_of(s));
            }
            match v["nextPageToken"].as_str() {
                Some(t) if !t.is_empty() => page = t.to_owned(),
                _ => break,
            }
        }
        Ok(all)
    }

    async fn get(&self, name: &str) -> anyhow::Result<Option<SecretMeta>> {
        let (status, v) = self.call(Method::GET, &self.secret_url(name), None).await?;
        match status {
            StatusCode::NOT_FOUND => Ok(None),
            s if s.is_success() => Ok(Some(Self::meta_of(&v))),
            s => anyhow::bail!(
                "Secret Manager returned {s} for {name}: {}",
                v["error"]["message"].as_str().unwrap_or("")
            ),
        }
    }

    async fn create(&self, name: &str, labels: &BTreeMap<String, String>) -> anyhow::Result<()> {
        let location = self
            .location
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("store.gcp.location is required to create secrets"))?;
        self.ok(
            Method::POST,
            &format!(
                "{}/v1/projects/{}/secrets?secretId={name}",
                self.api, self.project
            ),
            Some(json!({
                "replication": { "userManaged": { "replicas": [{ "location": location }] } },
                "labels": labels,
            })),
        )
        .await?;
        Ok(())
    }

    async fn update_meta(&self, name: &str, meta: &SecretMeta) -> anyhow::Result<()> {
        self.ok(
            Method::PATCH,
            &format!("{}?updateMask=labels,annotations", self.secret_url(name)),
            Some(json!({ "labels": meta.labels, "annotations": meta.annotations })),
        )
        .await?;
        Ok(())
    }

    async fn add_version(&self, name: &str, value: &str) -> anyhow::Result<u64> {
        let data = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
        let v = self
            .ok(
                Method::POST,
                &format!("{}:addVersion", self.secret_url(name)),
                Some(json!({ "payload": { "data": data } })),
            )
            .await?;
        v["name"]
            .as_str()
            .and_then(|n| n.rsplit('/').next())
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("Secret Manager returned no version for {name}"))
    }

    async fn access(&self, name: &str, version: Option<u64>) -> anyhow::Result<Option<String>> {
        let v = version.map_or_else(|| "latest".to_owned(), |n| n.to_string());
        let (status, body) = self
            .call(
                Method::GET,
                &format!("{}/versions/{v}:access", self.secret_url(name)),
                None,
            )
            .await?;
        match status {
            // ない、読めない、無効（FAILED_PRECONDITION）
            StatusCode::NOT_FOUND | StatusCode::FORBIDDEN | StatusCode::BAD_REQUEST => Ok(None),
            s if s.is_success() => {
                let data = body["payload"]["data"].as_str().unwrap_or("");
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .context("invalid payload")?;
                Ok(Some(
                    String::from_utf8(bytes).context("the value is not UTF-8")?,
                ))
            }
            s => anyhow::bail!(
                "Secret Manager returned {s} for {name}: {}",
                body["error"]["message"].as_str().unwrap_or("")
            ),
        }
    }

    async fn versions(&self, name: &str) -> anyhow::Result<Vec<Version>> {
        let mut all = Vec::new();
        let mut page = String::new();
        loop {
            let url = format!(
                "{}/versions?pageSize=100&pageToken={page}",
                self.secret_url(name)
            );
            let (status, v) = self.call(Method::GET, &url, None).await?;
            if status == StatusCode::NOT_FOUND {
                return Ok(all);
            }
            anyhow::ensure!(
                status.is_success(),
                "Secret Manager returned {status} for the versions of {name}"
            );
            for ver in v["versions"].as_array().into_iter().flatten() {
                let state = ver["state"].as_str().unwrap_or("");
                if state == "DESTROYED" {
                    continue;
                }
                if let Some(n) = ver["name"]
                    .as_str()
                    .and_then(|n| n.rsplit('/').next())
                    .and_then(|n| n.parse().ok())
                {
                    all.push(Version {
                        number: n,
                        enabled: state == "ENABLED",
                        created: ver["createTime"]
                            .as_str()
                            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                            .map(|t| t.with_timezone(&chrono::Utc)),
                    });
                }
            }
            match v["nextPageToken"].as_str() {
                Some(t) if !t.is_empty() => page = t.to_owned(),
                _ => break,
            }
        }
        Ok(all)
    }

    async fn disable(&self, name: &str, version: u64) -> anyhow::Result<()> {
        self.ok(
            Method::POST,
            &format!("{}/versions/{version}:disable", self.secret_url(name)),
            Some(json!({})),
        )
        .await?;
        Ok(())
    }

    async fn readers(&self, name: &str) -> anyhow::Result<BTreeSet<String>> {
        let p = self.policy(name).await?;
        Ok(p["bindings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["role"] == ACCESSOR)
            .flat_map(|b| b["members"].as_array().cloned().unwrap_or_default())
            .filter_map(|m| m.as_str().map(str::to_owned))
            .collect())
    }

    async fn set_readers(&self, name: &str, members: &BTreeSet<String>) -> anyhow::Result<()> {
        let mut p = self.policy(name).await?;
        let mut bindings: Vec<Value> = p["bindings"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|b| b["role"] != ACCESSOR)
            .collect();
        if !members.is_empty() {
            bindings.push(json!({ "role": ACCESSOR, "members": members }));
        }
        p["bindings"] = Value::Array(bindings);
        self.ok(
            Method::POST,
            &format!("{}:setIamPolicy", self.secret_url(name)),
            Some(json!({ "policy": p })),
        )
        .await?;
        Ok(())
    }
}
