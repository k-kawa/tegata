//! tegata の流れ（check、apply、rotate、prune、settle、import、render）を、ディレクトリの置き場所と、
//! 偽の外のプロバイダー（シェルスクリプト）で確かめる

use std::path::Path;

use tegata::config::Loaded;
use tegata::engine::{generations, Engine};
use tegata::store::local::LocalStore;
use tegata::store::{latest_enabled, Store};

/// 偽のプロバイダー：thing（managed）は作るたびに id-N、s-N を返し（expires のファイルがあれば、その時刻を expires で返す）、消した state を deleted に、消す要求を last-delete に書く。
/// info（data）は data.json を返す
fn fake_provider(dir: &Path) {
    let d = dir.display();
    let script = format!(
        r#"#!/bin/sh
req=$(cat)
case "$req" in
  *'"op":"describe"'*) echo '{{"types":{{"thing":"managed","info":"data"}}}}' ;;
  *'"op":"create"'*)
    echo "$req" > "{d}/last-create"
    n=$(( $(cat "{d}/count" 2>/dev/null || echo 0) + 1 )); echo $n > "{d}/count"
    exp=""; if [ -f "{d}/expires" ]; then exp=",\"expires\":\"$(cat "{d}/expires")\""; fi
    echo "{{\"outputs\":{{\"id\":\"id-$n\",\"secret\":\"s-$n\"}},\"state\":{{\"id\":\"id-$n\"}}$exp}}" ;;
  *'"op":"delete"'*) echo "$req" > "{d}/last-delete"; echo "$req" | sed 's/.*,"state":{{"id":"\([^"]*\)"}}.*/\1/' >> "{d}/deleted"; echo '{{}}' ;;
  *'"op":"read"'*) cat "{d}/data.json" ;;
