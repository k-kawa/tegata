//! tegata のプロバイダー：Cloudflare
//!
//! 型 `api_token`（managed）：アカウントの API トークン（アカウントが持ち、作った人がいなくなっても使える）を作る・消す。
//! - プロバイダーの config：`account_id`
//! - 鍵：`api_token`（あれば、それで API を呼ぶ。アカウントのトークンを作る権限が要る）。なければ、運営者の `cf` のログイン
//!   （OAuth。`account_api_tokens:create` を持つ）の鍵を、`cf` の設定のファイル（`oauth_token`、`expiration_time`）から読む。
//!   期限まで 2 分を切っていれば、`cf auth whoami` を流して `cf` に取り直させてから読み直す（OAuth の取り直しは自分で書かない）
//! - config：`name`（作った日時を後ろに付ける）、`policies`（権限の名前の並びと、効かせる先：`account: true`、`r2_buckets`、`zones`）。
//!   権限の名前は、時期によって Write と Edit があるので、`"A|B"` で候補を並べられる
//! - 出力：`id`、`value`、`r2_secret_access_key`（値の SHA-256。R2 の S3 の鍵のシークレット。ID がアクセスキー）
//! - state：`id`（消すときに使う）
//!
//! 型 `tunnel_token`（managed）：Cloudflare Tunnel のトークン（cloudflared がつなぐときの名乗り）。
//! Tunnel は Terraform が作る（`config_src = cloudflare`）。トークンは「アカウント・Tunnel の ID・秘密」をまとめたもので、
//! Tunnel の秘密は 1 つだけ。
//! - config：`tunnel`（Tunnel の名前）
//! - create：新しい秘密（32 バイトの乱数）を Tunnel に設定し、そのトークンを API で取る（取ったトークンの秘密が、
//!   設定したものと同じかを確かめる）。前の秘密のトークンでは、新しくつなげなくなる。つながっている cloudflared は、
//!   起動し直すまでつながったまま
//! - 出力：`value`（トークン）
//! - state：`tunnel_id`、`rotated_at`（秘密を設定した時刻）
//! - delete（prune）：前の秘密のトークンが、もう使われていないことを確かめる。残す世代の `rotated_at` より前に起動した
//!   コネクター（前の秘密でつないだもの）が残っていれば、どこからつないでいるかを出して断る（そこで cloudflared を
//!   新しいトークンで起動し直してから、もう一度 prune する）。切って消すことはしない（客のリクエストが届かなくなるため）
//!
//! 操作は、どれも API を直接呼ぶ。試験では TEGATA_CLOUDFLARE_API（API の場所）、TEGATA_CF_CONFIG（cf の設定のファイル）、
//! TEGATA_CF_BIN（cf の場所）で替える

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tegata_plugin::{Created, Kind, Provider, Request};

struct Cloudflare {
    api: String,
    cf: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct Policy {
    permissions: Vec<String>,
    #[serde(default)]
    account: bool,
    #[serde(default)]
    r2_buckets: Vec<String>,
    #[serde(default)]
    zones: Vec<String>,
}

#[derive(Deserialize)]
struct TokenConfig {
    name: String,
    policies: Vec<Policy>,
}

const ACCOUNT_SCOPE: &str = "com.cloudflare.api.account";
const ZONE_SCOPE: &str = "com.cloudflare.api.account.zone";
const BUCKET_SCOPE: &str = "com.cloudflare.edge.r2.bucket";

/// cf の設定のファイルの場所（TEGATA_CF_CONFIG、なければ OS ごとの既定）
fn cf_config_path() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("TEGATA_CF_CONFIG") {
        return Some(p.into());
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
    let candidates = [
        home.join("Library/Preferences/cloudflare/config/default.json"),
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("cloudflare/config/default.json"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// cf の設定のファイルの鍵と期限
fn read_cf_token(
    path: &std::path::Path,
) -> anyhow::Result<(String, Option<chrono::DateTime<chrono::Utc>>)> {
    let v: Value = serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?,
    )
    .with_context(|| format!("{} is not JSON", path.display()))?;
    let token = v["oauth_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("no oauth_token in {} (run `cf auth login`)", path.display())
        })?
        .to_owned();
    let expires = v["expiration_time"]
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Utc));
    Ok((token, expires))
}

