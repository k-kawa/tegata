//! トークンを作って消す流れを、偽の Cloudflare の API に向けて、作ったバイナリで確かめる

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

#[derive(Default)]
struct Fake {
    created: Mutex<Vec<Value>>,
    deleted: Mutex<Vec<String>>,
}

fn authed(h: &HeaderMap) {
    assert_eq!(h["authorization"], "Bearer boot");
}

async fn call(api: &str, req: Value) -> Value {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tegata-provider-cloudflare"))
        .env("TEGATA_CLOUDFLARE_API", api)
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
    let out = child.wait_with_output().await.unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

#[tokio::test]
async fn creates_and_deletes_tokens() {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route(
            "/accounts/acc/tokens/permission_groups",
            get(|h: HeaderMap| async move {
                authed(&h);
                Json(json!({ "success": true, "result": [
                    { "id": "g-read", "name": "Workers R2 Storage Bucket Item Read", "scopes": ["com.cloudflare.edge.r2.bucket"] },
                    { "id": "g-edit", "name": "Workers R2 Storage Edit", "scopes": ["com.cloudflare.api.account"] },
                    { "id": "g-dns", "name": "DNS Write", "scopes": ["com.cloudflare.api.account.zone"] },
                ] }))
            }),
        )
        .route(
            "/zones",
            get(|| async { Json(json!({ "success": true, "result": [{ "id": "z1" }] })) }),
        )
        .route(
            "/accounts/acc/tokens",
            post(|State(f): State<Arc<Fake>>, h: HeaderMap, Json(b): Json<Value>| async move {
                authed(&h);
                f.created.lock().unwrap().push(b);
                Json(json!({ "success": true, "result": { "id": "tok-1", "value": "secret-value" } }))
            }),
        )
        .route(
            "/accounts/acc/tokens/{id}",
            delete(|State(f): State<Arc<Fake>>, h: HeaderMap, Path(id): Path<String>| async move {
                authed(&h);
                f.deleted.lock().unwrap().push(id);
                Json(json!({ "success": true }))
            }),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let base = json!({
        "protocol": 1, "type": "api_token",
        "provider_config": { "account_id": "acc" },
        "credentials": { "api_token": "boot" },
        "config": { "name": "t", "policies": [
            { "permissions": ["Workers R2 Storage Bucket Item Read"], "r2_buckets": ["rel"] },
            { "permissions": ["Workers R2 Storage Write|Workers R2 Storage Edit"], "account": true },
            { "permissions": ["DNS Write"], "zones": ["example.com"] },
        ] },
    });
    let mut req = base.clone();
    req["op"] = json!("describe");
    assert_eq!(call(&api, req).await["types"]["api_token"], "managed");

    let mut req = base.clone();
    req["op"] = json!("create");
    let res = call(&api, req).await;
    assert_eq!(res["outputs"]["id"], "tok-1");
    assert_eq!(res["outputs"]["value"], "secret-value");
    assert_eq!(
        res["outputs"]["r2_secret_access_key"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(res["state"]["id"], "tok-1");
    let body = fake.created.lock().unwrap()[0].clone();
    assert!(
        body["name"].as_str().unwrap().starts_with("t (20"),
        "{body}"
    );
    assert_eq!(
        body["policies"][0]["resources"]["com.cloudflare.edge.r2.bucket.acc_default_rel"],
        "*"
    );
    assert_eq!(body["policies"][0]["permission_groups"][0]["id"], "g-read");
    assert_eq!(body["policies"][1]["permission_groups"][0]["id"], "g-edit");
    assert_eq!(
        body["policies"][2]["resources"]["com.cloudflare.api.account.zone.z1"],
        "*"
    );

    let mut req = base.clone();
    req["op"] = json!("delete");
    req["state"] = json!({ "id": "tok-1" });
    assert!(call(&api, req).await.get("error").is_none());
    assert_eq!(*fake.deleted.lock().unwrap(), vec!["tok-1".to_owned()]);

    // 権限の名前がなければ失敗する
    let mut req = base.clone();
    req["op"] = json!("create");
    req["config"]["policies"] = json!([{ "permissions": ["Nope"], "account": true }]);
    assert!(call(&api, req).await["error"]
        .as_str()
        .unwrap()
        .contains("permission group not found"));
}

/// 鍵がなければ cf のログインの鍵（設定のファイルの oauth_token）で API を呼ぶ。期限が切れていれば cf に取り直させる
#[tokio::test]
async fn uses_the_cf_login_without_a_token() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let s2 = seen.clone();
    let app = Router::new()
        .route(
            "/accounts/acc/tokens/permission_groups",
            get(move |h: HeaderMap| {
                let s2 = s2.clone();
                async move {
                    s2.lock().unwrap().push(h["authorization"].to_str().unwrap().to_owned());
                    Json(json!({ "success": true, "result": [
                        { "id": "g-read", "name": "Workers R2 Storage Bucket Item Read", "scopes": ["com.cloudflare.edge.r2.bucket"] },
                    ] }))
                }
            }),
        )
        .route(
            "/accounts/acc/tokens",
            post(|| async { Json(json!({ "success": true, "result": { "id": "tok-9", "value": "v9" } })) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = std::env::temp_dir().join(format!("tegata-cf-login-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("default.json");
    let write_config = |token: &str, expires: &str| {
        std::fs::write(
            &config,
            json!({ "oauth_token": token, "refresh_token": "r", "expiration_time": expires })
                .to_string(),
        )
        .unwrap();
    };
    // 偽の cf：auth whoami で鍵を取り直す（設定のファイルを書き換える）
    let cf = dir.join("cf");
    std::fs::write(
        &cf,
        format!(
            "#!/bin/sh\n[ \"$*\" = \"auth whoami\" ] || exit 1\nprintf '{{\"oauth_token\":\"oauth-2\",\"refresh_token\":\"r\",\"expiration_time\":\"2999-01-01T00:00:00Z\"}}' > \"{}\"\n",
            config.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cf, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let run = |req: Value| {
        let (cf, config, api) = (cf.clone(), config.clone(), api.clone());
        async move {
            let mut child =
                tokio::process::Command::new(env!("CARGO_BIN_EXE_tegata-provider-cloudflare"))
                    .env("TEGATA_CLOUDFLARE_API", &api)
                    .env("TEGATA_CF_CONFIG", &config)
                    .env("TEGATA_CF_BIN", &cf)
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
    let req = json!({
        "protocol": 1, "op": "create", "type": "api_token",
        "provider_config": { "account_id": "acc" },
        "config": { "name": "t", "policies": [{ "permissions": ["Workers R2 Storage Bucket Item Read"], "r2_buckets": ["rel"] }] },
    });

    // 期限の内：そのまま使う
    write_config("oauth-1", "2999-01-01T00:00:00Z");
    let res = run(req.clone()).await;
    assert_eq!(res["outputs"]["id"], "tok-9", "{res}");
    // 期限が過ぎた：cf auth whoami で取り直してから使う
    write_config("oauth-old", "2000-01-01T00:00:00Z");
    let res = run(req).await;
    assert_eq!(res["outputs"]["id"], "tok-9", "{res}");
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["Bearer oauth-1".to_owned(), "Bearer oauth-2".to_owned()]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// 偽の Tunnel：秘密を替えるとトークンが替わる。コネクターの一覧は試験が決める
#[derive(Default)]
struct FakeTunnel {
    secret: Mutex<String>,
    connectors: Mutex<Value>,
}

/// Tunnel の秘密を替えてトークンを取り（create）、前の秘密でつないだコネクターが残っていれば prune（delete）を断る
#[tokio::test]
async fn rotates_tunnel_tokens_and_checks_old_connectors_on_prune() {
    use base64::Engine as _;
    let fake = Arc::new(FakeTunnel::default());
    *fake.secret.lock().unwrap() = "old".to_owned();
    *fake.connectors.lock().unwrap() = json!([]);
    let app = Router::new()
        .route(
            "/accounts/acc/cfd_tunnel",
            get(|h: HeaderMap| async move {
                authed(&h);
                Json(json!({ "success": true, "result": [
                    { "id": "tun-1", "name": "example-env" },
                    { "id": "tun-2", "name": "example-admin" },
                ] }))
            }),
        )
        .route(
            "/accounts/acc/cfd_tunnel/tun-1",
            axum::routing::patch(|State(f): State<Arc<FakeTunnel>>, h: HeaderMap, Json(b): Json<Value>| async move {
                authed(&h);
                *f.secret.lock().unwrap() = b["tunnel_secret"].as_str().unwrap().to_owned();
                Json(json!({ "success": true, "result": { "id": "tun-1" } }))
            }),
        )
        .route(
            "/accounts/acc/cfd_tunnel/tun-1/token",
            get(|State(f): State<Arc<FakeTunnel>>| async move {
                let t = json!({ "a": "acc", "t": "tun-1", "s": *f.secret.lock().unwrap() }).to_string();
                Json(json!({ "success": true, "result": base64::engine::general_purpose::STANDARD.encode(t) }))
            }),
        )
        .route(
            "/accounts/acc/cfd_tunnel/tun-1/connections",
            get(|State(f): State<Arc<FakeTunnel>>| async move {
                Json(json!({ "success": true, "result": f.connectors.lock().unwrap().clone() }))
            }),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let base = json!({
        "protocol": 1, "type": "tunnel_token",
        "provider_config": { "account_id": "acc" },
        "credentials": { "api_token": "boot" },
        "config": { "tunnel": "example-env" },
    });
    let mut req = base.clone();
    req["op"] = json!("describe");
    assert_eq!(call(&api, req).await["types"]["tunnel_token"], "managed");

    // 新しい秘密を設定し、そのトークンを返す
    let mut req = base.clone();
    req["op"] = json!("create");
    let res = call(&api, req).await;
    assert!(res.get("error").is_none(), "{res}");
    let secret = fake.secret.lock().unwrap().clone();
    assert_ne!(secret, "old");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&secret)
            .unwrap()
            .len(),
        32
    );
    let token: Value = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(res["outputs"]["value"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(token["s"], secret.as_str());
    assert_eq!(res["state"]["tunnel_id"], "tun-1");
    let rotated_at = res["state"]["rotated_at"].as_str().unwrap().to_owned();
    let before = "2026-01-01T00:00:00Z";
    let after = "2999-01-01T00:00:00Z";

    // 入れ替えより前に起動したコネクターが残っていれば、prune を断る（どこからつないでいるかを出す）
    *fake.connectors.lock().unwrap() = json!([
        { "id": "c-old", "version": "2026.10.0", "run_at": before, "conns": [{ "origin_ip": "10.1.0.4" }] },
        { "id": "c-new", "version": "2026.10.0", "run_at": after, "conns": [{ "origin_ip": "10.1.0.2" }] },
    ]);
    let mut req = base.clone();
    req["op"] = json!("delete");
    req["state"] = json!({ "tunnel_id": "tun-1", "rotated_at": before });
    req["current_state"] = res["state"].clone();
    let err = call(&api, req.clone()).await["error"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        err.contains("c-old") && err.contains("10.1.0.4") && !err.contains("c-new"),
        "{err}"
    );

    // 残っていなければ、前のトークンは片付いた
    *fake.connectors.lock().unwrap() = json!([
        { "id": "c-new", "version": "2026.10.0", "run_at": after, "conns": [{ "origin_ip": "10.1.0.2" }] },
    ]);
    assert!(call(&api, req).await.get("error").is_none());

    // 残す世代が tegata で作ったものでなければ（手で入れた値）、確かめずに通す
    let mut req = base.clone();
    req["op"] = json!("delete");
    assert!(call(&api, req).await.get("error").is_none());
    assert!(!rotated_at.is_empty());
}

/// Turnstile の widget：作る（世代ごとに新しい widget）、設定を確かめる、消す（もうないものは消えたものとする）
#[tokio::test]
async fn creates_and_deletes_turnstile_widgets() {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route(
            "/accounts/acc/challenges/widgets",
            post(|State(f): State<Arc<Fake>>, h: HeaderMap, Json(b): Json<Value>| async move {
                authed(&h);
                let n = {
                    let mut c = f.created.lock().unwrap();
                    c.push(b);
                    c.len()
                };
                Json(json!({ "success": true, "result": {
                    "sitekey": format!("0x4AAAA-site-{n}"), "secret": format!("0x4AAAA-secret-{n}"),
                } }))
            }),
        )
        .route(
            "/accounts/acc/challenges/widgets/{sitekey}",
            delete(|State(f): State<Arc<Fake>>, h: HeaderMap, Path(k): Path<String>| async move {
                authed(&h);
                f.deleted.lock().unwrap().push(k.clone());
                if k == "gone" {
                    (
                        axum::http::StatusCode::NOT_FOUND,
                        Json(json!({ "success": false, "errors": [{ "code": 10007, "message": "not found" }] })),
                    )
                } else {
                    (axum::http::StatusCode::OK, Json(json!({ "success": true, "result": {} })))
                }
            }),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let base = json!({
        "protocol": 1, "type": "turnstile_widget",
        "provider_config": { "account_id": "acc" },
        "credentials": { "api_token": "boot" },
        "config": { "name": "sign-up", "domains": ["apps.example.com"] },
    });
    let mut req = base.clone();
    req["op"] = json!("describe");
    assert_eq!(
        call(&api, req).await["types"]["turnstile_widget"],
        "managed"
    );

    // 作る：mode の既定は managed。名前に作った日時が付く
    let mut req = base.clone();
    req["op"] = json!("create");
    let res = call(&api, req.clone()).await;
    assert_eq!(res["outputs"]["sitekey"], "0x4AAAA-site-1");
    assert_eq!(res["outputs"]["secret"], "0x4AAAA-secret-1");
    assert_eq!(res["state"], json!({ "sitekey": "0x4AAAA-site-1" }));
    let body = fake.created.lock().unwrap()[0].clone();
    assert!(
        body["name"].as_str().unwrap().starts_with("sign-up (20"),
        "{body}"
    );
    assert_eq!(body["domains"], json!(["apps.example.com"]));
    assert_eq!(body["mode"], "managed");

    // 次の世代は別の widget（サイトキーと秘密の鍵が一緒に替わる）
    let mut req2 = req.clone();
    req2["config"]["mode"] = json!("invisible");
    let res = call(&api, req2).await;
    assert_eq!(res["outputs"]["sitekey"], "0x4AAAA-site-2");
    assert_eq!(fake.created.lock().unwrap()[1]["mode"], "invisible");

    // 設定の誤り
    for (config, want) in [
        (
            json!({ "name": "x", "domains": [] }),
            "domains needs at least one host name",
        ),
        (
            json!({ "name": " ", "domains": ["a.example"] }),
            "name is empty",
        ),
        (
            json!({ "name": "x", "domains": ["a.example"], "mode": "loud" }),
            "mode must be one of",
        ),
        (
            json!({ "name": "x", "domains": ["a.example"], "region": "world" }),
            "invalid config",
        ),
    ] {
        let mut bad = req.clone();
        bad["config"] = config;
        let err = call(&api, bad).await["error"].as_str().unwrap().to_owned();
        assert!(err.contains(want), "{err}");
    }
    assert_eq!(fake.created.lock().unwrap().len(), 2);

    // 消す。もうないものは消えたものとする
    for sitekey in ["0x4AAAA-site-1", "gone"] {
        let mut req = base.clone();
        req["op"] = json!("delete");
        req["state"] = json!({ "sitekey": sitekey });
        let res = call(&api, req).await;
        assert!(res.get("error").is_none(), "{res}");
    }
    assert_eq!(
        *fake.deleted.lock().unwrap(),
        vec!["0x4AAAA-site-1".to_owned(), "gone".to_owned()]
    );
}
