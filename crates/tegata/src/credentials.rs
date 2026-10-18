//! プロバイダーの鍵（管理用のクレデンシャル）を取る
//!
//! 順番：環境変数（`env`。あって空でなければ）→ 取り出し元（`from`：別のプロジェクトの Secret Manager か、
//! コマンド）→ 端末なら入力を隠して聞く → `optional` なら渡さない。値はメモリの中だけに置き、
//! 出力やエラーの文に入れない

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::time::Duration;

use anyhow::Context;

use crate::config::{Credential, CredentialFrom, Loaded};
use crate::store::Store;

/// How long a credential command may take (it may ask the user to unlock, as 1Password does)
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// The credentials of one provider: name → value. Missing optional ones are left out
pub async fn resolve(loaded: &Loaded, provider: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let spec = &loaded.config.providers[provider].credentials;
    let mut got = BTreeMap::new();
    for (key, c) in spec {
        let what = format!("credential {key} of provider {provider}");
        if let Some(v) = from_env(c) {
            got.insert(key.clone(), v);
            continue;
        }
        if let Some(from) = &c.from {
            let v = read_from(loaded, from)
                .await
                .with_context(|| what.clone())?;
            got.insert(key.clone(), v);
            continue;
        }
        if c.optional {
            continue;
        }
        let prompt = c
            .prompt
            .clone()
            .unwrap_or_else(|| format!("{provider} {key}"));
        anyhow::ensure!(
            std::io::stderr().is_terminal(),
            "{what}: set {} or run in a terminal",
            c.env.as_deref().unwrap_or("its environment variable")
        );
        let v = rpassword::prompt_password(format!("{prompt} (input hidden): "))
            .with_context(|| format!("cannot read {key}"))?;
        got.insert(key.clone(), v);
    }
    Ok(got)
}

fn from_env(c: &Credential) -> Option<String> {
    c.env
        .as_deref()
        .and_then(|e| std::env::var(e).ok())
        .filter(|v| !v.is_empty())
}

/// One credential from its source
async fn read_from(loaded: &Loaded, from: &CredentialFrom) -> anyhow::Result<String> {
    let source = &loaded.config.credential_sources[&from.source];
    if let Some(g) = &source.gcp {
        let secret = from.secret.as_deref().expect("validated");
        let store = crate::store::gcp::GcpStore::new(Some(g.project.clone()), None).await?;
        return store
            .access(secret, from.version)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot read secret {secret}{} in project {} (does it exist, with an enabled version, and can you read it?)",
                    from.version.map(|v| format!(" version {v}")).unwrap_or_default(),
                    g.project
                )
            });
    }
    let command = source.command.as_ref().expect("validated");
    let reference = from.reference.as_deref().expect("validated");
    run_command(loaded, command, reference, COMMAND_TIMEOUT).await
}

