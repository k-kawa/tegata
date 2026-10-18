//! SES の SMTP の認証情報を作って消す流れを、偽の IAM の API（Query の形）に向けて、作ったバイナリで確かめる

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Form, Router};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

const NS: &str = "https://iam.amazonaws.com/doc/2010-05-08/";

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
}

async fn iam(
    State(f): State<Arc<Fake>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> (StatusCode, String) {
    // SDK が SigV4 で署名していること
    assert!(headers["authorization"]
        .to_str()
        .unwrap()
        .starts_with("AWS4-HMAC-SHA256 Credential=AKIATEST/"));
    let action = form["Action"].clone();
    f.calls.lock().unwrap().push(format!(
        "{action} {}",
        form.get("AccessKeyId").cloned().unwrap_or_default()
    ));
    match (action.as_str(), form.get("AccessKeyId").map(String::as_str)) {
        ("CreateAccessKey", _) => {
            assert_eq!(form["UserName"], "smtp-user");
            (
                StatusCode::OK,
                format!(
                    r#"<CreateAccessKeyResponse xmlns="{NS}"><CreateAccessKeyResult><AccessKey><UserName>smtp-user</UserName><AccessKeyId>AKIAEXAMPLE</AccessKeyId><Status>Active</Status><SecretAccessKey>wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY</SecretAccessKey><CreateDate>2026-10-15T00:00:00Z</CreateDate></AccessKey></CreateAccessKeyResult><ResponseMetadata><RequestId>r1</RequestId></ResponseMetadata></CreateAccessKeyResponse>"#
                ),
            )
        }
        ("DeleteAccessKey", Some("AKIAEXAMPLE")) => (
            StatusCode::OK,
            format!(
                r#"<DeleteAccessKeyResponse xmlns="{NS}"><ResponseMetadata><RequestId>r2</RequestId></ResponseMetadata></DeleteAccessKeyResponse>"#
            ),
        ),
        ("DeleteAccessKey", _) => (
            StatusCode::NOT_FOUND,
            format!(
                r#"<ErrorResponse xmlns="{NS}"><Error><Type>Sender</Type><Code>NoSuchEntity</Code><Message>The Access Key with id GONE cannot be found.</Message></Error><RequestId>r3</RequestId></ErrorResponse>"#
            ),
        ),
        _ => (StatusCode::BAD_REQUEST, String::new()),
    }
}

#[tokio::test]
async fn creates_and_deletes_smtp_credentials() {
    let fake = Arc::new(Fake::default());
    let app = Router::new().route("/", post(iam)).with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let run = |req: Value| {
        let endpoint = endpoint.clone();
        async move {
            let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tegata-provider-aws"))
                .env("AWS_ENDPOINT_URL_IAM", &endpoint)
                .env("AWS_ACCESS_KEY_ID", "AKIATEST")
                .env("AWS_SECRET_ACCESS_KEY", "test-secret")
                .env("AWS_CONFIG_FILE", "/nonexistent")
                .env("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent")
                .env_remove("AWS_PROFILE")
                .env_remove("AWS_REGION")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(req.to_string().as_bytes())
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&child.wait_with_output().await.unwrap().stdout)
                .unwrap()
        }
    };
    let base = json!({
        "protocol": 1, "type": "ses_smtp_credentials",
        "config": { "user": "smtp-user", "region": "us-west-2" },
    });
    let mut req = base.clone();
    req["op"] = json!("create");
    let res = run(req).await;
    assert_eq!(res["outputs"]["username"], "AKIAEXAMPLE", "{res}");
    assert_eq!(
        res["outputs"]["password"],
        include_str!("expected-smtp-password.txt"),
        "the SMTP password follows the SES derivation"
    );
    assert!(
        !res.to_string().contains("wJalrXUtnFEMI"),
        "the secret access key is not returned"
    );
    assert_eq!(
        res["state"],
        json!({ "user": "smtp-user", "access_key_id": "AKIAEXAMPLE" })
    );

    // 消す。もうないもの（NoSuchEntity）は消えたものとする
    for id in ["AKIAEXAMPLE", "GONE"] {
        let mut req = base.clone();
        req["op"] = json!("delete");
        req["state"] = json!({ "user": "smtp-user", "access_key_id": id });
        let res = run(req).await;
        assert!(res.get("error").is_none(), "delete {id}: {res}");
    }
    assert_eq!(
        *fake.calls.lock().unwrap(),
        vec![
            "CreateAccessKey ".to_owned(),
            "DeleteAccessKey AKIAEXAMPLE".to_owned(),
            "DeleteAccessKey GONE".to_owned()
        ]
    );
}