esac
"#
    );
    let path = dir.join("tegata-provider-fake");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn setup(dir: &Path) -> Loaded {
    fake_provider(dir);
    std::fs::write(dir.join("data.json"), r#"{"outputs":{"BUCKET":"b1"}}"#).unwrap();
    for (role, files) in [
        (
            "app",
            vec![
                (
                    "app.env",
                    "TOKEN=__APP_TOKEN__\nKEY=__KEY__\nPREV=__KEY_PREV__\nB=__BUCKET__\n",
                ),
                (
                    "control.env",
                    "HASH=__APP_TOKEN|sha256__\nID=__THING_ID__\nS=__THING_SECRET__\n",
                ),
                ("password", "__PW__"),
            ],
        ),
        (
            "monitor",
            vec![("monitor.env", "PW=__PW__\nB=__BUCKET__\nX=__NOT_MADE__\n")],
        ),
    ] {
        std::fs::create_dir_all(dir.join("tpl").join(role)).unwrap();
        for (f, t) in files {
            std::fs::write(dir.join("tpl").join(role).join(f), t).unwrap();
        }
    }
    let yaml = r#"
store: { local: { dir: store } }
roles:
  app: { member: "serviceAccount:app@x", templates: tpl/app }
  monitor: { member: "serviceAccount:mon@x", templates: tpl/monitor }
providers:
  fake:
    command: ./tegata-provider-fake
    config: { account: acc }
    credentials:
      token: { env: TEGATA_TEST_FAKE_TOKEN }
resources:
  app_token: { provider: random, config: { bytes: 16 }, secrets: { APP_TOKEN: value } }
  key: { provider: random, secrets: { KEY: value }, previous: KEY_PREV }
  pw: { provider: random, secrets: { PW: value } }
  thing:
    provider: fake
    type: thing
    config: { name: t }
    secrets: { THING_ID: id, THING_SECRET: secret }
  info: { provider: fake, type: info, secrets: { BUCKET: BUCKET } }
"#;
    std::fs::write(dir.join("tegata.yaml"), yaml).unwrap();
    Loaded::load(&dir.join("tegata.yaml")).unwrap()
}

#[tokio::test]
async fn manages_secrets_end_to_end() {
    std::env::set_var("TEGATA_TEST_FAKE_TOKEN", "tok-123");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let loaded = setup(dir);
    let store = LocalStore::new(dir.join("store"));
    let engine = Engine::new(&loaded, &store);

    // 雛形にあるのに作るものがない名前を知らせる
    let checked = engine.check(None).await.unwrap();
    assert_eq!(
        checked.problems,
        vec!["identity monitor: NOT_MADE is in the templates but no resource makes it"]
    );
    assert!(checked.warnings.is_empty(), "{:?}", checked.warnings);

    // apply：入れ物、読む権限、値
    engine.apply().await.unwrap();
    let token = store.access("APP_TOKEN", None).await.unwrap().unwrap();
    assert_eq!(token.len(), 32);
    assert_eq!(
        store.access("KEY_PREV", None).await.unwrap(),
        store.access("KEY", None).await.unwrap()
    );
    assert_eq!(
        store.access("THING_ID", None).await.unwrap().as_deref(),
        Some("id-1")
    );
    assert_eq!(
        store.access("BUCKET", None).await.unwrap().as_deref(),
        Some("b1")
    );
    // 鍵はプロバイダーに渡る
    assert!(std::fs::read_to_string(dir.join("last-create"))
        .unwrap()
        .contains("tok-123"));
    // 読む権限は雛形から：PW と BUCKET は両方、APP_TOKEN は app だけ
    let both: std::collections::BTreeSet<String> = ["serviceAccount:app@x", "serviceAccount:mon@x"]
        .map(String::from)
        .into();
    assert_eq!(store.readers("PW").await.unwrap(), both);
    assert_eq!(store.readers("BUCKET").await.unwrap(), both);
    assert_eq!(
        store.readers("APP_TOKEN").await.unwrap(),
        ["serviceAccount:app@x".to_owned()].into()
    );
    let meta = store.get("APP_TOKEN").await.unwrap().unwrap();
    assert_eq!(meta.labels["managed-by"], "tegata");
    assert_eq!(meta.labels["tegata-resource"], "app_token");

    // もう一度 apply しても、値は変わらない。data が変われば新しい版になり、前の版は無効になる
    std::fs::write(dir.join("data.json"), r#"{"outputs":{"BUCKET":"b2"}}"#).unwrap();
    engine.apply().await.unwrap();
    assert_eq!(
        store.access("APP_TOKEN", None).await.unwrap(),
        Some(token.clone())
    );
    assert_eq!(
        store.access("THING_ID", None).await.unwrap().as_deref(),
        Some("id-1")
    );
    assert_eq!(
        store.access("BUCKET", None).await.unwrap().as_deref(),
        Some("b2")
    );
    assert_eq!(
        store
            .versions("BUCKET")
            .await
            .unwrap()
            .iter()
            .filter(|v| v.enabled)
            .count(),
        1
    );

    // rotate：新しい世代。previous には前の値
    let old_key = store.access("KEY", None).await.unwrap().unwrap();
    engine
        .rotate(&["thing".into(), "key".into()])
        .await
        .unwrap();
    assert_eq!(
        store.access("THING_SECRET", None).await.unwrap().as_deref(),
        Some("s-2")
    );
    assert_eq!(
        store.access("KEY_PREV", None).await.unwrap(),
        Some(old_key.clone())
    );
    assert_ne!(store.access("KEY", None).await.unwrap(), Some(old_key));
    assert_eq!(
        generations(&store.get("THING_ID").await.unwrap().unwrap()).len(),
        2
    );

    // prune：古い世代のプロバイダーのものを消し、版を無効にする（消さない）
    engine.prune(&["thing".into()]).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("deleted")).unwrap().trim(),
        "id-1"
    );
    // 残す最新の世代の state を current_state で渡す（Tunnel のトークンのように、値が 1 つしか持てないもののため）
    assert!(std::fs::read_to_string(dir.join("last-delete"))
        .unwrap()
        .contains(r#""current_state":{"id":"id-2"}"#));
    assert_eq!(
        generations(&store.get("THING_ID").await.unwrap().unwrap()).len(),
        1
    );
    let versions = store.versions("THING_ID").await.unwrap();
    assert_eq!(versions.len(), 2);
    assert!(!versions.iter().find(|v| v.number == 1).unwrap().enabled);
    assert_eq!(latest_enabled(&store, "THING_ID").await.unwrap(), Some(2));

    // settle：previous を今の値に
    engine.settle(&["key".into()]).await.unwrap();
    assert_eq!(
        store.access("KEY_PREV", None).await.unwrap(),
        store.access("KEY", None).await.unwrap()
    );

    // render：雛形を埋める。sha256、1 行だけのファイル、ないものは残す
    let out = dir.join("out");
    let r = tegata::render::render_dir(&store, &dir.join("tpl/monitor"), &out)
        .await
        .unwrap();
    assert_eq!(r.missing.into_iter().collect::<Vec<_>>(), vec!["NOT_MADE"]);
    tegata::render::render_dir(&store, &dir.join("tpl/app"), &out)
        .await
        .unwrap();
    let control = std::fs::read_to_string(out.join("control.env")).unwrap();
    assert!(control.contains(&format!("HASH={}", tegata::template::sha256_hex(&token))));
    assert!(control.contains("S=s-2"));
    let pw = std::fs::read_to_string(out.join("password")).unwrap();
    assert_eq!(pw.len(), 64, "no newline is added to a single value file");
    assert!(std::fs::read_to_string(out.join("monitor.env"))
        .unwrap()
        .contains("X=__NOT_MADE__"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(out.join("app.env"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn imports_values_from_filled_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::env::set_var("TEGATA_TEST_FAKE_TOKEN", "tok-123");
    let loaded = setup(dir);
    let store = LocalStore::new(dir.join("store"));
    let engine = Engine::new(&loaded, &store);
    let filled = dir.join("filled");
    std::fs::create_dir_all(&filled).unwrap();
    std::fs::write(
        filled.join("app.env"),
        "TOKEN=old-token\nKEY=old-key\nB=old-bucket\n",
    )
    .unwrap();
    std::fs::write(
        filled.join("control.env"),
        "HASH=x\nID=old-id\nS=old-secret\n",
    )
    .unwrap();
    std::fs::write(filled.join("password"), "old-pw").unwrap();

    engine.import(&dir.join("tpl/app"), &filled).await.unwrap();
    assert_eq!(
        store.access("APP_TOKEN", None).await.unwrap().as_deref(),
        Some("old-token")
    );
    assert_eq!(
        store.access("THING_SECRET", None).await.unwrap().as_deref(),
        Some("old-secret")
    );
    assert_eq!(
        store.access("PW", None).await.unwrap().as_deref(),
        Some("old-pw")
    );
    // data（BUCKET）は import しない（apply が読む）。previous は apply が今の値で作る
    assert_eq!(store.access("BUCKET", None).await.unwrap(), None);
    // apply は、入れた値を変えない
    engine.apply().await.unwrap();
    assert_eq!(
        store.access("APP_TOKEN", None).await.unwrap().as_deref(),
        Some("old-token")
    );
    assert_eq!(
        store.access("KEY_PREV", None).await.unwrap().as_deref(),
        Some("old-key")
    );
    let gens = generations(&store.get("THING_ID").await.unwrap().unwrap());
    assert!(gens.values().next().unwrap().imported);

    // import した世代は、prune でプロバイダーに消させない
    engine.rotate(&["thing".into()]).await.unwrap();
    engine.prune(&["thing".into()]).await.unwrap();
    assert!(!dir.join("deleted").exists());
}

#[tokio::test]
async fn measures_the_age_of_generations() {
    use tegata::age::Standing;
    std::env::set_var("TEGATA_TEST_FAKE_TOKEN", "tok-123");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fake_provider(dir);
    std::fs::write(dir.join("data.json"), r#"{"outputs":{"BUCKET":"b1"}}"#).unwrap();
    let now = tegata::age::now();
    let expires = now + chrono::TimeDelta::days(30);
    std::fs::write(dir.join("expires"), expires.to_rfc3339()).unwrap();
    std::fs::write(
        dir.join("tegata.yaml"),
        r#"
stores: { main: { local: { dir: store } } }
defaults: { max_age: 180d, warn_before: 7d }
providers:
  fake:
    command: ./tegata-provider-fake
    credentials:
      token: { env: TEGATA_TEST_FAKE_TOKEN }
resources:
  key: { provider: random, stores: { main: [{ name: KEY }] }, max_age: 90d }
  thing:
    provider: fake
    type: thing
    stores: { main: [{ output: id, name: THING_ID }, { output: secret, name: THING_SECRET }] }
  info: { provider: fake, type: info, stores: { main: [{ output: BUCKET, name: BUCKET }] }, max_age: 1d }
"#,
    )
    .unwrap();
    let loaded = Loaded::load(&dir.join("tegata.yaml")).unwrap();
    let store = LocalStore::new(dir.join("store"));
    let engine = Engine::new(&loaded, &store);
    let checked = engine.check(None).await.unwrap();
    assert_eq!(
        checked.warnings,
        vec!["resource info: max_age has no effect, since fake info is read, not made"]
    );
    engine.apply().await.unwrap();

    // The generation records when it was made, and the expiry the provider returned
    let gens = generations(&store.get("THING_ID").await.unwrap().unwrap());
    let g = &gens[&1];
    assert!(g.created.is_some_and(|c| c >= now));
    assert_eq!(g.expires.map(|e| e.timestamp()), Some(expires.timestamp()));

    let (l, st) = (&loaded, &store);
    let at = move |days: i64| async move {
        let stale = Engine::new(l, st)
            .with_now(now + chrono::TimeDelta::days(days))
            .stale()
            .await
            .unwrap();
        stale
            .into_iter()
            .map(|s| (s.resource, s.standing))
            .collect::<Vec<_>>()
    };
    // Data resources never go stale
    assert_eq!(
        at(0).await,
        vec![("key".into(), Standing::Ok), ("thing".into(), Standing::Ok)]
    );
    // Expiring within warn_before
    assert_eq!(
        at(25).await,
        vec![
            ("key".into(), Standing::Ok),
            ("thing".into(), Standing::Due)
        ]
    );
    assert_eq!(
        at(85).await,
        vec![
            ("key".into(), Standing::Due),
            ("thing".into(), Standing::Expired)
        ]
    );
    assert_eq!(at(91).await[0], ("key".into(), Standing::RotateNow));

    // A generation recorded without a time is as old as its version
    let mut meta = store.get("KEY").await.unwrap().unwrap();
    let mut g: serde_json::Value = serde_json::from_str(&meta.annotations["tegata-gen-1"]).unwrap();
    g.as_object_mut().unwrap().remove("created");
    meta.annotations
        .insert("tegata-gen-1".into(), g.to_string());
    store.update_meta("KEY", &meta).await.unwrap();
    let version = store.versions("KEY").await.unwrap()[0].created.unwrap();
    let stale = engine.stale().await.unwrap();
    assert_eq!(stale[0].resource, "key");
    assert_eq!(stale[0].created, version);
    assert_eq!(stale[0].max_age.as_deref(), Some("90d"));
    assert_eq!(stale[0].command, "tegata rotate key");

    // A new generation starts again
    engine.rotate(&["key".into()]).await.unwrap();
    let stale = engine
        .with_now(now + chrono::TimeDelta::days(91))
        .stale()
        .await
        .unwrap();
    assert_eq!(stale[0].generation, 2);
}
