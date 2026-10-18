//! CLI：値を標準入力から入れる（set）、previous、render

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn tegata(dir: &std::path::Path, args: &[&str], stdin: Option<&str>) -> (bool, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tegata"))
        .arg("-f")
        .arg(dir.join("tegata.yaml"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(s) = stdin {
        input.write_all(s.as_bytes()).unwrap();
    }
    drop(input);
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn sets_values_from_stdin() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("tpl")).unwrap();
    std::fs::write(
        dir.join("tpl/a.env"),
        "T=__TUNNEL__\nK=__KEY__\nP=__KEY_PREV__\nF=__FROM__\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("tegata.yaml"),
        r#"
store: { local: { dir: store } }
roles: { app: { member: "serviceAccount:a@x", templates: tpl } }
resources:
  tunnel: { provider: manual, secrets: { TUNNEL: value } }
  key: { provider: random, secrets: { KEY: value }, previous: KEY_PREV }
  fixed: { provider: value, config: { from: "Hako <a@b>" }, secrets: { FROM: from } }
"#,
    )
    .unwrap();

    // apply は手で入れる値を聞かない（あとで set）
    let (ok, out) = tegata(dir, &["apply"], None);
    assert!(ok, "{out}");
    assert!(
        out.contains("tunnel: no value yet (put it with `tegata set tunnel`)"),
        "{out}"
    );

    let (ok, out) = tegata(dir, &["set", "tunnel"], Some("tok\n"));
    assert!(ok, "{out}");
    // 控えておいた鍵を戻す：今の値は previous に、入れた値が今の値に
    let (ok, out) = tegata(dir, &["set", "key"], Some("restored-key"));
    assert!(ok, "{out}");
    assert!(
        !tegata(dir, &["set", "tunnel"], Some("")).0,
        "an empty value is refused"
    );

    let (ok, out) = tegata(
        dir,
        &[
            "render",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
            dir.join("tpl").to_str().unwrap(),
            dir.join("out").to_str().unwrap(),
        ],
        None,
    );
    assert!(ok, "{out}");
    let a = std::fs::read_to_string(dir.join("out/a.env")).unwrap();
    assert!(a.starts_with("T=tok\nK=restored-key\nP="), "{a}");
    assert!(
        !a.contains("P=restored-key"),
        "previous keeps the generated key until settle"
    );
    let (ok, _) = tegata(dir, &["settle", "key"], None);
    assert!(ok);
    tegata(
        dir,
        &[
            "render",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
            dir.join("tpl").to_str().unwrap(),
            dir.join("out").to_str().unwrap(),
        ],
        None,
    );
    let a = std::fs::read_to_string(dir.join("out/a.env")).unwrap();
    assert!(a.contains("P=restored-key"));
    assert!(a.contains("F=Hako <a@b>"), "{a}");

    // リソースの名前を替えても、秘密は引き継がれる（値はそのまま）
    let yaml = std::fs::read_to_string(dir.join("tegata.yaml")).unwrap();
    std::fs::write(
        dir.join("tegata.yaml"),
        yaml.replace("  tunnel: {", "  tunnel_token: {"),
    )
    .unwrap();
    let (ok, out) = tegata(dir, &["apply"], None);
    assert!(ok, "{out}");
    assert!(
        out.contains("secret TUNNEL: moved from resource tunnel to tunnel_token"),
        "{out}"
    );
    std::fs::write(dir.join("tegata.yaml"), yaml).unwrap();
    let (ok, _) = tegata(dir, &["apply"], None);
    assert!(ok);

    let (ok, out) = tegata(dir, &["status"], None);
    assert!(ok);
    assert!(out.contains("tunnel (manual) generation 1"), "{out}");
    assert!(
        out.contains("key (random) generation 2 (+1 to prune)"),
        "{out}"
    );
}

/// 作ると前の値がすぐ効かなくなる型（replaces）は、端末で yes を求める。端末でなければ --yes が要る。--dry-run は何も変えない
#[test]
fn confirms_before_replacing_values() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let d = dir.display();
    let script = format!(
        r#"#!/bin/sh
req=$(cat)
case "$req" in
  *'"op":"describe"'*) echo '{{"types":{{"user":"managed","token":"managed"}},"replaces":["user"]}}' ;;
  *'"op":"create"'*)
    n=$(( $(cat "{d}/count" 2>/dev/null || echo 0) + 1 )); echo $n > "{d}/count"
    echo "{{\"outputs\":{{\"password\":\"p-$n\"}},\"state\":{{\"n\":$n}}}}" ;;
  *) echo '{{}}' ;;
