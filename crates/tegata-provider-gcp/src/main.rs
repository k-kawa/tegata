//! tegata のプロバイダー：Google Cloud
//!
//! 型 `sql_user`（managed）：Cloud SQL（PostgreSQL）の組み込みのユーザーのパスワードを作る。
//! - プロバイダーの config：`project`
//! - config：`instance`（インスタンスの名前）、`user`（ユーザーの名前）、`bytes`（パスワードの乱数のバイト数。既定 24。16 進で書く）
//! - create：乱数のパスワードを作り、ユーザーがなければ足し、あればパスワードを替える。パスワードはユーザーに 1 つだけなので、
//!   替えた時点で前のパスワードでは新しくつなげない（つながっている接続は切れない）
//! - 出力：`password`、`user`。state：`instance`、`user`、`rotated_at`
//! - delete（prune）：何もしない。前のパスワードは替えた時点でもう効かない。ユーザーは消さない（表を持つため。消すときは手で）
//!
//! 鍵は、`TEGATA_GCP_ACCESS_TOKEN` があればそれ、なければ `gcloud auth print-access-token`（運営者の手元。tegata の Secret Manager と同じ）。
//! 試験では TEGATA_GCP_SQLADMIN_API で API の場所を替える

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use tegata_plugin::{Created, Kind, Provider, Request};

/// 操作の終わりを待つ長さ（ユーザーの操作はふつう数秒）
const WAIT: Duration = Duration::from_secs(300);

struct Gcp {
    api: String,
    http: reqwest::Client,
}

impl Gcp {
    async fn token(&self) -> anyhow::Result<String> {
        if let Ok(t) = std::env::var("TEGATA_GCP_ACCESS_TOKEN") {
            return Ok(t);
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
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }

    /// 呼んで、状態と JSON を返す
    async fn call(
        &self,
        token: &str,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<(StatusCode, Value)> {
        let mut req = self.http.request(method, url).bearer_auth(token);
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = req.send().await.context("cannot reach Cloud SQL Admin")?;
        let status = res.status();
        Ok((status, res.json().await.unwrap_or(Value::Null)))
    }

    /// ユーザーを足す・パスワードを替える。インスタンスで別の操作が動いている間（409）は待って繰り返す。
    /// 返った操作が終わるまで待つ
    async fn write_user(
        &self,
        token: &str,
        project: &str,
        method: Method,
        url: &str,
        body: &Value,
    ) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + WAIT;
        let op = loop {
            let (status, v) = self.call(token, method.clone(), url, Some(body)).await?;
            if status.is_success() {
                break v;
            }
            anyhow::ensure!(
                status == StatusCode::CONFLICT && tokio::time::Instant::now() < deadline,
                "Cloud SQL Admin returned {status}: {}",
                v["error"]["message"]
            );
            eprintln!("another operation is running on the instance; retrying");
            tokio::time::sleep(Duration::from_secs(5)).await;
        };
        let name = op["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Cloud SQL Admin returned no operation"))?
            .to_owned();
        let mut op = op;
        loop {
            if op["status"] == "DONE" {
                if let Some(errors) = op["error"]["errors"].as_array().filter(|e| !e.is_empty()) {
                    let messages: Vec<String> = errors
                        .iter()
                        .map(|e| format!("{}: {}", e["code"], e["message"]))
                        .collect();
                    anyhow::bail!("operation {name} failed: {}", messages.join("; "));
                }
                return Ok(());
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "operation {name} did not finish in {}s",
                WAIT.as_secs()
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
            let (status, v) = self
                .call(
                    token,
                    Method::GET,
                    &format!("{}/v1/projects/{project}/operations/{name}", self.api),
                    None,
                )
                .await?;
            anyhow::ensure!(
                status.is_success(),
                "Cloud SQL Admin returned {status} for operation {name}"
            );
            op = v;
        }
    }
}

/// 乱数のパスワード（16 進。URL にそのまま書ける）
fn password(bytes: u64) -> anyhow::Result<String> {
    use rand::RngCore;
    anyhow::ensure!((16..=64).contains(&bytes), "config.bytes must be 16..=64");
    let mut buf = vec![0u8; bytes as usize];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[async_trait::async_trait]
impl Provider for Gcp {
    fn describe(&self) -> BTreeMap<String, Kind> {
        BTreeMap::from([("sql_user".to_owned(), Kind::Managed)])
    }

    fn replaces(&self) -> Vec<String> {
        vec!["sql_user".to_owned()]
    }

    async fn create(&self, req: &Request) -> anyhow::Result<Created> {
        let project = req.provider_str("project")?;
        let instance = req.config_str("instance")?;
        let user = req.config_str("user")?;
        let value = password(req.config["bytes"].as_u64().unwrap_or(24))?;
        let token = self.token().await?;
        let users = format!(
            "{}/v1/projects/{project}/instances/{instance}/users",
            self.api
        );
        let (status, v) = self
            .call(&token, Method::GET, &format!("{users}/{user}"), None)
            .await?;
        let body = json!({ "name": user, "password": value });
        let rotated_at = chrono::Utc::now();
        match status {
            StatusCode::NOT_FOUND => {
                self.write_user(&token, project, Method::POST, &users, &body)
                    .await?;
                eprintln!("created Cloud SQL user {user} on {instance}");
            }
            s if s.is_success() => {
                anyhow::ensure!(
                    v["type"].as_str().is_none_or(|t| t == "BUILT_IN"),
                    "Cloud SQL user {user} on {instance} is not a built-in user ({})",
                    v["type"]
                );
                self.write_user(
                    &token,
                    project,
                    Method::PUT,
                    &format!("{users}?name={user}"),
                    &body,
                )
                .await?;
                eprintln!(
                    "set a new password on Cloud SQL user {user} on {instance}; the old one no longer works for new connections"
                );
            }
            s => anyhow::bail!(
                "Cloud SQL Admin returned {s} for user {user}: {}",
                v["error"]["message"]
            ),
        }
        Ok(Created {
            outputs: BTreeMap::from([
                ("password".to_owned(), value),
                ("user".to_owned(), user.to_owned()),
            ]),
            state: json!({
                "instance": instance,
                "user": user,
                "rotated_at": rotated_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            }),
        })
    }

    async fn delete(&self, req: &Request) -> anyhow::Result<()> {
        // パスワードは 1 つだけで、前のものは替えた時点で効かない。ユーザーは表を持つので消さない
        eprintln!(
            "Cloud SQL user {}: nothing to delete (the old password stopped working when it was replaced)",
            req.state["user"].as_str().unwrap_or("?")
        );
        Ok(())
    }
}

fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tegata_plugin::run(Gcp {
        api: std::env::var("TEGATA_GCP_SQLADMIN_API")
            .unwrap_or_else(|_| "https://sqladmin.googleapis.com".to_owned()),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("the HTTP client settings are valid"),
    })
}
