//! `tegata init` against a local mirror of releases signed with a test key

use std::io::Cursor;
use std::path::Path;

use tegata::config::Loaded;
use tegata::install::{self, Lock, Mirror};

const SOURCE: &str = "github.com/example/tools";
const OTHER_PLATFORM: &str = "linux_riscv64";

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Writes one release of the `hello` provider into the mirror, signed with `key`
fn release(mirror: &Path, version: &str, key: &minisign::KeyPair, body: &str) {
    release_script(
        &mirror.join(SOURCE),
        version,
        key,
        &format!("#!/bin/sh\necho {body} {version}\n"),
    );
}

/// Writes one release into `repo` (`<mirror>/<source>`); its executable (for every platform) is
/// `script`
fn release_script(repo: &Path, version: &str, key: &minisign::KeyPair, script: &str) {
    let dir = repo.join(version);
    std::fs::create_dir_all(&dir).unwrap();
    let mut sums = String::new();
    for platform in [install::platform().unwrap(), OTHER_PLATFORM.to_owned()] {
        let name = format!("tegata-provider-hello_{version}_{platform}");
        std::fs::write(dir.join(&name), script).unwrap();
        sums.push_str(&format!("{}  {name}\n", sha256_hex(script.as_bytes())));
    }
    sign(&dir, &sums, key);
}

/// One cache for the whole test binary (tests run in parallel; each uses its own source)
fn use_cache() {
    static CACHE: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = CACHE.get_or_init(|| tempfile::tempdir().unwrap());
    std::env::set_var("TEGATA_CACHE_DIR", dir.path());
}

fn sign(dir: &Path, sums: &str, key: &minisign::KeyPair) {
    std::fs::write(dir.join("SHA256SUMS"), sums).unwrap();
    let sig = minisign::sign(
        Some(&key.pk),
        &key.sk,
        Cursor::new(sums.as_bytes()),
        None,
        None,
    )
    .unwrap();
    std::fs::write(dir.join("SHA256SUMS.minisig"), sig.into_string()).unwrap();
}

fn config(dir: &Path, provider: &str) -> Loaded {
    let path = dir.join("tegata.yaml");
    std::fs::write(
        &path,
        format!("store:\n  local:\n    dir: ./store\nroles: {{}}\nproviders:\n{provider}resources: {{}}\n"),
    )
    .unwrap();
    Loaded::load(&path).unwrap()
}

fn locked_version(dir: &Path) -> String {
    let lock = Lock::load(&dir.join(install::LOCK_FILE)).unwrap().unwrap();
    lock.providers["hello"].version.clone()
}

#[tokio::test]
async fn installs_pins_and_checks_providers() {
    let work = tempfile::tempdir().unwrap();
    let mirror = tempfile::tempdir().unwrap();
    use_cache();
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pk = key.pk.to_base64();
    for v in ["1.0.0", "1.2.0", "2.0.0"] {
        release(mirror.path(), v, &key, "hello");
    }
    let loaded = config(
        work.path(),
        &format!("  hello:\n    source: {SOURCE}\n    version: \"^1\"\n    public_key: {pk}\n"),
    );
    let releases = Mirror(mirror.path().to_owned());
    let p = &loaded.config.providers["hello"];

    // before init: asks to run it
    let err = install::installed(&loaded, "hello", p)
        .unwrap_err()
        .to_string();
    assert!(err.contains("tegata init"), "{err}");

    // the newest version the requirement allows, with hashes for every platform
    let lines = install::init(&loaded, false, &releases).await.unwrap();
    assert!(
        lines.iter().any(|l| l.contains("v1.2.0 (downloaded)")),
        "{lines:?}"
    );
    let lock = Lock::load(&work.path().join(install::LOCK_FILE))
        .unwrap()
        .unwrap();
    let locked = &lock.providers["hello"];
    assert_eq!(locked.version, "1.2.0");
    assert_eq!(locked.public_key, pk);
    assert_eq!(locked.hashes.len(), 2);
    assert!(locked.hashes.contains_key(OTHER_PLATFORM));
    let path = install::installed(&loaded, "hello", p).unwrap();
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("hello 1.2.0"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
            0o111
        );
    }

    // a newer release does not change the locked version without --upgrade
    release(mirror.path(), "1.3.0", &key, "hello");
    let lines = install::init(&loaded, false, &releases).await.unwrap();
    assert!(
        lines.iter().any(|l| l.contains("v1.2.0 (installed)")),
        "{lines:?}"
    );
    assert_eq!(locked_version(work.path()), "1.2.0");
    let lines = install::init(&loaded, true, &releases).await.unwrap();
    assert!(
        lines.iter().any(|l| l.contains("v1.3.0 (downloaded)")),
        "{lines:?}"
    );
    assert_eq!(locked_version(work.path()), "1.3.0");

    // a changed installed file is refused, and init installs it again
    let path = install::installed(&loaded, "hello", p).unwrap();
    std::fs::write(&path, "#!/bin/sh\necho changed\n").unwrap();
    let err = install::installed(&loaded, "hello", p)
        .unwrap_err()
        .to_string();
    assert!(err.contains("differs from tegata.lock"), "{err}");
    let lines = install::init(&loaded, false, &releases).await.unwrap();
    assert!(
        lines.iter().any(|l| l.contains("v1.3.0 (downloaded)")),
        "{lines:?}"
    );
    install::installed(&loaded, "hello", p).unwrap();

    // the locked release's files were replaced (even if signed): refused without --upgrade
    let dir = mirror.path().join(SOURCE).join("1.3.0");
    let original = std::fs::read_to_string(dir.join("SHA256SUMS")).unwrap();
    let replaced = original.replacen(&original[..8], "00000000", 1);
    sign(&dir, &replaced, &key);
    let err = format!(
        "{:#}",
        install::init(&loaded, false, &releases).await.unwrap_err()
    );
    assert!(err.contains("differ from tegata.lock"), "{err}");
    sign(&dir, &original, &key);

    // a release signed by another key is refused
    let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    release(mirror.path(), "1.4.0", &other, "evil");
    let err = format!(
        "{:#}",
        install::init(&loaded, true, &releases).await.unwrap_err()
    );
    assert!(err.contains("not signed by the expected key"), "{err}");
    assert_eq!(locked_version(work.path()), "1.3.0");
}