esac
"#
    );
    std::fs::write(dir.join("tegata-provider-fake"), script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.join("tegata-provider-fake"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    std::fs::create_dir_all(dir.join("tpl")).unwrap();
    std::fs::write(dir.join("tpl/a.env"), "P=__DB_PASSWORD__\nT=__TOKEN__\n").unwrap();
    std::fs::write(
        dir.join("tegata.yaml"),
        r#"
store: { local: { dir: store } }
roles: { app: { member: "serviceAccount:a@x", templates: tpl } }
providers: { fake: { command: ./tegata-provider-fake } }
resources:
  db_password: { provider: fake, type: user, config: { user: control }, secrets: { DB_PASSWORD: password } }
  token: { provider: fake, type: token, secrets: { TOKEN: password } }
"#,
    )
    .unwrap();
    let count = || {
        std::fs::read_to_string(dir.join("count"))
            .map(|s| s.trim().parse::<u32>().unwrap())
            .unwrap_or(0)
    };

    // dry-run：作るものを出すだけ
    let (ok, out) = tegata(dir, &["apply", "--dry-run"], None);
    assert!(ok, "{out}");
    assert!(
        out.contains(
            "would make a new generation of db_password (the old value stops working at once)"
        ),
        "{out}"
    );
    assert!(
        out.contains("would make a new generation of token\n"),
        "{out}"
    );
    assert!(out.contains("read by: app (serviceAccount:a@x)"), "{out}");
    assert_eq!(count(), 0);
    assert!(
        !dir.join("store").exists()
            || std::fs::read_dir(dir.join("store"))
                .unwrap()
                .next()
                .is_none()
    );

    // 端末でなければ、--yes がないと何もせずに断る（ほかのリソースも作らない）
    let (ok, out) = tegata(dir, &["apply"], Some("yes\n"));
    assert!(!ok);
    assert!(
        out.contains("db_password would stop working at once"),
        "{out}"
    );
    assert!(out.contains("--yes"), "{out}");
    assert_eq!(count(), 0);

    let (ok, out) = tegata(dir, &["apply", "--yes"], None);
    assert!(ok, "{out}");
    assert_eq!(count(), 2);

    // 値があれば apply は作らないので、聞かない
    let (ok, out) = tegata(dir, &["apply"], None);
    assert!(ok, "{out}");
    let (ok, out) = tegata(dir, &["apply", "--dry-run"], None);
    assert!(ok && out.contains("no new generation to make"), "{out}");

    // rotate：前の値がすぐ効かなくなるものだけ確かめる
    let (ok, out) = tegata(dir, &["rotate", "token"], None);
    assert!(ok, "{out}");
    assert_eq!(count(), 3);
    let (ok, _) = tegata(dir, &["rotate", "token", "db_password"], None);
    assert!(!ok);
    assert_eq!(count(), 3, "nothing is made when the confirmation fails");
    let (ok, _) = tegata(dir, &["rotate", "--dry-run", "db_password"], None);
    assert!(ok);
    assert_eq!(count(), 3);
    let (ok, out) = tegata(dir, &["rotate", "--yes", "db_password"], None);
    assert!(ok, "{out}");
    assert_eq!(count(), 4);
    assert!(!tegata(dir, &["rotate", "--yes", "--dry-run", "db_password"], None).0);
}

/// examples/minimal の README の手順が、そのとおりに動く
#[test]
fn the_minimal_example_works() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/minimal");
    std::fs::create_dir_all(dir.join("templates/app")).unwrap();
    for f in ["tegata.yaml", "templates/app/app.env"] {
        std::fs::copy(example.join(f), dir.join(f)).unwrap();
    }
    let render = |dir: &Path| {
        let (ok, out) = tegata(
            dir,
            &[
                "render",
                "--store-dir",
                dir.join("store").to_str().unwrap(),
                dir.join("templates/app").to_str().unwrap(),
                dir.join("out").to_str().unwrap(),
            ],
            None,
        );
        assert!(ok, "{out}");
        std::fs::read_to_string(dir.join("out/app.env")).unwrap()
    };
    let line = |text: &str, key: &str| -> String {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap()
            .to_owned()
    };

    for (args, stdin) in [
        (vec!["check"], None),
        (vec!["apply"], None),
        (vec!["set", "admin_password"], Some("correct horse\n")),
        (vec!["status"], None),
    ] {
        let (ok, out) = tegata(dir, &args, stdin);
        assert!(ok, "{args:?}: {out}");
    }
    let first = render(dir);
    let key = line(&first, "SESSION_KEY");
    assert_eq!(key.len(), 64, "{first}");
    assert_eq!(line(&first, "PREVIOUS_SESSION_KEY"), key);
    assert_eq!(
        line(&first, "ADMIN_PASSWORD_SHA256"),
        tegata::template::sha256_hex("correct horse")
    );
    assert_eq!(line(&first, "MAIL_FROM"), "Example <noreply@example.com>");

    let (ok, out) = tegata(dir, &["rotate", "session_key"], None);
    assert!(ok, "{out}");
    let rotated = render(dir);
    assert_ne!(line(&rotated, "SESSION_KEY"), key);
    assert_eq!(line(&rotated, "PREVIOUS_SESSION_KEY"), key);
    for args in [
        vec!["settle", "session_key"],
        vec!["prune", "session_key"],
        vec!["status"],
    ] {
        let (ok, out) = tegata(dir, &args, None);
        assert!(ok, "{args:?}: {out}");
    }
    let settled = render(dir);
    assert_eq!(
        line(&settled, "PREVIOUS_SESSION_KEY"),
        line(&settled, "SESSION_KEY")
    );
}
