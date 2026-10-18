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

    let templates = dir.join("templates/app");
    for (args, stdin) in [
        (vec!["check"], None),
        (vec!["check", "app", templates.to_str().unwrap()], None),
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

/// Help and errors point to the documentation, and every page and section they name exists
#[test]
fn help_and_errors_link_to_the_docs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    const BASE: &str = "https://github.com/k-kawa/tegata/blob/main/docs/";
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let check_links = |out: &str| {
        let links: Vec<&str> = out
            .split_whitespace()
            .filter_map(|w| w.strip_prefix(BASE))
            .collect();
        assert!(!links.is_empty(), "{out}");
        for link in links {
            let (page, section) = link.split_once('#').unwrap_or((link, ""));
            let text = std::fs::read_to_string(docs.join(page))
                .unwrap_or_else(|_| panic!("no page {page}"));
            if !section.is_empty() {
                let heading = format!("\n## {}\n", section.replace('-', " "));
                assert!(text.contains(&heading), "no section {link}");
            }
        }
    };

    let (ok, out) = tegata(dir, &["--help"], None);
    assert!(ok, "{out}");
    assert!(
        out.contains("Repository: https://github.com/k-kawa/tegata"),
        "{out}"
    );
    check_links(&out);
    for command in [
        "init", "check", "status", "apply", "rotate", "set", "settle", "prune", "import",
        "migrate", "env", "render",
    ] {
        let (ok, out) = tegata(dir, &[command, "--help"], None);
        assert!(ok, "{out}");
        assert!(
            out.contains(&format!("commands.md#tegata-{command}")),
            "{out}"
        );
        check_links(&out);
    }

    // The configuration file cannot be read: the configuration reference
    let (ok, out) = tegata(dir, &["status"], None);
    assert!(!ok);
    assert!(out.contains("see: https://"), "{out}");
    assert!(out.contains("reference/configuration.md"), "{out}");
    check_links(&out);

    // A command fails: its section of the command reference
    std::fs::write(
        dir.join("tegata.yaml"),
        "store: { local: { dir: store } }\nresources:\n  k: { provider: random, secrets: { K: value } }\n",
    )
    .unwrap();
    let (ok, out) = tegata(dir, &["settle", "k"], None);
    assert!(!ok);
    assert!(out.contains("error: k has no previous"), "{out}");
    assert!(out.contains("commands.md#tegata-settle"), "{out}");
    check_links(&out);
}

