//! tegata のプロバイダー：Tailscale
//!
//! 型 `auth_key`（managed）：機器を tailnet に入れる参加の鍵を作る・消す。
//! - 鍵：`client_id`、`client_secret`（OAuth client。Auth Keys: Write と、付けるタグ）
//! - config：`tags`、`reusable`（既定 false）、`ephemeral`（既定 false）、`preauthorized`（既定 true）、
//!   `expiry_seconds`（既定 7 日）、`description`
//! - 出力：`key`。state：`id`。鍵が切れる時刻を `expires` で返す
//!
//! 試験では TEGATA_TAILSCALE_API で API の場所を替える

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context;
use serde_json::{json, Value};
use tegata_plugin::{Created, Kind, Provider, Request};

/// Tailscale's default lifetime of an auth key: 7 days
const DEFAULT_EXPIRY: u64 = 7 * 24 * 3600;

struct Tailscale {
    api: String,
    http: reqwest::Client,
}

impl Tailscale {
    async fn access_token(&self, req: &Request) -> anyhow::Result<String> {
        let res = self
            .http
            .post(format!("{}/api/v2/oauth/token", self.api))
            .form(&[
                ("client_id", req.credential("client_id")?),
                ("client_secret", req.credential("client_secret")?),
            ])
            .send()
            .await
            .context("cannot reach Tailscale")?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        anyhow::ensure!(status.is_success(), "Tailscale OAuth returned {status}");
        v["access_token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Tailscale returned no access token"))
    }
}

#[async_trait::async_trait]
impl Provider for Tailscale {
    fn describe(&self) -> BTreeMap<String, Kind> {
        BTreeMap::from([("auth_key".to_owned(), Kind::Managed)])
    }

    async fn create(&self, req: &Request) -> anyhow::Result<Created> {
        let c = &req.config;
        let tags = c["tags"].as_array().cloned().unwrap_or_default();
        anyhow::ensure!(
            !tags.is_empty(),
            "config.tags is required (keys made by OAuth clients must have tags)"
        );
        let expiry_seconds = c["expiry_seconds"].as_u64().unwrap_or(DEFAULT_EXPIRY);
        let requested_at = chrono::Utc::now();
        let body = json!({
            "capabilities": { "devices": { "create": {
                "reusable": c["reusable"].as_bool().unwrap_or(false),
                "ephemeral": c["ephemeral"].as_bool().unwrap_or(false),
                "preauthorized": c["preauthorized"].as_bool().unwrap_or(true),
                "tags": tags,
            } } },
            "expirySeconds": expiry_seconds,
            "description": c["description"].as_str().unwrap_or(&req.resource),
        });
        let res = self
            .http
            .post(format!("{}/api/v2/tailnet/-/keys", self.api))
            .bearer_auth(self.access_token(req).await?)
            .json(&body)
            .send()
            .await
            .context("cannot reach Tailscale")?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        anyhow::ensure!(
            status.is_success(),
            "Tailscale returned {status}: {}",
            v["message"]
        );
        let id = v["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no key id returned"))?;
        let key = v["key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no key returned"))?;
        eprintln!("created Tailscale auth key {id}");
        Ok(Created {
            outputs: BTreeMap::from([("key".to_owned(), key.to_owned())]),
            state: json!({ "id": id }),
            // Tailscale says when the key expires; otherwise, from expiry_seconds
            expires: Some(match v["expires"].as_str() {
                Some(t) => t.to_owned(),
                None => (requested_at
                    + chrono::TimeDelta::seconds(
                        i64::try_from(expiry_seconds).unwrap_or(i64::MAX / 1_000),
                    ))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            }),
        })
    }

    async fn delete(&self, req: &Request) -> anyhow::Result<()> {
        let id = req.state["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("state.id is missing"))?;
        let res = self
            .http
            .delete(format!("{}/api/v2/tailnet/-/keys/{id}", self.api))
            .bearer_auth(self.access_token(req).await?)
            .send()
            .await
            .context("cannot reach Tailscale")?;
        let status = res.status();
        // 使った鍵、切れた鍵はもうない
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            eprintln!("deleted Tailscale auth key {id}");
            return Ok(());
        }
        anyhow::bail!("Tailscale returned {status} when deleting key {id}")
    }
}

fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tegata_plugin::run(Tailscale {
        api: std::env::var("TEGATA_TAILSCALE_API")
            .unwrap_or_else(|_| "https://api.tailscale.com".to_owned()),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("the HTTP client settings are valid"),
    })
}