#[test]
fn source_and_command_are_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let write = |provider: &str| {
        let path = dir.path().join("tegata.yaml");
        std::fs::write(
            &path,
            format!("store:\n  local:\n    dir: ./store\nroles: {{}}\nproviders:\n{provider}resources: {{}}\n"),
        )
        .unwrap();
        Loaded::load(&path).err().map(|e| format!("{e:#}"))
    };
    let both = write("  x:\n    command: a\n    source: github.com/a/b\n    version: \"1\"\n");
    assert!(both.unwrap().contains("exactly one of command and source"));
    let no_version = write("  x:\n    source: github.com/a/b\n");
    assert!(no_version.unwrap().contains("set version with source"));
    let bad_source = write("  x:\n    source: gitlab.com/a/b\n    version: \"1\"\n");
    assert!(bad_source.unwrap().contains("github.com/<owner>/<repo>"));
    let version_with_command = write("  x:\n    command: a\n    version: \"1\"\n");
    assert!(version_with_command.unwrap().contains("go with source"));
    assert!(write("  x:\n    source: github.com/a/b\n    version: \"~1.2\"\n").is_none());
}

/// A provider that makes one value, as a shell script
const SCRIPT: &str = r#"#!/bin/sh
case "$(cat)" in
  *'"op":"describe"'*) echo '{"types":{"token":"managed"}}' ;;
  *) echo '{"outputs":{"value":"from-installed-provider"},"state":{}}' ;;
esac
"#;

