//! プロバイダーを呼ぶ
//!
//! `random`（乱数）、`manual`（手で入れる）、`value`（YAML に書いた秘密でない値。読むだけ）は tegata の中にある。ほかは外の実行ファイルで、1 回の操作ごとに起動し、
//! 標準入力に要求を 1 つ渡して、標準出力から応答を 1 つ受け取る

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Context;
use tegata_plugin::{Created, Kind, Op, Request, Response, PROTOCOL};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::config::{Loaded, Resource};

pub struct Providers<'a> {
    loaded: &'a Loaded,
    credentials: Mutex<BTreeMap<String, BTreeMap<String, String>>>,
    kinds: Mutex<BTreeMap<String, Described>>,
}

/// プロバイダーの describe の結果
#[derive(Clone)]
struct Described {
    types: BTreeMap<String, Kind>,
    /// 作ると前の値がすぐ効かなくなる型
    replaces: BTreeSet<String>,
}

pub fn is_builtin(provider: &str) -> bool {
    matches!(provider, "random" | "manual" | "value")
}

impl<'a> Providers<'a> {
    pub fn new(loaded: &'a Loaded) -> Self {
        Self {
            loaded,
            credentials: Mutex::new(BTreeMap::new()),
            kinds: Mutex::new(BTreeMap::new()),
        }
    }

    /// プロバイダーの型ごとの種類
    pub async fn describe(&self, provider: &str) -> anyhow::Result<BTreeMap<String, Kind>> {
        Ok(self.described(provider).await?.types)
    }

    async fn described(&self, provider: &str) -> anyhow::Result<Described> {
        if provider == "value" {
            return Ok(Described {
                types: BTreeMap::from([(String::new(), Kind::Data)]),
                replaces: BTreeSet::new(),
            });
        }
        if is_builtin(provider) {
            return Ok(Described {
                types: BTreeMap::from([(String::new(), Kind::Managed)]),
                replaces: BTreeSet::new(),
            });
        }
        let mut kinds = self.kinds.lock().await;
        if let Some(k) = kinds.get(provider) {
            return Ok(k.clone());
        }
        let res = self
            .call(
                provider,
                Op::Describe,
                "",
                "",
                &Default::default(),
                &serde_json::Value::Null,
                false,
            )
            .await?;
        let d = Described {
            types: res.types,
            replaces: res.replaces.into_iter().collect(),
        };
        kinds.insert(provider.to_owned(), d.clone());
        Ok(d)
    }

    /// 作ると前の値がすぐ効かなくなるか（プロバイダーが申告した型）
    pub async fn replaces(&self, res: &Resource) -> anyhow::Result<bool> {
        Ok(self
            .described(&res.provider)
            .await?
            .replaces
            .contains(&res.kind))
    }

