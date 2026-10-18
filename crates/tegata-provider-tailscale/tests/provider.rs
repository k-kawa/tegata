//! 参加の鍵を作って消す流れを、偽の Tailscale の API に向けて、作ったバイナリで確かめる

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, post};
use axum::{Form, Json, Router};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

#[derive(Default)]
struct Fake {
    created: Mutex<Vec<Value>>,
    deleted: Mutex<Vec<String>>,
}

async fn call(api: &str, req: Value) -> Value {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tegata-provider-tailscale"))
        .env("TEGATA_TAILSCALE_API", api)
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
    serde_json::from_slice(&child.wait_with_output().await.unwrap().stdout).unwrap()
}

#[tokio::test]
async fn creates_and_deletes_auth_keys() {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route(
            "/api/v2/oauth/token",
            post(
                |Form(f): Form<std::collections::HashMap<String, String>>| async move {
                    if f["client_id"] == "cid" && f["client_secret"] == "csec" {
                        Ok(Json(json!({ "access_token": "at" })))
                    } else {
                        Err(StatusCode::UNAUTHORIZED)
                    }
                },
            ),
        )
        .route(
            "/api/v2/tailnet/-/keys",
            post(
                |State(f): State<Arc<Fake>>, h: HeaderMap, Json(b): Json<Value>| async move {
                    assert_eq!(h["authorization"], "Bearer at");
                    f.created.lock().unwrap().push(b);
                    Json(json!({ "id": "k1", "key": "tskey-auth-xyz" }))
                },
            ),
        )
        .route(
            "/api/v2/tailnet/-/keys/{id}",
            delete(
                |State(f): State<Arc<Fake>>, Path(id): Path<String>| async move {
                    f.deleted.lock().unwrap().push(id);
                    StatusCode::NOT_FOUND // 使った鍵はもうない
                },
            ),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let base = json!({
        "protocol": 1, "type": "auth_key", "resource": "app_tailscale",
        "credentials": { "client_id": "cid", "client_secret": "csec" },
        "config": { "tags": ["tag:example"], "expiry_seconds": 3600 },
    });
    let mut req = base.clone();
    req["op"] = json!("create");
    let res = call(&api, req).await;
    assert_eq!(res["outputs"]["key"], "tskey-auth-xyz");
    assert_eq!(res["state"]["id"], "k1");
    let body = fake.created.lock().unwrap()[0].clone();
    assert_eq!(
        body["capabilities"]["devices"]["create"]["tags"][0],
        "tag:example"
    );
    assert_eq!(body["capabilities"]["devices"]["create"]["reusable"], false);
    assert_eq!(
        body["capabilities"]["devices"]["create"]["preauthorized"],
        true
    );
    assert_eq!(body["expirySeconds"], 3600);
    assert_eq!(body["description"], "app_tailscale");

    let mut req = base.clone();
    req["op"] = json!("delete");
    req["state"] = json!({ "id": "k1" });
    assert!(call(&api, req).await.get("error").is_none());
    assert_eq!(*fake.deleted.lock().unwrap(), vec!["k1".to_owned()]);

    let mut req = base;
    req["op"] = json!("create");
    req["credentials"]["client_secret"] = json!("wrong");
    assert!(call(&api, req).await["error"]
        .as_str()
        .unwrap()
        .contains("OAuth"));
}
