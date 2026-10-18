//! 届け先（GitHub Actions の secrets）：偽の GitHub に届け、届いた値を試験の秘密の鍵で開いて確かめる

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use base64::Engine as _;
use tegata::config::Loaded;
use tegata::deliver::github::GithubAccess;
use tegata::engine::{Approval, Engine};
use tegata::store::local::LocalStore;
use tegata::store::Store;

/// 偽の GitHub が持つもの：場所（`o/r` か `o/r@環境`）→ 名前 → (値, 変えた時刻)
#[derive(Default)]
struct Fake {
    secrets: BTreeMap<String, BTreeMap<String, (String, String)>>,
    clock: u32,
    puts: u32,
}

impl Fake {
    fn tick(&mut self) -> String {
        self.clock += 1;
        format!(
            "2026-01-01T00:{:02}:{:02}Z",
            self.clock / 60,
            self.clock % 60
        )
    }

    fn value(&self, place: &str, name: &str) -> Option<String> {
        self.secrets.get(place)?.get(name).map(|(v, _)| v.clone())
    }

    /// GitHub の画面で手で書き換えたことにする
    fn set_by_hand(&mut self, place: &str, name: &str, value: &str) {
        let at = self.tick();
        self.secrets
            .entry(place.to_owned())
            .or_default()
            .insert(name.to_owned(), (value.to_owned(), at));
    }
}

type Shared = Arc<Mutex<Fake>>;