/// tegata migrate prints the current form, and apply after migrating changes nothing
#[test]
fn migrating_changes_nothing_in_the_store() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("web")).unwrap();
    std::fs::create_dir_all(dir.join("worker")).unwrap();
    std::fs::write(
        dir.join("web/app.env"),
        "K=__KEY__\nP=__KEY_PREV__\nM=__MAIL__\n",
    )
    .unwrap();
    std::fs::write(dir.join("worker/app.env"), "M=__MAIL__\n").unwrap();
    std::fs::write(
        dir.join("tegata.yaml"),
        r#"
store: { local: { dir: store } }
roles:
  web: { member: "serviceAccount:web@x", templates: web }
  worker: { member: "serviceAccount:worker@x", templates: worker }
resources:
  key: { provider: random, secrets: { KEY: value }, previous: KEY_PREV }
  mail: { provider: value, config: { from: "a@b" }, secrets: { MAIL: from } }
  unused: { provider: manual, secrets: { UNUSED: value } }
"#,
    )
    .unwrap();
    let (ok, out) = tegata(dir, &["apply"], None);
    assert!(ok, "{out}");
    assert!(out.contains("uses the old form"), "{out}");
    let (_, before) = tegata(dir, &["status"], None);

    let (ok, migrated) = tegata(dir, &["migrate"], None);
    assert!(ok, "{migrated}");
    std::fs::write(dir.join("tegata.yaml"), &migrated).unwrap();
    let (ok, out) = tegata(dir, &["apply"], None);
    assert!(ok, "{out}");
    assert!(
        !out.contains("readers of") && !out.contains("created") && !out.contains("version"),
        "apply after migrate changed something:\n{out}\n{migrated}"
    );
    assert!(!out.contains("old form"), "{out}");
    let (_, after) = tegata(dir, &["status"], None);
    let without_warnings = |s: &str| {
        s.lines()
            .filter(|l| !l.starts_with("warning:"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(without_warnings(&after), without_warnings(&before));
    let (ok, out) = tegata(dir, &["check"], None);
    assert!(ok && out.ends_with("ok\n"), "{out}");
}

/// In the current form, readers decide access, and check compares them with templates it is given
#[test]
fn readers_decide_access_and_check_compares_templates() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("edge")).unwrap();
    std::fs::write(dir.join("edge/tunnel.env"), "T=__TUNNEL__\nK=__KEY__\n").unwrap();
    std::fs::write(
        dir.join("tegata.yaml"),
        r#"
stores:
  main: { local: { dir: store } }
identities:
  app: [serviceAccount:app-1@x, serviceAccount:app-2@x]
  edge: [serviceAccount:edge@x]
resources:
  key:
    provider: random
    stores:
      main:
        - { name: KEY, readers: [app] }
    previous: { name: KEY_PREV }
  tunnel:
    provider: manual
    stores:
      main:
        - { name: TUNNEL, readers: [edge] }
  mail:
    provider: value
    config: { from: "a@b" }
    stores:
      main:
        - { output: from, name: MAIL, readers: [app, edge] }
"#,
    )
    .unwrap();
    // Without templates, check looks at the configuration and providers only
    let (ok, out) = tegata(dir, &["check"], None);
    assert!(ok && out.ends_with("ok\n"), "{out}");
    // edge's template uses KEY, which only app reads; and edge reads MAIL, which it does not use
    let edge = dir.join("edge");
    let (ok, out) = tegata(dir, &["check", "edge", edge.to_str().unwrap()], None);
    assert!(!ok, "{out}");
    assert!(
        out.contains("identity edge: KEY is in the templates but edge cannot read it (add edge to its readers)"),
        "{out}"
    );
    assert!(
        out.contains("warning: identity edge: can read MAIL, which its templates do not use"),
        "{out}"
    );

    let (ok, out) = tegata(dir, &["apply"], None);
    assert!(ok, "{out}");
    // every member of every reader, and previous read like the value it copies
    assert!(
        out.contains("readers of KEY: serviceAccount:app-1@x, serviceAccount:app-2@x"),
        "{out}"
    );
    assert!(
        out.contains("readers of KEY_PREV: serviceAccount:app-1@x, serviceAccount:app-2@x"),
        "{out}"
    );
    assert!(
        out.contains("readers of MAIL: serviceAccount:app-1@x, serviceAccount:app-2@x, serviceAccount:edge@x"),
        "{out}"
    );
    let (ok, out) = tegata(dir, &["status"], None);
    assert!(ok, "{out}");
    assert!(
        out.lines()
            .any(|l| l.contains("MAIL") && l.trim_end().ends_with("app,edge")),
        "{out}"
    );
    assert!(
        out.lines()
            .any(|l| l.contains("TUNNEL") && l.trim_end().ends_with("edge")),
        "{out}"
    );

    let (ok, out) = tegata(dir, &["check", "nobody", edge.to_str().unwrap()], None);
    assert!(!ok);
    assert!(out.contains("no identity nobody in the config"), "{out}");
    let (ok, _) = tegata(dir, &["check", "edge"], None);
    assert!(!ok, "an identity needs templates");

    // import takes the values out of files filled from the templates
    let filled = dir.join("filled");
    std::fs::create_dir_all(&filled).unwrap();
    std::fs::write(filled.join("tunnel.env"), "T=tunnel-token\nK=ignored\n").unwrap();
    let (ok, out) = tegata(
        dir,
        &["import", edge.to_str().unwrap(), filled.to_str().unwrap()],
        None,
    );
    assert!(ok, "{out}");
    assert!(out.contains("TUNNEL: version 1 (imported)"), "{out}");
    assert!(out.contains("KEY: already has a value; skipped"), "{out}");
}

/// Environments: -e chooses one, each is labeled, check looks at all of them, and an environment
/// with confirm asks before every command that changes the store
#[test]
fn environments_are_chosen_labeled_checked_and_confirmed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("envs")).unwrap();
    std::fs::create_dir_all(dir.join("tpl")).unwrap();
    std::fs::write(dir.join("tpl/app.env"), "K=__KEY__\nM=__MAIL__\n").unwrap();
    let shared = r#"
stores:
  main: { local: { dir: shared } }
identities:
  app: []
