//! Secret Manager の置き場所（REST）を、偽の Secret Manager とメタデータのサーバーに向けて確かめる

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{json, Value};
use tegata::store::gcp::GcpStore;
use tegata::store::Store;

#[derive(Default)]
struct Secret {
    labels: Value,
    annotations: Value,
    versions: Vec<(u64, String, bool)>,
    policy: Value,
}

#[derive(Default)]
struct Fake {
    secrets: Mutex<BTreeMap<String, Secret>>,
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": { "message": msg } }))).into_response()
}

async fn handle(State(f): State<Arc<Fake>>, req: Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let query = req.uri().query().unwrap_or("").to_owned();
    let auth = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body: Bytes = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    // メタデータのサーバー
    if let Some(p) = path.strip_prefix("/computeMetadata/v1/") {
        return match p {
            "project/project-id" => "proj".into_response(),
            "instance/service-accounts/default/token" => {
                Json(json!({ "access_token": "meta-token" })).into_response()
            }
            _ => StatusCode::NOT_FOUND.into_response(),
        };
    }
    if auth != "Bearer test-token" && auth != "Bearer meta-token" {
        return err(StatusCode::UNAUTHORIZED, "bad token");
    }
    let Some(rest) = path.strip_prefix("/v1/projects/proj/secrets") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut secrets = f.secrets.lock().unwrap();
    let rest = rest.trim_start_matches('/');
    if rest.is_empty() {
        return match method {
            Method::GET => Json(json!({ "secrets": secrets.iter().map(|(n, s)| json!({
                "name": format!("projects/proj/secrets/{n}"), "labels": s.labels, "annotations": s.annotations,
            })).collect::<Vec<_>>() }))
            .into_response(),
            Method::POST => {
                let id = query.strip_prefix("secretId=").unwrap_or("").to_owned();
                assert_eq!(body["replication"]["userManaged"]["replicas"][0]["location"], "asia-northeast1");
                if secrets.contains_key(&id) {
                    return err(StatusCode::CONFLICT, "exists");
                }
                secrets.insert(
                    id.clone(),
                    Secret {
                        labels: body["labels"].clone(),
                        annotations: json!({}),
                        policy: json!({ "etag": "e0" }),
                        ..Default::default()
                    },
                );
                Json(json!({ "name": format!("projects/proj/secrets/{id}") })).into_response()
            }
            _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
        };
    }
    let (name, tail) = match rest.split_once('/') {
        Some((n, t)) => (n.to_owned(), t.to_owned()),
        None => (rest.to_owned(), String::new()),
    };
    let (name, action) = match name.split_once(':') {
        Some((n, a)) => (n.to_owned(), a.to_owned()),
        None => (name, String::new()),
    };
    let Some(s) = secrets.get_mut(&name) else {
        return err(StatusCode::NOT_FOUND, "no secret");
    };
    match (method, action.as_str(), tail.as_str()) {
        (Method::GET, "", "") => {
            Json(json!({ "labels": s.labels, "annotations": s.annotations })).into_response()
        }
        (Method::PATCH, "", "") => {
            assert_eq!(query, "updateMask=labels,annotations");
            s.labels = body["labels"].clone();
            s.annotations = body["annotations"].clone();
            Json(json!({})).into_response()
        }
        (Method::POST, "addVersion", "") => {
            let data = body["payload"]["data"].as_str().unwrap();
            let v = String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap(),
            )
            .unwrap();
            let n = s.versions.len() as u64 + 1;
            s.versions.push((n, v, true));
            Json(json!({ "name": format!("projects/proj/secrets/{name}/versions/{n}") }))
                .into_response()
        }
        (Method::GET, "getIamPolicy", "") => Json(s.policy.clone()).into_response(),
        (Method::POST, "setIamPolicy", "") => {
            assert_eq!(body["policy"]["etag"], s.policy["etag"]);
            s.policy = body["policy"].clone();
            s.policy["etag"] = json!("e1");
            Json(s.policy.clone()).into_response()
        }
        (Method::GET, "", "versions") => Json(
            json!({ "versions": s.versions.iter().map(|(n, _, e)| json!({
            "name": format!("projects/proj/secrets/{name}/versions/{n}"),
            "state": if *e { "ENABLED" } else { "DISABLED" },
            "createTime": format!("2026-10-0{n}T09:30:00.123456Z"),
        })).collect::<Vec<_>>() }),
        )
        .into_response(),
        (method, "", t) if t.starts_with("versions/") => {
            let (v, act) = t["versions/".len()..].split_once(':').unwrap();
            let found = if v == "latest" {
                s.versions.iter_mut().rev().find(|x| x.2)
            } else {
                s.versions.iter_mut().find(|x| x.0.to_string() == v)
            };
            let Some(found) = found else {
                return err(StatusCode::NOT_FOUND, "no version");
            };
            match (method, act) {
                (Method::GET, "access") if !found.2 => err(StatusCode::BAD_REQUEST, "disabled"),
                (Method::GET, "access") => Json(json!({ "payload": {
                    "data": base64::engine::general_purpose::STANDARD.encode(found.1.as_bytes())
                } }))
                .into_response(),
                (Method::POST, "disable") => {
                    found.2 = false;
                    Json(json!({})).into_response()
                }
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[tokio::test]
async fn talks_to_secret_manager() {
    let fake = Arc::new(Fake::default());
    let app = Router::new().fallback(handle).with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::env::set_var("TEGATA_GCP_API", format!("http://{addr}"));
    std::env::set_var("TEGATA_GCP_ACCESS_TOKEN", "test-token");

    // 運営者の手元の形（鍵を渡す）
    let store = GcpStore::new(Some("proj".into()), Some("asia-northeast1".into()))
        .await
        .unwrap();
    assert!(store.get("A").await.unwrap().is_none());
    assert_eq!(store.access("A", None).await.unwrap(), None);
    store
        .create(
            "A",
            &BTreeMap::from([("managed-by".into(), "tegata".into())]),
        )
        .await
        .unwrap();
    assert_eq!(store.add_version("A", "v1 with ünïcode").await.unwrap(), 1);
    assert_eq!(store.add_version("A", "v2").await.unwrap(), 2);
    assert_eq!(
        store.access("A", None).await.unwrap().as_deref(),
        Some("v2")
    );
    store.disable("A", 2).await.unwrap();
    assert_eq!(
        store.access("A", None).await.unwrap().as_deref(),
        Some("v1 with ünïcode")
    );
    assert_eq!(store.access("A", Some(2)).await.unwrap(), None);
    let vs = store.versions("A").await.unwrap();
    assert_eq!(vs.len(), 2);
    // Secret Manager's createTime is each version's age
    assert_eq!(
        vs[0].created.map(|t| t.to_rfc3339()).as_deref(),
        Some("2026-10-01T09:30:00.123456+00:00")
    );
    let mut meta = store.get("A").await.unwrap().unwrap();
    assert_eq!(meta.labels["managed-by"], "tegata");
    meta.annotations.insert("tegata-gen-1".into(), "{}".into());
    store.update_meta("A", &meta).await.unwrap();
    assert_eq!(
        store.list().await.unwrap()["A"].annotations["tegata-gen-1"],
        "{}"
    );
    let readers = ["serviceAccount:a@x".to_owned()].into();
    store.set_readers("A", &readers).await.unwrap();
    assert_eq!(store.readers("A").await.unwrap(), readers);
    store.set_readers("A", &Default::default()).await.unwrap();
    assert!(store.readers("A").await.unwrap().is_empty());

    // 管理用の鍵の取り出し元：別のプロジェクトの Secret Manager（ここでは同じ偽物）
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tegata.yaml");
    std::fs::write(
        &path,
        r#"
stores: { main: { local: { dir: s } } }
credential_sources:
  admin: { gcp: { project: proj } }
providers:
  p:
    command: x
    credentials:
      latest: { from: { source: admin, secret: A } }
      pinned: { from: { source: admin, secret: A, version: 1 } }
resources: {}
"#,
    )
    .unwrap();
    let loaded = tegata::config::Loaded::load(&path).unwrap();
    let got = tegata::credentials::resolve(&loaded, "p").await.unwrap();
    assert_eq!(
        got["latest"], "v1 with ünïcode",
        "the latest enabled version"
    );
    assert_eq!(got["pinned"], "v1 with ünïcode");
    std::fs::write(
        &path,
        std::fs::read_to_string(&path)
            .unwrap()
            .replace("secret: A, version: 1", "secret: MISSING"),
    )
    .unwrap();
    let loaded = tegata::config::Loaded::load(&path).unwrap();
    let e = format!(
        "{:#}",
        tegata::credentials::resolve(&loaded, "p")
            .await
            .unwrap_err()
    );
    assert!(e.contains("credential pinned of provider p"), "{e}");
    assert!(
        e.contains("cannot read secret MISSING in project proj"),
        "{e}"
    );

    // VM の形：プロジェクトと鍵をメタデータのサーバーから取って、雛形を埋める
    std::env::remove_var("TEGATA_GCP_ACCESS_TOKEN");
    std::env::set_var("TEGATA_GCP_METADATA", format!("http://{addr}"));
    let vm = GcpStore::new(None, None).await.unwrap();
    assert_eq!(vm.project(), "proj");
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("tpl")).unwrap();
    std::fs::write(
        tmp.path().join("tpl/x.env"),
        "A=__A__\nB=__B__\nH=__A|sha256__\n",
    )
    .unwrap();
    let r = tegata::render::render_dir(&vm, &tmp.path().join("tpl"), &tmp.path().join("out"))
        .await
        .unwrap();
    assert_eq!(r.missing.into_iter().collect::<Vec<_>>(), vec!["B"]);
    let out = std::fs::read_to_string(tmp.path().join("out/x.env")).unwrap();
    assert!(out.starts_with("A=v1 with ünïcode\nB=__B__\nH="));
}