/// `/repos/o/r/actions/secrets[/name]` と `/repos/o/r/environments/e/secrets[/name]` → (場所, 名前)
fn place_of(path: &str) -> Option<(String, Option<String>)> {
    let parts: Vec<String> = path
        .trim_start_matches('/')
        .split('/')
        .map(percent_decode)
        .collect();
    let (place, rest) = match parts.as_slice() {
        [r, o, n, a, s, rest @ ..] if r == "repos" && a == "actions" && s == "secrets" => {
            (format!("{o}/{n}"), rest)
        }
        [r, o, n, e, env, s, rest @ ..]
            if r == "repos" && e == "environments" && s == "secrets" =>
        {
            (format!("{o}/{n}@{env}"), rest)
        }
        _ => return None,
    };
    match rest {
        [] => Some((place, None)),
        [name] => Some((place, Some(name.clone()))),
        _ => None,
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let hex = s
            .get(i + 1..i + 3)
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

async fn serve(state: Shared, secret_key: crypto_box::SecretKey) -> String {
    let public =
        base64::engine::general_purpose::STANDARD.encode(secret_key.public_key().as_bytes());
    let app = axum::Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let state = state.clone();
            let secret_key = secret_key.clone();
            let public = public.clone();
            async move {
                if headers.get("authorization").and_then(|v| v.to_str().ok())
                    != Some("Bearer test-token")
                {
                    return (StatusCode::UNAUTHORIZED, String::new());
                }
                let Some((place, name)) = place_of(uri.path()) else {
                    return (StatusCode::NOT_FOUND, String::new());
                };
                let mut s = state.lock().unwrap();
                match (method, name.as_deref()) {
                    (Method::GET, Some("public-key")) => (
                        StatusCode::OK,
                        serde_json::json!({ "key_id": "kid-1", "key": public }).to_string(),
                    ),
                    (Method::GET, None) => {
                        let list: Vec<_> = s
                            .secrets
                            .get(&place)
                            .into_iter()
                            .flatten()
                            .map(|(n, (_, at))| serde_json::json!({ "name": n, "created_at": at, "updated_at": at }))
                            .collect();
                        (
                            StatusCode::OK,
                            serde_json::json!({ "total_count": list.len(), "secrets": list }).to_string(),
                        )
                    }
                    (Method::PUT, Some(name)) => {
                        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(v["key_id"], "kid-1");
                        let sealed = base64::engine::general_purpose::STANDARD
                            .decode(v["encrypted_value"].as_str().unwrap())
                            .unwrap();
                        let value = String::from_utf8(secret_key.unseal(&sealed).unwrap()).unwrap();
                        s.puts += 1;
                        let at = s.tick();
                        let existed = s
                            .secrets
                            .entry(place)
                            .or_default()
                            .insert(name.to_ascii_uppercase(), (value, at))
                            .is_some();
                        let status = if existed { StatusCode::NO_CONTENT } else { StatusCode::CREATED };
                        (status, String::new())
                    }
                    (Method::DELETE, Some(name)) => {
                        let gone = s
                            .secrets
                            .get_mut(&place)
                            .and_then(|m| m.remove(&name.to_ascii_uppercase()))
                            .is_none();
                        let status = if gone { StatusCode::NOT_FOUND } else { StatusCode::NO_CONTENT };
                        (status, String::new())
                    }
                    _ => (StatusCode::NOT_FOUND, String::new()),
                }
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

const RESOURCES: &str = r#"
store: { local: { dir: store } }
resources:
  key: { provider: random, secrets: { KEY: value }, previous: KEY_PREV }
  token: { provider: random, secrets: { TOKEN: value } }
"#;

fn load(dir: &Path, destinations: &str) -> Loaded {
    std::fs::write(
        dir.join("tegata.yaml"),
        format!("{RESOURCES}{destinations}"),
    )
    .unwrap();
    Loaded::load(&dir.join("tegata.yaml")).unwrap()
}

const BOTH: &str = r#"
destinations:
  ci:
    github: { repo: o/r }
    secrets: [KEY, KEY_PREV]
  prod:
    github: { repo: o/r, environment: "production env" }
    secrets: { DEPLOY_TOKEN: TOKEN }
"#;

#[tokio::test]
async fn delivers_to_github_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let fake: Shared = Arc::default();
    let base = serve(
        fake.clone(),
        crypto_box::SecretKey::generate(&mut crypto_box::aead::OsRng),
    )
    .await;
    let access = GithubAccess {
        api: base,
        token: Some("test-token".to_owned()),
    };
    let store = LocalStore::new(dir.join("store"));
    // GitHub にもとからあるもの：同じ名前（KEY）と、tegata と関係のないもの（OTHER）
    fake.lock()
        .unwrap()
        .set_by_hand("o/r", "KEY", "set by hand before tegata");
    fake.lock().unwrap().set_by_hand("o/r", "OTHER", "not ours");
    let get = |place: &str, name: &str| fake.lock().unwrap().value(place, name);
    let puts = || fake.lock().unwrap().puts;

    // apply：作って届ける。もとからある同じ名前は上書きする。関係のないものは触らない
    let loaded = load(dir, BOTH);
    let engine = Engine::new(&loaded, &store).with_github(access.clone());
    engine.apply().await.unwrap();
    let key = store.access("KEY", None).await.unwrap().unwrap();
    assert_eq!(get("o/r", "KEY").as_deref(), Some(key.as_str()));
    assert_eq!(get("o/r", "KEY_PREV").as_deref(), Some(key.as_str()));
    let token = store.access("TOKEN", None).await.unwrap().unwrap();
    assert_eq!(get("o/r@production env", "DEPLOY_TOKEN"), Some(token));
    assert_eq!(get("o/r", "OTHER").as_deref(), Some("not ours"));
    assert_eq!(puts(), 3);
    engine.status().await.unwrap();

    // もう一度 apply：届いているので、書かない
    engine.apply().await.unwrap();
    assert_eq!(puts(), 3);

    // rotate：新しい値を届ける。previous には前の値
    engine.rotate(&["key".to_owned()]).await.unwrap();
    let new_key = store.access("KEY", None).await.unwrap().unwrap();
    assert_ne!(new_key, key);
    assert_eq!(get("o/r", "KEY").as_deref(), Some(new_key.as_str()));
    assert_eq!(get("o/r", "KEY_PREV").as_deref(), Some(key.as_str()));
    // settle：previous も届け直す
    engine.settle(&["key".to_owned()]).await.unwrap();
    assert_eq!(get("o/r", "KEY_PREV").as_deref(), Some(new_key.as_str()));

    // GitHub で手で書き換えた・消したものは、apply が書き直す
    fake.lock()
        .unwrap()
        .set_by_hand("o/r", "KEY", "changed by hand");
    fake.lock()
        .unwrap()
        .secrets
        .get_mut("o/r")
        .unwrap()
        .remove("KEY_PREV");
    engine.status().await.unwrap();
    engine.apply().await.unwrap();
    assert_eq!(get("o/r", "KEY").as_deref(), Some(new_key.as_str()));
    assert_eq!(get("o/r", "KEY_PREV").as_deref(), Some(new_key.as_str()));

    // dry-run：何も変えない
    let before = puts();
    fake.lock()
        .unwrap()
        .set_by_hand("o/r", "KEY", "changed again");
    Engine::new(&loaded, &store)
        .with_github(access.clone())
        .with_approval(Approval::DryRun)
        .apply()
        .await
        .unwrap();
    assert_eq!(puts(), before);
    assert_eq!(get("o/r", "KEY").as_deref(), Some("changed again"));

    // 設定から外したもの（KEY_PREV と、届け先 prod ごと）は消す。関係のないものは残す
    let loaded = load(
        dir,
        "destinations:\n  ci:\n    github: { repo: o/r }\n    secrets: [KEY]\n",
    );
    let engine = Engine::new(&loaded, &store).with_github(access.clone());
    engine.apply().await.unwrap();
    assert_eq!(get("o/r", "KEY_PREV"), None);
    assert_eq!(get("o/r@production env", "DEPLOY_TOKEN"), None);
    assert_eq!(get("o/r", "OTHER").as_deref(), Some("not ours"));
    assert_eq!(get("o/r", "KEY").as_deref(), Some(new_key.as_str()));
    for secret in ["KEY_PREV", "TOKEN"] {
        let meta = store.get(secret).await.unwrap().unwrap();
        assert!(tegata::deliver::deliveries(&meta).is_empty(), "{secret}");
    }

    // 届け先の名前を替える（同じ秘密を別の名前で）：前の名前のものは消す
    let loaded = load(
        dir,
        "destinations:\n  ci:\n    github: { repo: o/r }\n    secrets: { APP_KEY: KEY }\n",
    );
    Engine::new(&loaded, &store)
        .with_github(access.clone())
        .apply()
        .await
        .unwrap();
    assert_eq!(get("o/r", "APP_KEY").as_deref(), Some(new_key.as_str()));
    assert_eq!(get("o/r", "KEY"), None);

    // 鍵がなければ、どの秘密も届かず、apply は失敗として終わる（値は置き場所に入っている）
    let loaded = load(dir, BOTH);
    let wrong = GithubAccess {
        token: Some("wrong".to_owned()),
        ..access
    };
    let err = Engine::new(&loaded, &store)
        .with_github(wrong)
        .apply()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("destinations"), "{err}");
}

#[test]
fn checks_destination_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let err = |dest: &str| {
        std::fs::write(
            dir.join("tegata.yaml"),
            format!("{RESOURCES}destinations:\n{dest}"),
        )
        .unwrap();
        format!("{:#}", Loaded::load(&dir.join("tegata.yaml")).unwrap_err())
    };
    assert!(
        err("  a: { github: { repo: o/r }, secrets: [NOPE] }\n").contains("no resource makes NOPE")
    );
    assert!(err("  a: { github: { repo: o }, secrets: [KEY] }\n").contains("owner/repo"));
    assert!(
        err("  a: { github: { repo: o/r }, secrets: { my-key: KEY } }\n")
            .contains("not a valid GitHub secret name")
    );
    assert!(
        err("  a: { github: { repo: o/r }, secrets: { GITHUB_KEY: KEY } }\n")
            .contains("not a valid GitHub secret name")
    );
    assert!(
        err("  a: { github: { repo: o/r }, secrets: { 1KEY: KEY } }\n")
            .contains("not a valid GitHub secret name")
    );
    assert!(
        err("  a: { github: { repo: o/r }, secrets: { key: KEY, KEY: TOKEN } }\n")
            .contains("listed twice")
    );
    assert!(
        err("  a: { github: { repo: o/r, environment: \"\" }, secrets: [KEY] }\n")
            .contains("environment is empty")
    );
}