environments:
  staging: envs/staging.yaml
  production:
    confirm: true
    stores: { main: { local: { dir: prod } } }
    identities: { app: [serviceAccount:app@prod] }
resources:
  key:
    provider: random
    stores: { main: [{ name: KEY, readers: [app] }] }
    previous: { name: KEY_PREV }
  mail:
    provider: manual
    stores: { main: [{ name: MAIL, readers: [app] }] }
"#;
    std::fs::write(dir.join("tegata.yaml"), shared).unwrap();
    std::fs::write(
        dir.join("envs/staging.yaml"),
        "stores: { main: { local: { dir: stg } } }\nidentities: { app: [serviceAccount:app@stg] }\n",
    )
    .unwrap();
    let env = |e: &str, args: &[&str], stdin: Option<&str>| {
        let mut all = vec!["-e", e];
        all.extend_from_slice(args);
        tegata(dir, &all, stdin)
    };

    let (ok, out) = tegata(dir, &["status"], None);
    assert!(!ok);
    assert!(
        out.contains("choose one with -e (staging, production)"),
        "{out}"
    );
    let (ok, out) = tegata(dir, &["migrate"], None);
    assert!(!ok && out.contains("current form already"), "{out}");
    let (ok, out) = tegata(dir, &["init"], None);
    assert!(
        ok && out.contains("no providers with a source"),
        "init needs no -e: {out}"
    );

    // staging: no confirm; secrets are labeled with their environment
    let (ok, out) = env("staging", &["apply"], None);
    assert!(ok, "{out}");
    assert!(
        out.contains("readers of KEY: serviceAccount:app@stg"),
        "{out}"
    );
    let key = std::fs::read_to_string(dir.join("stg/KEY.json")).unwrap();
    assert!(key.contains("\"tegata-env\": \"staging\""), "{key}");

    // production asks; outside a terminal it needs --yes, for every command that changes the store
    let (ok, out) = env("production", &["apply"], None);
    assert!(!ok);
    assert!(out.contains("production: apply the configuration"), "{out}");
    assert!(out.contains("pass --yes"), "{out}");
    assert!(!dir.join("prod").exists(), "nothing was written");
    let (ok, out) = env("production", &["apply", "--dry-run"], None);
    assert!(ok, "dry-run changes nothing, so it does not ask: {out}");
    let (ok, out) = env("production", &["apply", "--yes"], None);
    assert!(ok, "{out}");
    let tpl = dir.join("tpl");
    let tpl = tpl.to_str().unwrap();
    for (args, stdin) in [
        (vec!["set", "mail"], Some("m@x")),
        (vec!["rotate", "key"], None),
        (vec!["settle", "key"], None),
        (vec!["prune"], None),
        (vec!["import", tpl, tpl], None),
    ] {
        let (ok, out) = env("production", &args, stdin);
        assert!(!ok && out.contains("pass --yes"), "{args:?}: {out}");
        let mut yes = args.clone();
        yes.push("--yes");
        let (ok, out) = env("production", &yes, stdin);
        assert!(ok, "{yes:?}: {out}");
        assert!(out.contains("production: "), "says what it does: {out}");
    }
    let (ok, out) = env("production", &["status"], None);
    assert!(
        ok && !out.contains("production:"),
        "read-only commands never ask: {out}"
    );

    // check: every environment, and environments that share a member
    let (ok, out) = tegata(dir, &["check", "app", tpl], None);
    assert!(ok && out.contains("ok (staging, production)"), "{out}");
    let (ok, out) = tegata(dir, &["env", "production"], None);
    assert!(ok && out.contains("dir: prod"), "{out}");
    assert!(
        out.starts_with("# confirm: true"),
        "shown, but as a comment: {out}"
    );
    let merged = dir.join("merged");
    std::fs::create_dir_all(&merged).unwrap();
    std::fs::write(merged.join("tegata.yaml"), &out).unwrap();
    let (ok, out) = tegata(&merged, &["migrate"], None);
    assert!(ok, "the output loads as a configuration: {out}");
    std::fs::write(
        dir.join("envs/staging.yaml"),
        "stores: { main: { local: { dir: prod } } }\nidentities: { app: [serviceAccount:app@prod] }\n",
    )
    .unwrap();
    let (ok, out) = tegata(dir, &["check"], None);
    assert!(!ok);
    assert!(
        out.contains(
            "environments staging and production both give access to serviceAccount:app@prod"
        ),
        "{out}"
    );
    // a store that holds another environment's secrets is left alone
    let (ok, out) = env("staging", &["apply"], None);
    assert!(!ok);
    assert!(
        out.contains("secret KEY belongs to environment production"),
        "{out}"
    );
}