/// Runs the command with the reference as its last argument, without a shell, and returns its
/// standard output without one trailing newline. Its output is never put in an error
async fn run_command(
    loaded: &Loaded,
    command: &[String],
    reference: &str,
    timeout: Duration,
) -> anyhow::Result<String> {
    let program = &command[0];
    // A path with a `/` is relative to the configuration, like a provider's command
    let path = if program.contains('/') {
        loaded.path(program)
    } else {
        program.into()
    };
    let child = tokio::process::Command::new(&path)
        .args(&command[1..])
        .arg(reference)
        .stdin(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("{program} did not finish in {}s", timeout.as_secs()))??;
    anyhow::ensure!(out.status.success(), "{program} failed ({})", out.status);
    let mut value = String::from_utf8(out.stdout)
        .map_err(|_| anyhow::anyhow!("{program} printed something that is not UTF-8"))?;
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    anyhow::ensure!(!value.is_empty(), "{program} printed nothing");
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(dir: &std::path::Path, yaml: &str) -> Loaded {
        let path = dir.join("tegata.yaml");
        std::fs::write(&path, yaml).unwrap();
        Loaded::load(&path).unwrap()
    }

    fn config(credentials: &str) -> String {
        format!(
            r#"
stores: {{ main: {{ local: {{ dir: s }} }} }}
credential_sources:
  cmd: {{ command: [./get.sh, --field] }}
providers:
  p:
    command: x
    credentials:
{credentials}
resources: {{}}
"#
        )
    }

    /// A stand-in for `op read` and the like: prints a value chosen by its last argument
    fn script(dir: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("get.sh");
        std::fs::write(
            &p,
            r#"#!/bin/sh
[ "$1" = --field ] || exit 9
case "$2" in
  token) printf 'tok-123\n' ;;
  crlf) printf 'v\r\n' ;;
  two) printf 'a\n\n' ;;
  empty) ;;
  secret-fails) echo do-not-show; exit 3 ;;
  slow) sleep 120 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn reads_credentials_from_a_command() {
        let dir = tempfile::tempdir().unwrap();
        script(dir.path());
        let l = loaded(
            dir.path(),
            &config(
                r#"      a: { from: { source: cmd, ref: token } }
      b: { from: { source: cmd, ref: crlf } }
      c: { from: { source: cmd, ref: two } }
      d: { optional: true }"#,
            ),
        );
        let got = resolve(&l, "p").await.unwrap();
        assert_eq!(got["a"], "tok-123", "one trailing newline removed");
        assert_eq!(got["b"], "v");
        assert_eq!(got["c"], "a\n", "only one");
        assert!(!got.contains_key("d"));
    }

    #[tokio::test]
    async fn an_environment_variable_wins_over_the_source() {
        let dir = tempfile::tempdir().unwrap();
        script(dir.path());
        let var = "TEGATA_TEST_CREDENTIAL_OVERRIDE";
        std::env::set_var(var, "from-env");
        let l = loaded(
            dir.path(),
            &config(&format!(
                "      a: {{ env: {var}, from: {{ source: cmd, ref: secret-fails }} }}"
            )),
        );
        assert_eq!(resolve(&l, "p").await.unwrap()["a"], "from-env");
        std::env::remove_var(var);
        // without it, the source is read (and here it fails)
        let e = format!("{:#}", resolve(&l, "p").await.unwrap_err());
        assert!(e.contains("credential a of provider p"), "{e}");
    }

    #[tokio::test]
    async fn command_failures_do_not_show_the_output() {
        let dir = tempfile::tempdir().unwrap();
        script(dir.path());
        for (reference, message) in [("secret-fails", "failed"), ("empty", "printed nothing")] {
            let l = loaded(
                dir.path(),
                &config(&format!(
                    "      a: {{ from: {{ source: cmd, ref: {reference} }} }}"
                )),
            );
            let e = format!("{:#}", resolve(&l, "p").await.unwrap_err());
            assert!(e.contains(message), "{reference}: {e}");
            assert!(!e.contains("do-not-show"), "{e}");
        }
    }

    #[test]
    fn checks_what_each_source_needs() {
        let dir = tempfile::tempdir().unwrap();
        let err = |credentials: &str, sources: &str| {
            let path = dir.path().join("tegata.yaml");
            std::fs::write(
                &path,
                format!(
                    "stores: {{ main: {{ local: {{ dir: s }} }} }}\ncredential_sources:\n{sources}\nproviders:\n  p:\n    command: x\n    credentials:\n{credentials}\nresources: {{}}\n"
                ),
            )
            .unwrap();
            format!("{:#}", Loaded::load(&path).unwrap_err())
        };
        let both = "  cmd: { command: [op, read] }\n  admin: { gcp: { project: adm } }";
        for (credentials, message) in [
            (
                "      a: { from: { source: nope, ref: x } }",
                "no credential source nope",
            ),
            (
                "      a: { from: { source: cmd, secret: x } }",
                "give `ref: <reference>`",
            ),
            (
                "      a: { from: { source: admin, ref: x } }",
                "give the secret",
            ),
            ("      a: { from: { source: admin } }", "give the secret"),
        ] {
            let e = err(credentials, both);
            assert!(e.contains(message), "{credentials}: {e}");
        }
        let ok = "      a: { from: { source: admin, secret: s } }";
        assert!(err(ok, "  admin: { gcp: { project: adm }, command: [x] }")
            .contains("exactly one of gcp and command"));
        assert!(err(ok, "  admin: { command: [] }").contains("a program and its arguments"));
    }

    #[tokio::test]
    async fn a_command_that_hangs_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        script(dir.path());
        let l = loaded(dir.path(), &config("      a: { optional: true }"));
        let command = ["./get.sh".to_owned(), "--field".to_owned()];
        let started = std::time::Instant::now();
        let e = run_command(&l, &command, "slow", Duration::from_millis(300))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("did not finish"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