fn fresh(expires: Option<chrono::DateTime<chrono::Utc>>) -> bool {
    expires.is_some_and(|t| t - chrono::Utc::now() > chrono::Duration::minutes(2))
}

impl Cloudflare {
    /// API の鍵：api_token、なければ cf のログイン
    async fn token(&self, req: &Request) -> anyhow::Result<String> {
        if let Some(t) = req.credentials.get("api_token").filter(|t| !t.is_empty()) {
            return Ok(t.clone());
        }
        let path = cf_config_path()
            .ok_or_else(|| anyhow::anyhow!("no api_token and no cf login (run `cf auth login`, or set the api_token credential)"))?;
        let (token, expires) = read_cf_token(&path)?;
        if fresh(expires) {
            return Ok(token);
        }
        // 期限が近い・過ぎた：cf に取り直させる（cf は使うときに refresh_token で取り直す）
        let out = tokio::process::Command::new(&self.cf)
            .args(["auth", "whoami"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .with_context(|| format!("cannot run {} to refresh the cf login", self.cf))?;
        anyhow::ensure!(
            out.status.success(),
            "cf auth whoami failed (run `cf auth login`): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        let (token, expires) = read_cf_token(&path)?;
        anyhow::ensure!(
            fresh(expires),
            "the cf login is still expired after refreshing (run `cf auth login`)"
        );
        Ok(token)
    }

    async fn http(
        &self,
        token: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let mut r = self
            .http
            .request(method.clone(), format!("{}{path}", self.api))
            .bearer_auth(token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let res = r
            .send()
            .await
            .with_context(|| format!("cannot reach Cloudflare ({method} {path})"))?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        if v["success"] != Value::Bool(true) {
            anyhow::bail!(
                "Cloudflare returned {status} for {method} {path}: {}",
                v["errors"]
            );
        }
        Ok(v["result"].clone())
    }

    /// 権限の名前 → ID（効かせる先の種類で絞る）
    fn group_id(groups: &Value, names: &str, scope: &str) -> anyhow::Result<String> {
        for name in names.split('|').map(str::trim) {
            let found = groups.as_array().into_iter().flatten().find(|g| {
                g["name"] == name
                    && g["scopes"]
                        .as_array()
                        .is_some_and(|s| s.iter().any(|x| x == scope))
            });
            if let Some(id) = found.and_then(|g| g["id"].as_str()) {
                return Ok(id.to_owned());
            }
        }
        anyhow::bail!("permission group not found: {names} ({scope})")
    }
}

impl Cloudflare {
    /// 名前から Tunnel の ID（消したものは除く）
    async fn tunnel_id(&self, token: &str, account: &str, name: &str) -> anyhow::Result<String> {
        let v = self
            .http(
                token,
                reqwest::Method::GET,
                &format!("/accounts/{account}/cfd_tunnel?name={name}&is_deleted=false"),
                None,
            )
            .await?;
        let found: Vec<&Value> = v
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["name"] == name)
            .collect();
        match found.as_slice() {
            [t] => t["id"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("tunnel {name} has no id")),
            [] => anyhow::bail!(
                "tunnel not found in the account: {name} (create it with Terraform first)"
            ),
            _ => anyhow::bail!("more than one tunnel is named {name}"),
        }
    }

    /// Tunnel の秘密を新しくして、そのトークンを返す
    async fn rotate_tunnel(&self, req: &Request) -> anyhow::Result<Created> {
        let account = req.provider_str("account_id")?;
        let name = req.config_str("tunnel")?;
        let token = self.token(req).await?;
        let id = self.tunnel_id(&token, account, name).await?;
        let secret: [u8; 32] = rand::random();
        let secret = base64::engine::general_purpose::STANDARD.encode(secret);
        let rotated_at = chrono::Utc::now();
        self.http(
            &token,
            reqwest::Method::PATCH,
            &format!("/accounts/{account}/cfd_tunnel/{id}"),
            Some(json!({ "tunnel_secret": secret })),
        )
        .await?;
        let v = self
            .http(
                &token,
                reqwest::Method::GET,
                &format!("/accounts/{account}/cfd_tunnel/{id}/token"),
                None,
            )
            .await?;
        let value = v
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no tunnel token returned"))?
            .to_owned();
        // トークンは base64 の JSON（a：アカウント、t：Tunnel、s：秘密）。設定した秘密のものかを確かめる
        let decoded: Value = base64::engine::general_purpose::STANDARD
            .decode(&value)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or_else(|| anyhow::anyhow!("the tunnel token is not in the expected form"))?;
        anyhow::ensure!(
            decoded["t"] == id.as_str() && decoded["s"] == secret.as_str(),
            "the tunnel token does not carry the secret that was just set (try again)"
        );
        eprintln!(
            "set a new secret on Cloudflare Tunnel {name} ({id}); connectors started before {} use the old token",
            rotated_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        );
        Ok(Created {
            outputs: BTreeMap::from([("value".to_owned(), value)]),
            state: json!({ "tunnel_id": id, "rotated_at": rotated_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true) }),
        })
    }

    /// 前の秘密のトークンが使われていないことを確かめる（消すものは Cloudflare に残らない。秘密は 1 つだけなので）
    async fn retire_tunnel_token(&self, req: &Request) -> anyhow::Result<()> {
        let account = req.provider_str("account_id")?;
        let name = req.config_str("tunnel")?;
        let Some(rotated_at) = req.current_state["rotated_at"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        else {
            // 残す世代が tegata で作ったものでない（手で入れた値）。いつ替わったかが分からないので確かめられない
            eprintln!("tunnel {name}: the latest token was not made by tegata; nothing to check");
            return Ok(());
        };
        let token = self.token(req).await?;
        let id = match req.current_state["tunnel_id"].as_str() {
            Some(id) => id.to_owned(),
            None => self.tunnel_id(&token, account, name).await?,
        };
        let v = self
            .http(
                &token,
                reqwest::Method::GET,
                &format!("/accounts/{account}/cfd_tunnel/{id}/connections"),
                None,
            )
            .await?;
        let mut old = Vec::new();
        for c in v.as_array().into_iter().flatten() {
            let run_at = c["run_at"]
                .as_str()
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
            if run_at.is_none_or(|t| t < rotated_at) {
                let origin = c["conns"][0]["origin_ip"].as_str().unwrap_or("?");
                old.push(format!(
                    "connector {} from {origin} (cloudflared {}, started {})",
                    c["id"].as_str().unwrap_or("?"),
                    c["version"].as_str().unwrap_or("?"),
                    c["run_at"].as_str().unwrap_or("?"),
                ));
            }
        }
        anyhow::ensure!(
            old.is_empty(),
            "tunnel {name}: still connected with the old token (started before the rotation at {}): {}. \
             Restart cloudflared there with the new token, then prune again",
            rotated_at.to_rfc3339(),
            old.join("; ")
        );
        eprintln!(
            "tunnel {name}: no connector uses a token older than {}",
            rotated_at.to_rfc3339()
        );
        Ok(())
    }
}

pub fn sha256_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[async_trait::async_trait]
impl Provider for Cloudflare {
    fn describe(&self) -> BTreeMap<String, Kind> {
        BTreeMap::from([
            ("api_token".to_owned(), Kind::Managed),
            ("tunnel_token".to_owned(), Kind::Managed),
        ])
    }

    /// Tunnel の秘密は 1 つだけ
    fn replaces(&self) -> Vec<String> {
        vec!["tunnel_token".to_owned()]
    }

    async fn create(&self, req: &Request) -> anyhow::Result<Created> {
        if req.kind == "tunnel_token" {
            return self.rotate_tunnel(req).await;
        }
        let cfg: TokenConfig =
            serde_json::from_value(req.config.clone()).context("invalid config")?;
        let account = req.provider_str("account_id")?;
        let token = self.token(req).await?;
        let groups = self
            .http(
                &token,
                reqwest::Method::GET,
                &format!("/accounts/{account}/tokens/permission_groups"),
                None,
            )
            .await?;
        let mut policies = Vec::new();
        for p in &cfg.policies {
            // 効かせる先の種類ごとに 1 つの policy にする（権限の範囲が先の種類と合わないと断られる）
            let mut targets: Vec<(&str, Value)> = Vec::new();
            if p.account {
                targets.push((
                    ACCOUNT_SCOPE,
                    json!({ format!("{ACCOUNT_SCOPE}.{account}"): "*" }),
                ));
            }
            if !p.r2_buckets.is_empty() {
                let mut res = serde_json::Map::new();
                for b in &p.r2_buckets {
                    res.insert(format!("{BUCKET_SCOPE}.{account}_default_{b}"), json!("*"));
                }
                targets.push((BUCKET_SCOPE, Value::Object(res)));
            }
            if !p.zones.is_empty() {
                let mut res = serde_json::Map::new();
                for z in &p.zones {
                    let v = self
                        .http(
                            &token,
                            reqwest::Method::GET,
                            &format!("/zones?name={z}&account.id={account}"),
                            None,
                        )
                        .await?;
                    let id = v[0]["id"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("zone not found in the account: {z}"))?;
                    res.insert(format!("{ZONE_SCOPE}.{id}"), json!("*"));
                }
                targets.push((ZONE_SCOPE, Value::Object(res)));
            }
            anyhow::ensure!(
                !targets.is_empty(),
                "a policy needs account, r2_buckets or zones"
            );
            for (scope, resources) in targets {
                let ids = p
                    .permissions
                    .iter()
                    .map(|n| Self::group_id(&groups, n, scope).map(|id| json!({ "id": id })))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                policies.push(
                    json!({ "effect": "allow", "resources": resources, "permission_groups": ids }),
                );
            }
        }
        // 作った日時を名前に付ける。入れ替えの間は同じ設定のトークンが 2 つあり、ダッシュボードで消すときに取り違えないように
        let name = format!(
            "{} ({})",
            cfg.name,
            chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
        );
        let v = self
            .http(
                &token,
                reqwest::Method::POST,
                &format!("/accounts/{account}/tokens"),
                Some(json!({ "name": name, "policies": policies })),
            )
            .await?;
        let id = v["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no token id returned"))?;
        let value = v["value"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no token value returned"))?;
        eprintln!("created Cloudflare account token: {name} ({id})");
        Ok(Created {
            outputs: BTreeMap::from([
                ("id".to_owned(), id.to_owned()),
                ("value".to_owned(), value.to_owned()),
                ("r2_secret_access_key".to_owned(), sha256_hex(value)),
            ]),
            state: json!({ "id": id }),
        })
    }

    async fn delete(&self, req: &Request) -> anyhow::Result<()> {
        if req.kind == "tunnel_token" {
            return self.retire_tunnel_token(req).await;
        }
        let id = req.state["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("state.id is missing"))?;
        let account = req.provider_str("account_id")?;
        let res = self
            .http
            .delete(format!("{}/accounts/{account}/tokens/{id}", self.api))
            .bearer_auth(self.token(req).await?)
            .send()
            .await
            .context("cannot reach Cloudflare")?;
        let status = res.status();
        // もうないものは消えたものとする
        anyhow::ensure!(
            status.is_success() || status == reqwest::StatusCode::NOT_FOUND,
            "Cloudflare returned {status} when deleting token {id}: {}",
            res.text().await.unwrap_or_default()
        );
        eprintln!("deleted Cloudflare account token {id}");
        Ok(())
    }
}

fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tegata_plugin::run(Cloudflare {
        api: std::env::var("TEGATA_CLOUDFLARE_API")
            .unwrap_or_else(|_| "https://api.cloudflare.com/client/v4".to_owned()),
        cf: std::env::var("TEGATA_CF_BIN").unwrap_or_else(|_| "cf".to_owned()),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("the HTTP client settings are valid"),
    })
}