/// render writes only what changed, runs --on-change once when something did, can say so in its
/// exit status, and with --fail-on-missing writes nothing when a name is missing
#[test]
fn render_writes_changes_and_runs_a_hook() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("tpl")).unwrap();
    std::fs::write(dir.join("tpl/a.env"), "A=__A__\n").unwrap();
    std::fs::write(dir.join("tpl/b.env"), "B=__B__\n").unwrap();
    std::fs::write(
        dir.join("tegata.yaml"),
        r#"
stores: { main: { local: { dir: store } } }
resources:
  a: { provider: manual, stores: { main: [{ name: A }] } }
  b: { provider: manual, stores: { main: [{ name: B }] } }
"#,
    )
    .unwrap();
    for (r, v) in [("a", "one"), ("b", "two")] {
        let (ok, out) = tegata(dir, &["set", r], Some(v));
        assert!(ok, "{out}");
    }
    let log = dir.join("hook.log");
    let hook = format!("echo \"$TEGATA_CHANGED\" >> {}", log.display());
    let render = |extra: &[&str]| {
        let store = dir.join("store");
        let tpl = dir.join("tpl");
        let out = dir.join("out");
        let mut args = vec![
            "render",
            "--store-dir",
            store.to_str().unwrap(),
            "--on-change",
            &hook,
        ];
        args.extend_from_slice(extra);
        args.push(tpl.to_str().unwrap());
        args.push(out.to_str().unwrap());
        let o = Command::new(env!("CARGO_BIN_EXE_tegata"))
            .args(&args)
            .output()
            .unwrap();
        (
            o.status.code(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        )
    };
    let hook_runs = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };

    // first time: both files are new, and the hook runs once with both
    let (code, out) = render(&["--exit-code"]);
    assert_eq!(code, Some(3), "{out}");
    assert!(out.contains("changed: a.env b.env"), "{out}");
    assert_eq!(hook_runs(), vec!["a.env b.env"]);
    let written = std::fs::metadata(dir.join("out/a.env"))
        .unwrap()
        .modified()
        .unwrap();

    // nothing changed: nothing written, no hook, exit 0
    std::thread::sleep(std::time::Duration::from_millis(20));
    let (code, out) = render(&["--exit-code"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(!out.contains("changed:"), "{out}");
    assert_eq!(hook_runs().len(), 1);
    let again = std::fs::metadata(dir.join("out/a.env"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(written, again, "an unchanged file is not rewritten");

    // one value changes: only its file, and the hook gets its name
    let (ok, out) = tegata(dir, &["set", "a"], Some("three"));
    assert!(ok, "{out}");
    let (code, out) = render(&[]);
    assert_eq!(
        code,
        Some(0),
        "without --exit-code, a change still exits 0: {out}"
    );
    assert_eq!(hook_runs(), vec!["a.env b.env", "a.env"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("out/a.env")).unwrap(),
        "A=three\n"
    );

    // a failing hook fails render (the files are written already)
    let (ok, out) = tegata(dir, &["set", "b"], Some("four"));
    assert!(ok, "{out}");
    let o = Command::new(env!("CARGO_BIN_EXE_tegata"))
        .args([
            "render",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
            "--on-change",
            "exit 4",
            dir.join("tpl").to_str().unwrap(),
            dir.join("out").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("the --on-change command failed"), "{out}");
    assert_eq!(
        std::fs::read_to_string(dir.join("out/b.env")).unwrap(),
        "B=four\n"
    );

    // a missing name: with --fail-on-missing, nothing is written and the hook does not run
    std::fs::write(dir.join("tpl/a.env"), "A=__A__\nC=__NOT_THERE__\n").unwrap();
    let (code, out) = render(&["--fail-on-missing"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains("not in the store, or not readable: NOT_THERE"),
        "{out}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("out/a.env")).unwrap(),
        "A=three\n"
    );
    assert_eq!(hook_runs().len(), 2);
    // without it, the name stays and is listed, as before
    let (code, out) = render(&[]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("not in the store: NOT_THERE"), "{out}");
}