    /// リソースの種類
    pub async fn kind(&self, name: &str, res: &Resource) -> anyhow::Result<Kind> {
        if res.provider == "value" {
            return Ok(Kind::Data);
        }
        if is_builtin(&res.provider) {
            return Ok(Kind::Managed);
        }
        self.describe(&res.provider)
            .await?
            .get(&res.kind)
            .copied()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "resource {name}: provider {} has no type {:?}",
                    res.provider,
                    res.kind
                )
            })
    }

    pub async fn create(&self, name: &str, res: &Resource) -> anyhow::Result<Created> {
        match res.provider.as_str() {
            "random" => random(res),
            "manual" => manual(res),
            provider => {
                let r = self
                    .call(
                        provider,
                        Op::Create,
                        name,
                        &res.kind,
                        &res.config,
                        &serde_json::Value::Null,
                        true,
                    )
                    .await?;
                Ok(Created {
                    outputs: r.outputs,
                    state: r.state,
                })
            }
        }
    }

    /// 世代のものを消す。`current` は残す最新の世代の state（prune のとき。プロバイダーに `current_state` で渡す）
    pub async fn delete(
        &self,
        name: &str,
        res: &Resource,
        state: &serde_json::Value,
        current: &serde_json::Value,
    ) -> anyhow::Result<()> {
        if is_builtin(&res.provider) {
            return Ok(());
        }
        self.call_with(
            &res.provider,
            Op::Delete,
            name,
            &res.kind,
            &res.config,
            state,
            current,
            true,
        )
        .await?;
        Ok(())
    }

    pub async fn read(
        &self,
        name: &str,
        res: &Resource,
    ) -> anyhow::Result<BTreeMap<String, String>> {
        if res.provider == "value" {
            // config の文字列の値（秘密でないものだけを書く）
            let map = res
                .config
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("{name}: config must be a map of strings"))?;
            return map
                .iter()
                .map(|(k, v)| {
                    v.as_str()
                        .map(|s| (k.clone(), s.to_owned()))
                        .ok_or_else(|| anyhow::anyhow!("{name}: config.{k} is not a string"))
                })
                .collect();
        }
        anyhow::ensure!(
            !is_builtin(&res.provider),
            "built-in providers have no data"
        );
        Ok(self
            .call(
                &res.provider,
                Op::Read,
                name,
                &res.kind,
                &res.config,
                &serde_json::Value::Null,
                true,
            )
            .await?
            .outputs)
    }

    /// 鍵を集める（環境変数、なければ端末で聞く）。1 回の実行で 1 度だけ聞く
    async fn credentials(&self, provider: &str) -> anyhow::Result<BTreeMap<String, String>> {
        let mut cache = self.credentials.lock().await;
        if let Some(c) = cache.get(provider) {
            return Ok(c.clone());
        }
        let spec = &self.loaded.config.providers[provider].credentials;
        let mut got = BTreeMap::new();
        for (key, c) in spec {
            let from_env = c
                .env
                .as_deref()
                .and_then(|e| std::env::var(e).ok())
                .filter(|v| !v.is_empty());
            let value = match from_env {
                Some(v) => v,
                None if c.optional => continue,
                None => {
                    let prompt = c
                        .prompt
                        .clone()
                        .unwrap_or_else(|| format!("{provider} {key}"));
                    anyhow::ensure!(
                        std::io::stderr().is_terminal(),
                        "credential {key} of provider {provider}: set {} or run in a terminal",
                        c.env.as_deref().unwrap_or("its environment variable")
                    );
                    rpassword::prompt_password(format!("{prompt} (input hidden): "))
                        .with_context(|| format!("cannot read {key}"))?
                }
            };
            got.insert(key.clone(), value);
        }
        cache.insert(provider.to_owned(), got.clone());
        Ok(got)
    }

    fn command(&self, provider: &str) -> anyhow::Result<PathBuf> {
        let cmd = &self.loaded.config.providers[provider].command;
        if cmd.contains('/') {
            return Ok(self.loaded.path(cmd));
        }
        // PATH、なければ tegata の隣（cargo で作ったもの、同じ場所に置いたもの）
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                let p = dir.join(cmd);
                if p.is_file() {
                    return Ok(p);
                }
            }
        }
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from))
        {
            let p = dir.join(cmd);
            if p.is_file() {
                return Ok(p);
            }
        }
        anyhow::bail!("provider {provider}: {cmd} is not found in PATH or next to tegata")
    }

    #[allow(clippy::too_many_arguments)]
    async fn call(
        &self,
        provider: &str,
        op: Op,
        resource: &str,
        kind: &str,
        config: &serde_json::Value,
        state: &serde_json::Value,
        with_credentials: bool,
    ) -> anyhow::Result<Response> {
        self.call_with(
            provider,
            op,
            resource,
            kind,
            config,
            state,
            &serde_json::Value::Null,
            with_credentials,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn call_with(
        &self,
        provider: &str,
        op: Op,
        resource: &str,
        kind: &str,
        config: &serde_json::Value,
        state: &serde_json::Value,
        current_state: &serde_json::Value,
        with_credentials: bool,
    ) -> anyhow::Result<Response> {
        let p = &self.loaded.config.providers[provider];
        let req = Request {
            protocol: PROTOCOL,
            op,
            kind: kind.to_owned(),
            config: config.clone(),
            provider_config: p.config.clone(),
            credentials: if with_credentials {
                self.credentials(provider).await?
            } else {
                BTreeMap::new()
            },
            state: state.clone(),
            current_state: current_state.clone(),
            base_dir: self.loaded.base_dir.display().to_string(),
            resource: resource.to_owned(),
        };
        let command = self.command(provider)?;
        let mut child = tokio::process::Command::new(&command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| format!("cannot run {}", command.display()))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(&serde_json::to_vec(&req)?).await?;
        drop(stdin);
        let out = child.wait_with_output().await?;
        let res: Response = serde_json::from_slice(&out.stdout).with_context(|| {
            format!(
                "provider {provider} returned no valid response ({:?} {resource}, {})",
                op, out.status
            )
        })?;
        if let Some(e) = res.error {
            anyhow::bail!("provider {provider} ({op:?} {resource}): {e}");
        }
        anyhow::ensure!(
            out.status.success(),
            "provider {provider} ({op:?} {resource}) exited with {}",
            out.status
        );
        Ok(res)
    }
}

/// 乱数（config.bytes。既定 32 バイト）を 16 進で
fn random(res: &Resource) -> anyhow::Result<Created> {
    use rand::RngCore;
    let bytes = res
        .config
        .get("bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(32);
    anyhow::ensure!((8..=256).contains(&bytes), "random: bytes must be 8..=256");
    let mut buf = vec![0u8; bytes as usize];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    let value: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    Ok(Created {
        outputs: res
            .secrets
            .values()
            .map(|out| (out.clone(), value.clone()))
            .collect(),
        state: serde_json::Value::Null,
    })
}

/// 手で入れる：端末なら秘密ごとに入力を隠して聞く。端末でなければ、標準入力のすべて（秘密が 1 つのときだけ）
pub fn manual(res: &Resource) -> anyhow::Result<Created> {
    let mut outputs = BTreeMap::new();
    if std::io::stdin().is_terminal() {
        for (secret, out) in &res.secrets {
            let v = rpassword::prompt_password(format!("{secret} (input hidden): "))?;
            anyhow::ensure!(!v.is_empty(), "{secret}: empty value");
            outputs.insert(out.clone(), v);
        }
    } else {
        anyhow::ensure!(
            res.secrets.len() == 1,
            "manual: pipe one value at a time (this resource has several secrets; run in a terminal)"
        );
        let mut v = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut v)?;
        let v = v.strip_suffix('\n').unwrap_or(&v).to_owned();
        anyhow::ensure!(!v.is_empty(), "empty value on stdin");
        outputs.insert(res.secrets.values().next().expect("one").clone(), v);
    }
    Ok(Created {
        outputs,
        state: serde_json::Value::Null,
    })
}