fn tegata(dir: &Path, mirror: &Path, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_tegata"))
        .arg("-f")
        .arg(dir.join("tegata.yaml"))
        .args(args)
        .env("TEGATA_PROVIDER_MIRROR", mirror)
        .env("TEGATA_CACHE_DIR", dir.join("cache"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
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
fn the_cli_uses_installed_providers() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let mirror = tempfile::tempdir().unwrap();
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let source = "github.com/example/cli";
    release_script(&mirror.path().join(source), "0.1.0", &key, SCRIPT);
    std::fs::create_dir_all(dir.join("tpl")).unwrap();
    std::fs::write(dir.join("tpl/a.env"), "T=__TOKEN__\n").unwrap();
    std::fs::write(
        dir.join("tegata.yaml"),
        format!(
            r#"
store: {{ local: {{ dir: store }} }}
roles: {{ app: {{ member: "serviceAccount:a@x", templates: tpl }} }}
providers:
  hello: {{ source: {source}, version: "0.1", public_key: {} }}
resources:
  token: {{ provider: hello, type: token, secrets: {{ TOKEN: value }} }}
"#,
            key.pk.to_base64()
        ),
    )
    .unwrap();

    let (ok, out) = tegata(dir, mirror.path(), &["check"]);
    assert!(!ok);
    assert!(out.contains("run `tegata init` (provider hello)"), "{out}");

    let (ok, out) = tegata(dir, mirror.path(), &["init"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("hello: github.com/example/cli v0.1.0 (downloaded)"),
        "{out}"
    );
    assert!(out.contains("wrote tegata.lock"), "{out}");
    let (ok, out) = tegata(dir, mirror.path(), &["init"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("(installed)") && !out.contains("wrote"),
        "{out}"
    );

    let (ok, out) = tegata(dir, mirror.path(), &["apply", "--yes"]);
    assert!(ok, "{out}");
    let store = dir.join("store");
    let (ok, out) = tegata(
        dir,
        mirror.path(),
        &[
            "render",
            "--store-dir",
            store.to_str().unwrap(),
            dir.join("tpl").to_str().unwrap(),
            dir.join("out").to_str().unwrap(),
        ],
    );
    assert!(ok, "{out}");
    let a = std::fs::read_to_string(dir.join("out/a.env")).unwrap();
    assert_eq!(a, "T=from-installed-provider\n");
}

#[tokio::test]
async fn installs_from_github_releases() {
    use axum::extract::{Path as UrlPath, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;

    use_cache();
    let files = tempfile::tempdir().unwrap();
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    for v in ["0.9.0", "1.0.0"] {
        release_script(files.path(), v, &key, SCRIPT);
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());

    #[derive(Clone)]
    struct App {
        base: String,
        files: std::path::PathBuf,
    }
    fn authorized(headers: &HeaderMap) -> Result<(), StatusCode> {
        match headers.get("authorization").and_then(|v| v.to_str().ok()) {
            Some("Bearer test-token") => Ok(()),
            _ => Err(StatusCode::UNAUTHORIZED),
        }
    }
    let app = axum::Router::new()
        .route(
            "/repos/example/gh/releases",
            get(|headers: HeaderMap| async move {
                authorized(&headers)?;
                Ok::<_, StatusCode>(axum::Json(serde_json::json!([
                    { "tag_name": "v1.0.0" },
                    { "tag_name": "v0.9.0" },
                    { "tag_name": "v2.0.0", "draft": true },
                    { "tag_name": "nightly" },
                ])))
            }),
        )
        .route(
            "/repos/example/gh/releases/tags/{tag}",
            get(
                |State(app): State<App>, UrlPath(tag): UrlPath<String>, headers: HeaderMap| async move {
                    authorized(&headers)?;
                    let version = tag.strip_prefix('v').ok_or(StatusCode::NOT_FOUND)?;
                    let dir = app.files.join(version);
                    let assets: Vec<_> = std::fs::read_dir(&dir)
                        .map_err(|_| StatusCode::NOT_FOUND)?
                        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .map(|name| {
                            serde_json::json!({
                                "name": name,
                                "url": format!("{}/assets/{version}/{name}", app.base),
                            })
                        })
                        .collect();
                    Ok::<_, StatusCode>(axum::Json(
                        serde_json::json!({ "tag_name": tag, "assets": assets }),
                    ))
                },
            ),
        )
        .route(
            "/assets/{version}/{name}",
            get(
                |State(app): State<App>,
                 UrlPath((version, name)): UrlPath<(String, String)>,
                 headers: HeaderMap| async move {
                    authorized(&headers)?;
                    // without this header, GitHub answers with the asset's JSON
                    if headers.get("accept").and_then(|v| v.to_str().ok())
                        != Some("application/octet-stream")
                    {
                        return Err(StatusCode::NOT_ACCEPTABLE);
                    }
                    std::fs::read(app.files.join(version).join(name))
                        .map_err(|_| StatusCode::NOT_FOUND)
                },
            ),
        )
        .with_state(App {
            base: base.clone(),
            files: files.path().to_owned(),
        });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    std::env::set_var("TEGATA_GITHUB_API", &base);
    std::env::set_var("GITHUB_TOKEN", "test-token");
    let github = install::GitHub::new();
    let work = tempfile::tempdir().unwrap();
    let loaded = config(
        work.path(),
        &format!(
            "  hello:\n    source: github.com/example/gh\n    version: \"*\"\n    public_key: {}\n",
            key.pk.to_base64()
        ),
    );
    let lines = install::init(&loaded, false, &github).await.unwrap();
    assert!(
        lines.iter().any(|l| l.contains("v1.0.0 (downloaded)")),
        "drafts and tags that are not versions are skipped: {lines:?}"
    );
    let p = &loaded.config.providers["hello"];
    let path = install::installed(&loaded, "hello", p).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), SCRIPT);
}
