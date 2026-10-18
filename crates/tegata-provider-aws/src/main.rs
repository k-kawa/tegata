//! tegata のプロバイダー：AWS
//!
//! 型 `ses_smtp_credentials`（managed）：Amazon SES の SMTP の認証情報を作る・消す。
//! IAM ユーザー（SES で送るだけの権限。作るのは手で）のアクセスキーを作り、
//! そのシークレットから SMTP のパスワードを作る（SES の決まり：SigV4 の鍵の導き方で、`SendRawEmail` に署名した値の先頭に
//! 0x04 を付けて base64。https://docs.aws.amazon.com/ses/latest/dg/smtp-credentials.html）。
//! - config：`user`（IAM ユーザー）、`region`（SES のリージョン。既定 us-west-2）、`profile`（aws のプロファイル。省ける）
//! - 出力：`username`（アクセスキーの ID）、`password`（SMTP のパスワード）。アクセスキーのシークレットそのものは返さない
//! - state：`user`、`access_key_id`（消すときに使う）
//!
//! AWS の SDK で IAM の API を直接呼ぶ。鍵は SDK の決まった順番で探す（環境変数、`~/.aws` の credentials と config、
//! `aws sso login` の結果など。Terraform の AWS の provider と同じ）。`aws` の CLI は要らない。
//! IAM ユーザーのアクセスキーは 2 つまでなので、入れ替えは rotate → 確かめる → prune の順にする。
//! 試験では AWS_ENDPOINT_URL_IAM で IAM の API の場所を替える

use std::collections::BTreeMap;

use anyhow::Context;
use aws_smithy_http_client::tls;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use tegata_plugin::{Created, Kind, Provider, Request};

struct Aws;

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// SES の SMTP のパスワード（アクセスキーのシークレットとリージョンから）
pub fn smtp_password(secret: &str, region: &str) -> String {
    let mut sig = hmac(format!("AWS4{secret}").as_bytes(), b"11111111");
    for part in [region, "ses", "aws4_request", "SendRawEmail"] {
        sig = hmac(&sig, part.as_bytes());
    }
    let mut v = vec![0x04];
    v.extend(sig);
    base64::engine::general_purpose::STANDARD.encode(v)
}

/// IAM の SDK のクライアント（TLS は rustls と ring）。config.profile があればそのプロファイル
async fn iam(req: &Request) -> aws_sdk_iam::Client {
    let http = aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(
            tls::rustls_provider::CryptoMode::Ring,
        ))
        .build_https();
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest()).http_client(http);
    if let Some(p) = req.config["profile"].as_str() {
        loader = loader.profile_name(p);
    }
    let conf = loader.load().await;
    // IAM はリージョンによらない。リージョンの設定がなければ us-east-1 で署名する
    let mut b = aws_sdk_iam::config::Builder::from(&conf);
    if conf.region().is_none() {
        b = b.region(aws_sdk_iam::config::Region::new("us-east-1"));
    }
    aws_sdk_iam::Client::from_conf(b.build())
}

/// SDK の失敗を、理由が読める形にする
fn sdk_error<E: std::error::Error + Send + Sync + 'static>(
    e: aws_sdk_iam::error::SdkError<E>,
) -> anyhow::Error {
    anyhow::anyhow!(aws_sdk_iam::error::DisplayErrorContext(e).to_string())
}

#[async_trait::async_trait]
impl Provider for Aws {
    fn describe(&self) -> BTreeMap<String, Kind> {
        BTreeMap::from([("ses_smtp_credentials".to_owned(), Kind::Managed)])
    }

    async fn create(&self, req: &Request) -> anyhow::Result<Created> {
        let user = req.config_str("user")?;
        let region = req.config["region"].as_str().unwrap_or("us-west-2");
        let res = iam(req)
            .await
            .create_access_key()
            .user_name(user)
            .send()
            .await
            .map_err(sdk_error)
            .context("cannot create an access key (an IAM user can have at most 2; prune the old one first)")?;
        let key = res
            .access_key()
            .ok_or_else(|| anyhow::anyhow!("no access key returned"))?;
        let (id, secret) = (key.access_key_id(), key.secret_access_key());
        eprintln!("created an access key for {user} ({id})");
        Ok(Created {
            outputs: BTreeMap::from([
                ("username".to_owned(), id.to_owned()),
                ("password".to_owned(), smtp_password(secret, region)),
            ]),
            state: json!({ "user": user, "access_key_id": id }),
        })
    }

    async fn delete(&self, req: &Request) -> anyhow::Result<()> {
        let user = req.state["user"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("state.user is missing"))?;
        let id = req.state["access_key_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("state.access_key_id is missing"))?;
        match iam(req)
            .await
            .delete_access_key()
            .user_name(user)
            .access_key_id(id)
            .send()
            .await
        {
            Ok(_) => {}
            // もうないものは消えたものとする
            Err(e)
                if e.as_service_error()
                    .is_some_and(|se| se.is_no_such_entity_exception()) => {}
            Err(e) => {
                return Err(sdk_error(e).context(format!("cannot delete the access key {id}")))
            }
        }
        eprintln!("deleted the access key {id} of {user}");
        Ok(())
    }
}

fn main() {
    tegata_plugin::run(Aws)
}

#[cfg(test)]
mod tests {
    /// AWS の文書の手順（Python の例）で、同じシークレットから計算した値と比べる
    #[test]
    fn derives_the_smtp_password() {
        let p = super::smtp_password("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", "us-west-2");
        assert_eq!(p.len(), 44);
        assert!(
            p.starts_with("B"),
            "the version byte 0x04 encodes to a leading B"
        );
        assert_eq!(p, EXPECTED);
    }
    const EXPECTED: &str = include_str!("../tests/expected-smtp-password.txt");
}
