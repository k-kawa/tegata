//! Cloud SQL のユーザーのパスワードを作る流れを、偽の Cloud SQL Admin の API に向けて、作ったバイナリで確かめる

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

#[derive(Default)]
struct Fake {
    /// ユーザー → パスワード
    users: Mutex<HashMap<String, String>>,
    /// 届いた書き込み（方法、ユーザー）
    writes: Mutex<Vec<(String, String)>>,
    /// 操作の状態を聞かれた回数
    polls: Mutex<u32>,
}

const BASE: &str = "/v1/projects/p1/instances/db1/users";

async fn call(api: &str, req: Value) -> Value {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tegata-provider-gcp"))
        .env("TEGATA_GCP_SQLADMIN_API", api)
        .env("TEGATA_GCP_ACCESS_TOKEN", "at")
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

fn authorized(h: &HeaderMap) {
    assert_eq!(h["authorization"], "Bearer at");
}

#[tokio::test]
async fn creates_users_and_replaces_passwords() {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route(
            BASE,
            get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                .post(
                    |State(f): State<Arc<Fake>>, h: HeaderMap, Json(b): Json<Value>| async move {
                        authorized(&h);
                        let name = b["name"].as_str().unwrap().to_owned();
                        f.users
                            .lock()
                            .unwrap()
                            .insert(name.clone(), b["password"].as_str().unwrap().to_owned());
                        f.writes.lock().unwrap().push(("POST".to_owned(), name));
                        // 終わっていない操作を返し、状態を聞かせる
                        Json(json!({ "name": "op-insert", "status": "PENDING" }))
                    },
                )
                .put(
                    |State(f): State<Arc<Fake>>,
                     Query(q): Query<HashMap<String, String>>,
                     Json(b): Json<Value>| async move {
                        let name = q["name"].clone();
                        if name == "broken" {
                            return Json(json!({
                                "name": "op-broken", "status": "DONE",
                                "error": { "errors": [{ "code": "INTERNAL", "message": "boom" }] },
                            }));
                        }
                        f.users
                            .lock()
                            .unwrap()
                            .insert(name.clone(), b["password"].as_str().unwrap().to_owned());
                        f.writes.lock().unwrap().push(("PUT".to_owned(), name));
                        Json(json!({ "name": "op-update", "status": "DONE" }))
                    },
                ),
        )
        .route(
            &format!("{BASE}/{{user}}"),
            get(
                |State(f): State<Arc<Fake>>, Path(user): Path<String>| async move {
                    if user == "broken" || f.users.lock().unwrap().contains_key(&user) {
                        Ok(Json(json!({ "name": user, "type": "BUILT_IN" })))
                    } else {
                        Err(StatusCode::NOT_FOUND)
                    }
                },
            ),
        )
        .route(
            "/v1/projects/p1/operations/{op}",
            get(
                |State(f): State<Arc<Fake>>, Path(op): Path<String>| async move {
                    *f.polls.lock().unwrap() += 1;
                    Json(json!({ "name": op, "status": "DONE" }))
                },
            ),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let base = json!({
        "protocol": 1, "type": "sql_user", "resource": "control_db_password",
        "provider_config": { "project": "p1" },
        "config": { "instance": "db1", "user": "control" },
    });

    // 作ると前のパスワードがすぐ効かなくなる型として申告する（tegata が作る前に確かめる）
    let res = call(&api, json!({ "protocol": 1, "op": "describe" })).await;
    assert_eq!(res["types"]["sql_user"], "managed");
    assert_eq!(res["replaces"], json!(["sql_user"]));

    // ないユーザーは足す
    let mut req = base.clone();
    req["op"] = json!("create");
    let res = call(&api, req.clone()).await;
    assert!(res.get("error").is_none(), "{res}");
    let first = res["outputs"]["password"].as_str().unwrap().to_owned();
    assert_eq!(first.len(), 48);
    assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(res["outputs"]["user"], "control");
    assert_eq!(res["state"]["user"], "control");
    assert_eq!(res["state"]["instance"], "db1");
    assert!(res["state"]["rotated_at"].is_string());
    assert_eq!(fake.users.lock().unwrap()["control"], first);
    assert_eq!(*fake.polls.lock().unwrap(), 1);

    // あるユーザーは、パスワードを替える
    let res = call(&api, req).await;
    let second = res["outputs"]["password"].as_str().unwrap().to_owned();
    assert_ne!(first, second);
    assert_eq!(fake.users.lock().unwrap()["control"], second);
    assert_eq!(
        *fake.writes.lock().unwrap(),
        vec![
            ("POST".to_owned(), "control".to_owned()),
            ("PUT".to_owned(), "control".to_owned()),
        ]
    );

    // prune は何もしない（ユーザーを消さない）
    let mut req = base.clone();
    req["op"] = json!("delete");
    req["state"] = json!({ "instance": "db1", "user": "control" });
    assert!(call(&api, req).await.get("error").is_none());
    assert!(fake.users.lock().unwrap().contains_key("control"));

    // 失敗した操作は失敗として返す
    let mut req = base;
    req["op"] = json!("create");
    req["config"]["user"] = json!("broken");
    assert!(call(&api, req).await["error"]
        .as_str()
        .unwrap()
        .contains("boom"));
}
