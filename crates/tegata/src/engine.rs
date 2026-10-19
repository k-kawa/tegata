//! サブコマンドの中身

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use tegata_plugin::Kind;

use crate::config::{GithubDestination, Loaded, Resource};
use crate::deliver::github::{Github, GithubAccess, Token};
use crate::deliver::{deliveries, Delivered, Destination as _};
use crate::provider::{is_builtin, Providers};
use crate::store::{latest_enabled, SecretMeta, Store};
use crate::template;

const MANAGED_BY: &str = "managed-by";
const RESOURCE: &str = "tegata-resource";
/// The environment a secret belongs to, when the configuration has environments
const ENV: &str = "tegata-env";
const GEN_PREFIX: &str = "tegata-gen-";

/// 1 つの世代の記録（リソースの最初の秘密の注記）
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Generation {
    /// 秘密 → 足した版
    pub versions: BTreeMap<String, u64>,
    /// プロバイダーが返した state（消すのに使う）
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub state: serde_json::Value,
    /// 前の形から入れたもの（プロバイダーの state がない）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub imported: bool,
}

pub fn generations(meta: &SecretMeta) -> BTreeMap<u64, Generation> {
    meta.annotations
        .iter()
        .filter_map(|(k, v)| {
            let n = k.strip_prefix(GEN_PREFIX)?.parse().ok()?;
            Some((n, serde_json::from_str(v).ok()?))
        })
        .collect()
}

/// Asks for the environment's name: `--yes` passes, a terminal is needed otherwise, and only the
/// exact name continues
fn confirm_name(
    env: &str,
    approval: Approval,
    terminal: bool,
    input: &mut dyn std::io::BufRead,
) -> anyhow::Result<()> {
    if approval == Approval::Yes {
        return Ok(());
    }
    anyhow::ensure!(
        terminal,
        "environment {env} asks before changing the store; run in a terminal to confirm, or pass --yes"
    );
    eprint!("Type the environment name to continue: ");
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    anyhow::ensure!(answer.trim() == env, "cancelled");
    Ok(())
}

/// What `check` found
#[derive(Debug, Default)]
pub struct Checked {
    /// Make `check` fail
    pub problems: Vec<String>,
    /// Worth a look, but not wrong (an identity can read more than its templates use)
    pub warnings: Vec<String>,
}

/// 新しい世代を作る前の確かめ方
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Approval {
    /// 前の値がすぐ効かなくなるものがあれば、端末で `yes` を求める（端末でなければ断る）
    #[default]
    Ask,
    /// 確かめずに進む（`--yes`）
    Yes,
    /// 作るものを出すだけで、何も変えない（`--dry-run`）
    DryRun,
}

pub struct Engine<'a> {
    pub loaded: &'a Loaded,
    pub store: &'a dyn Store,
    pub providers: Providers<'a>,
    pub approval: Approval,
    /// The GitHub token for destinations (read when first needed)
    github: Token,
}

/// One secret to deliver: (destination name, where, name there, secret name here)
type Wanted<'c> = (&'c str, &'c GithubDestination, String, String);

impl<'a> Engine<'a> {
    pub fn new(loaded: &'a Loaded, store: &'a dyn Store) -> Self {
        Self {
            loaded,
            store,
            providers: Providers::new(loaded),
            approval: Approval::default(),
            github: Token::new(GithubAccess::from_env()),
        }
    }

    /// Where destinations reach GitHub (tests point it at a fake server)
    pub fn with_github(mut self, access: GithubAccess) -> Self {
        self.github = Token::new(access);
        self
    }

    pub fn with_approval(mut self, approval: Approval) -> Self {
        self.approval = approval;
        self
    }

    /// 新しい世代を作るリソースを見せ、前の値がすぐ効かなくなるものは確かめる。
    /// 進んでよければ true、dry-run なら false。断られたら失敗
    async fn approve(&self, names: &[String]) -> anyhow::Result<bool> {
        let mut replacing = Vec::new();
        for name in names {
            let res = self.resource(name)?;
            let replaces = self.providers.replaces(res).await?;
            if self.approval == Approval::DryRun {
                println!(
                    "would make a new generation of {name}{}",
                    if replaces {
                        " (the old value stops working at once)"
                    } else {
                        ""
                    }
                );
            }
            if replaces {
                replacing.push((name, res));
            }
        }
        if self.approval == Approval::DryRun && names.is_empty() {
            println!("no new generation to make");
        }
        if replacing.is_empty() {
            return Ok(self.approval != Approval::DryRun);
        }
        let all_readers = self.loaded.readers()?;
        let mut notice = String::new();
        for (name, res) in &replacing {
            notice.push_str(&format!(
                "{name} ({} {}: {})\n  the old value stops working at once; open connections stay, new ones with the old value fail\n",
                res.provider, res.kind, res.config
            ));
            let ids: BTreeSet<&String> = res
                .all_secrets()
                .iter()
                .filter_map(|s| all_readers.get(s))
                .flatten()
                .collect();
            let readers: Vec<String> = ids
                .into_iter()
                .map(|id| {
                    format!(
                        "{id} ({})",
                        self.loaded.config.identities[id].members.join(", ")
                    )
                })
                .collect();
            if readers.is_empty() {
                notice.push_str("  nobody reads it\n");
            } else {
                notice.push_str(&format!(
                    "  read by: {}\n  update them right after this (run `tegata render` there and restart what uses the value)\n",
                    readers.join(", ")
                ));
            }
        }
        eprint!("{notice}");
        match self.approval {
            Approval::DryRun => Ok(false),
            Approval::Yes => Ok(true),
            Approval::Ask => {
                use std::io::IsTerminal;
                anyhow::ensure!(
                    std::io::stdin().is_terminal(),
                    "{} would stop working at once; run in a terminal to confirm, or pass --yes",
                    replacing
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                eprint!("Enter \"yes\" to continue: ");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                anyhow::ensure!(answer.trim() == "yes", "cancelled");
                Ok(true)
            }
        }
    }

    /// apply で新しい世代を作るリソース（値がまだない、作るもの）
    async fn to_fill(&self) -> anyhow::Result<Vec<String>> {
        let mut names = Vec::new();
        for (name, res) in &self.loaded.config.resources {
            if res.provider == "manual" {
                continue;
            }
            // 種類が分からないもの（プロバイダーがないなど）は、apply がほかと続けて失敗を挙げる
            if !matches!(self.providers.kind(name, res).await, Ok(Kind::Managed)) {
                continue;
            }
            let mut empty = true;
            for secret in res.secrets.keys() {
                if latest_enabled(self.store, secret).await?.is_some() {
                    empty = false;
                    break;
                }
            }
            if empty {
                names.push(name.clone());
            }
        }
        Ok(names)
    }

    fn resource(&self, name: &str) -> anyhow::Result<&'a Resource> {
        self.loaded
            .config
            .resources
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("no resource {name} in the config"))
    }

    fn labels(&self, resource: &str) -> BTreeMap<String, String> {
        let mut labels = BTreeMap::from([
            (MANAGED_BY.to_owned(), "tegata".to_owned()),
            (RESOURCE.to_owned(), resource.to_owned()),
        ]);
        if let Some(env) = &self.loaded.config.environment {
            labels.insert(ENV.to_owned(), env.clone());
        }
        labels
    }

    /// The names of the secrets this configuration's resources declare (including `previous`)
    fn declared(&self) -> BTreeSet<String> {
        self.loaded
            .config
            .resources
            .values()
            .flat_map(Resource::all_secrets)
            .collect()
    }

    /// Whether a secret in the store belongs to this configuration and environment: managed by
    /// tegata, not labeled with another environment (as in `ensure_secret`), and declared by one of
    /// this configuration's resources. Other configurations and environments may share the store,
    /// so only these secrets' deliveries are ever removed
    fn owns(&self, declared: &BTreeSet<String>, secret: &str, meta: &SecretMeta) -> bool {
        meta.labels.get(MANAGED_BY).map(String::as_str) == Some("tegata")
            && self.other_environment(meta).is_none()
            && declared.contains(secret)
    }

    /// The environment a secret is labeled with, when it is not the one being worked on.
    /// Such a secret belongs to another environment that uses the same store: never touch it
    fn other_environment<'m>(&self, meta: &'m SecretMeta) -> Option<&'m String> {
        meta.labels
            .get(ENV)
            .filter(|e| Some(*e) != self.loaded.config.environment.as_ref())
    }

    /// In an environment with `confirm: true`, says what the command will do there and asks for
    /// the environment's name (`--yes` skips the question). Elsewhere, and for `--dry-run`, nothing
    fn confirm_environment(&self, what: &str) -> anyhow::Result<()> {
        let config = &self.loaded.config;
        if !config.confirm || self.approval == Approval::DryRun {
            return Ok(());
        }
        let env = config.environment.as_deref().unwrap_or_default();
        eprintln!("{env}: {what}");
        use std::io::IsTerminal;
        let stdin = std::io::stdin();
        confirm_name(env, self.approval, stdin.is_terminal(), &mut stdin.lock())
    }

    /// 秘密 → 読む相手（その秘密を読む identity のメンバー）
    fn desired_readers(&self) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
        Ok(self
            .loaded
            .readers()?
            .into_iter()
            .map(|(secret, ids)| {
                let members = ids
                    .iter()
                    .flat_map(|id| self.loaded.config.identities[id].members.iter().cloned())
                    .collect();
                (secret, members)
            })
            .collect())
    }

    /// プロバイダーに型があるか。`templates` があれば（identity と雛形のディレクトリ）、
    /// その identity が雛形の名前をすべて読めるか。前の形では、役割の雛形もいつも確かめる
    pub async fn check(&self, templates: Option<(&str, &Path)>) -> anyhow::Result<Checked> {
        let mut checked = Checked::default();
        let owned: BTreeSet<String> = self
            .loaded
            .config
            .resources
            .values()
            .flat_map(Resource::all_secrets)
            .collect();
        let readers = self.loaded.readers()?;
        let reads =
            |id: &str, secret: &str| readers.get(secret).is_some_and(|ids| ids.contains(id));
        let mut pairs = self.loaded.old_template_names()?;
        if let Some((id, dir)) = templates {
            anyhow::ensure!(
                self.loaded.config.identities.contains_key(id),
                "no identity {id} in the config"
            );
            pairs.insert(id.to_owned(), template::names_in_dir(dir)?);
        }
        for (id, names) in pairs {
            for n in &names {
                if !owned.contains(n) {
                    checked.problems.push(format!(
                        "identity {id}: {n} is in the templates but no resource makes it"
                    ));
                } else if !reads(&id, n) {
                    checked.problems.push(format!(
                        "identity {id}: {n} is in the templates but {id} cannot read it (add {id} to its readers)"
                    ));
                }
            }
            for secret in owned.difference(&names) {
                if reads(&id, secret) {
                    checked.warnings.push(format!(
                        "identity {id}: can read {secret}, which its templates do not use"
                    ));
                }
            }
        }
        for (name, res) in &self.loaded.config.resources {
            if let Err(e) = self.providers.kind(name, res).await {
                checked.problems.push(format!("{e:#}"));
            }
        }
        Ok(checked)
    }

    /// 入れ物、ラベル、読む権限をそろえ、値のないリソースを作り、data を読み直す
    /// 失敗したリソースがあっても、ほかは続ける（鍵のないプロバイダーがあっても、ほかをそろえられるように）
    pub async fn apply(&self) -> anyhow::Result<()> {
        let env = self
            .loaded
            .config
            .environment
            .as_deref()
            .unwrap_or_default();
        self.confirm_environment(&format!(
            "apply the configuration (to see what changes first: tegata -e {env} apply --dry-run)"
        ))?;
        if !self.approve(&self.to_fill().await?).await? {
            // dry-run：届け先に書くもの、消すものも出す
            return self.deliver(true, true).await;
        }
        let readers = self.desired_readers()?;
        let mut failed = Vec::new();
        for (name, res) in &self.loaded.config.resources {
            let result = async {
                for secret in res.all_secrets() {
                    self.ensure_secret(
                        name,
                        &secret,
                        readers.get(&secret).cloned().unwrap_or_default(),
                    )
                    .await?;
                }
                match self.providers.kind(name, res).await? {
                    Kind::Managed => self.fill(name, res).await,
                    Kind::Data => self.refresh(name, res).await,
                }
            }
            .await;
            if let Err(e) = result {
                eprintln!("error: {name}: {e:#}");
                failed.push(name.clone());
            }
        }
        if let Err(e) = self.deliver(true, false).await {
            eprintln!("error: destinations: {e:#}");
            failed.push("destinations".to_owned());
        }
        anyhow::ensure!(failed.is_empty(), "failed: {}", failed.join(", "));
        Ok(())
    }

    async fn ensure_secret(
        &self,
        resource: &str,
        secret: &str,
        readers: BTreeSet<String>,
    ) -> anyhow::Result<()> {
        let labels = self.labels(resource);
        match self.store.get(secret).await? {
            None => {
                self.store.create(secret, &labels).await?;
                println!("created secret {secret}");
            }
            Some(mut meta) => {
                if let Some(other) = self.other_environment(&meta) {
                    // ほかの環境の秘密には触らない（同じプロジェクトを使っている）
                    anyhow::bail!("secret {secret} belongs to environment {other}");
                }
                if let Some(other) = meta.labels.get(RESOURCE).filter(|r| r.as_str() != resource) {
                    // 設定にないリソース（名前を替えた、まとめた）のものなら引き継ぐ。あるリソースのものは取らない
                    anyhow::ensure!(
                        !self.loaded.config.resources.contains_key(other),
                        "secret {secret} belongs to resource {other}, not {resource}"
                    );
                    println!("secret {secret}: moved from resource {other} to {resource}");
                }
                if [MANAGED_BY, RESOURCE, ENV]
                    .iter()
                    .any(|k| labels.contains_key(*k) && meta.labels.get(*k) != labels.get(*k))
                {
                    meta.labels.extend(labels);
                    self.store.update_meta(secret, &meta).await?;
                    println!("labeled secret {secret}");
                }
            }
        }
        if self.store.readers(secret).await? != readers {
            self.store.set_readers(secret, &readers).await?;
            println!(
                "readers of {secret}: {}",
                if readers.is_empty() {
                    "(none)".to_owned()
                } else {
                    readers.iter().cloned().collect::<Vec<_>>().join(", ")
                }
            );
        }
        Ok(())
    }

    /// 値のない作るリソースを作る
    async fn fill(&self, name: &str, res: &Resource) -> anyhow::Result<()> {
        let mut missing = Vec::new();
        for secret in res.secrets.keys() {
            if latest_enabled(self.store, secret).await?.is_none() {
                missing.push(secret.clone());
            }
        }
        if missing.len() == res.secrets.len() {
            if res.provider == "manual" {
                println!("{name}: no value yet (put it with `tegata set {name}`)");
                return Ok(());
            }
            self.new_generation(name, res, false).await?;
        } else if !missing.is_empty() {
            println!(
                "{name}: {} have no value but the others do; run `tegata rotate {name}` to make them together",
                missing.join(", ")
            );
        }
        if let Some(prev) = &res.previous {
            if latest_enabled(self.store, prev).await?.is_none() {
                self.copy_to_previous(res).await?;
            }
        }
        Ok(())
    }

    /// data を読み直し、変わった秘密に新しい版を足して、前の版を無効にする（秘密でない値が主）
    async fn refresh(&self, name: &str, res: &Resource) -> anyhow::Result<()> {
        let outputs = self.providers.read(name, res).await?;
        for (secret, out) in &res.secrets {
            let value = outputs
                .get(out)
                .ok_or_else(|| anyhow::anyhow!("{name}: provider returned no {out}"))?;
            if self.store.access(secret, None).await?.as_deref() == Some(value.as_str()) {
                continue;
            }
            let v = self.store.add_version(secret, value).await?;
            for old in self.store.versions(secret).await? {
                if old.enabled && old.number != v {
                    self.store.disable(secret, old.number).await?;
                }
            }
            println!("{secret}: version {v} (from {name})");
        }
        Ok(())
    }

    async fn copy_to_previous(&self, res: &Resource) -> anyhow::Result<()> {
        let prev = res.previous.as_deref().expect("checked");
        let current = self
            .store
            .access(res.primary(), None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("{} has no value to copy", res.primary()))?;
        if self.store.access(prev, None).await?.as_deref() != Some(current.as_str()) {
            let v = self.store.add_version(prev, &current).await?;
            println!("{prev}: version {v} (the value of {})", res.primary());
        }
        Ok(())
    }

    /// 新しい世代を作る。rotate なら、previous に今の値を先に写す
    async fn new_generation(&self, name: &str, res: &Resource, rotate: bool) -> anyhow::Result<()> {
        if rotate
            && res.previous.is_some()
            && latest_enabled(self.store, res.primary()).await?.is_some()
        {
            self.copy_to_previous(res).await?;
        }
        let created = self.providers.create(name, res).await?;
        for out in res.secrets.values() {
            if !created.outputs.contains_key(out) {
                // 作ったものを残さない
                let _ = self
                    .providers
                    .delete(name, res, &created.state, &serde_json::Value::Null)
                    .await;
                anyhow::bail!("{name}: provider returned no output {out}");
            }
        }
        let mut record = Generation {
            state: created.state.clone(),
            ..Default::default()
        };
        for (secret, out) in &res.secrets {
            let v = self
                .store
                .add_version(secret, &created.outputs[out])
                .await?;
            record.versions.insert(secret.clone(), v);
            println!("{secret}: version {v}");
        }
        self.record(name, res, record).await
    }

    /// 世代を最初の秘密の注記に残す。残せなければ state を出す（手で消せるように）
    async fn record(&self, name: &str, res: &Resource, record: Generation) -> anyhow::Result<()> {
        let result = async {
            let mut meta = self
                .store
                .get(res.primary())
                .await?
                .ok_or_else(|| anyhow::anyhow!("secret {} does not exist", res.primary()))?;
            let n = generations(&meta).keys().max().copied().unwrap_or(0) + 1;
            meta.annotations
                .insert(format!("{GEN_PREFIX}{n}"), serde_json::to_string(&record)?);
            self.store.update_meta(res.primary(), &meta).await?;
            println!("{name}: generation {n}");
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = &result {
            eprintln!(
                "error: could not record the generation of {name}: {e:#}\n  provider state (to clean up by hand): {}",
                record.state
            );
        }
        result
    }

    pub async fn rotate(&self, names: &[String]) -> anyhow::Result<()> {
        for name in names {
            let res = self.resource(name)?;
            anyhow::ensure!(
                self.providers.kind(name, res).await? == Kind::Managed,
                "{name} is read from its provider (apply refreshes it)"
            );
        }
        self.confirm_environment(&format!("make a new generation of {}", names.join(", ")))?;
        if !self.approve(names).await? {
            return Ok(());
        }
        for name in names {
            self.new_generation(name, self.resource(name)?, true)
                .await?;
        }
        self.deliver(false, false).await
    }

    /// 値を手で入れる（端末なら聞く、そうでなければ標準入力）。manual のほか、控えておいた乱数の鍵を戻すときにも使う。
    /// 外のプロバイダーのものは、作ったものの記録がないので import と同じ扱い（prune で消さない）
    pub async fn set(&self, name: &str) -> anyhow::Result<()> {
        let res = self.resource(name)?;
        anyhow::ensure!(
            self.providers.kind(name, res).await? == Kind::Managed,
            "{name} is read from its provider (apply refreshes it)"
        );
        self.confirm_environment(&format!("put a new value of {name}"))?;
        let readers = self.desired_readers()?;
        for secret in res.all_secrets() {
            self.ensure_secret(
                name,
                &secret,
                readers.get(&secret).cloned().unwrap_or_default(),
            )
            .await?;
        }
        if res.previous.is_some() && latest_enabled(self.store, res.primary()).await?.is_some() {
            self.copy_to_previous(res).await?;
        }
        let created = crate::provider::manual(res)?;
        let mut record = Generation {
            imported: !is_builtin(&res.provider),
            ..Default::default()
        };
        for (secret, out) in &res.secrets {
            let v = self
                .store
                .add_version(secret, &created.outputs[out])
                .await?;
            record.versions.insert(secret.clone(), v);
            println!("{secret}: version {v}");
        }
        self.record(name, res, record).await?;
        if let Some(prev) = &res.previous {
            if latest_enabled(self.store, prev).await?.is_none() {
                self.copy_to_previous(res).await?;
            }
        }
        self.deliver(false, false).await
    }

    pub async fn settle(&self, names: &[String]) -> anyhow::Result<()> {
        for name in names {
            let res = self.resource(name)?;
            anyhow::ensure!(res.previous.is_some(), "{name} has no previous");
        }
        self.confirm_environment(&format!(
            "make previous equal to the current value of {} (the old value stops being accepted)",
            names.join(", ")
        ))?;
        for name in names {
            self.copy_to_previous(self.resource(name)?).await?;
        }
        self.deliver(false, false).await
    }

    /// 最新でない世代を片付ける（プロバイダーのものを消し、版を無効にする。版は消さない）
    pub async fn prune(&self, names: &[String]) -> anyhow::Result<()> {
        let names: Vec<String> = if names.is_empty() {
            self.loaded.config.resources.keys().cloned().collect()
        } else {
            names.to_vec()
        };
        for name in &names {
            self.resource(name)?;
        }
        self.confirm_environment(&format!(
            "delete what providers made for older generations of {}, and disable their versions",
            names.join(", ")
        ))?;
        for name in &names {
            let res = self.resource(name)?;
            let Some(mut meta) = self.store.get(res.primary()).await? else {
                continue;
            };
            let gens = generations(&meta);
            let Some(&latest) = gens.keys().max() else {
                continue;
            };
            for (n, g) in gens.iter().filter(|(n, _)| **n != latest) {
                if g.imported && !is_builtin(&res.provider) {
                    println!("{name}: generation {n} was imported; delete what it made by hand if needed");
                } else {
                    self.providers
                        .delete(name, res, &g.state, &gens[&latest].state)
                        .await?;
                }
                for (secret, v) in &g.versions {
                    if self
                        .store
                        .versions(secret)
                        .await?
                        .iter()
                        .any(|x| x.number == *v && x.enabled)
                    {
                        self.store.disable(secret, *v).await?;
                        println!("{secret}: disabled version {v}");
                    }
                }
                meta.annotations.remove(&format!("{GEN_PREFIX}{n}"));
                self.store.update_meta(res.primary(), &meta).await?;
                println!("{name}: pruned generation {n}");
            }
            if let Some(prev) = &res.previous {
                if let Some(keep) = latest_enabled(self.store, prev).await? {
                    for v in self.store.versions(prev).await? {
                        if v.enabled && v.number != keep {
                            self.store.disable(prev, v.number).await?;
                            println!("{prev}: disabled version {}", v.number);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 前の形（雛形を手で埋めたファイル）から、値のない秘密に値を入れる。
    /// `dir` のファイルを、`templates` の同じ名前の雛形と突き合わせて値を取り出す
    pub async fn import(&self, templates: &Path, dir: &Path) -> anyhow::Result<()> {
        self.confirm_environment(&format!(
            "put the values in {} into secrets that have none",
            dir.display()
        ))?;
        let mut values = BTreeMap::new();
        for tpl in template::files(templates)? {
            let filled = dir.join(tpl.file_name().expect("a file"));
            let Ok(text) = std::fs::read_to_string(&filled) else {
                continue;
            };
            for (k, v) in template::extract(&std::fs::read_to_string(&tpl)?, &text) {
                values.entry(k).or_insert(v);
            }
        }
        let readers = self.desired_readers()?;
        for (name, res) in &self.loaded.config.resources {
            if self.providers.kind(name, res).await? != Kind::Managed {
                continue;
            }
            for secret in res.all_secrets() {
                self.ensure_secret(
                    name,
                    &secret,
                    readers.get(&secret).cloned().unwrap_or_default(),
                )
                .await?;
            }
            let mut record = Generation {
                imported: true,
                ..Default::default()
            };
            for secret in res.all_secrets() {
                let Some(v) = values.get(&secret) else {
                    continue;
                };
                if latest_enabled(self.store, &secret).await?.is_some() {
                    println!("{secret}: already has a value; skipped");
                    continue;
                }
                let n = self.store.add_version(&secret, v).await?;
                println!("{secret}: version {n} (imported)");
                if res.secrets.contains_key(&secret) {
                    record.versions.insert(secret, n);
                }
            }
            if !record.versions.is_empty() {
                self.record(name, res, record).await?;
            }
        }
        self.deliver(false, false).await
    }

    /// 届け先に書くもの（設定の順）
    fn wanted(&self) -> Vec<Wanted<'a>> {
        let mut out = Vec::new();
        for (dest, d) in &self.loaded.config.destinations {
            for (there, here) in d.secrets() {
                out.push((dest.as_str(), &d.github, there, here));
            }
        }
        out
    }

    /// 届け先の今の秘密（名前は大文字。GitHub は大文字と小文字を区別しない）→ 変えた時刻
    async fn remote(&self, to: &GithubDestination) -> anyhow::Result<BTreeMap<String, String>> {
        Ok(Github::new(&self.github, to)
            .list()
            .await?
            .into_iter()
            .map(|(n, at)| (n.to_ascii_uppercase(), at))
            .collect())
    }

    /// 秘密の最新の有効な版を、届け先に書く（届いていないもの、GitHub で書き換えられたものだけ）。
    /// `cleanup` なら、設定から外したもの（tegata が届けたもの）を届け先から消す。`dry_run` なら出すだけ
    async fn deliver(&self, cleanup: bool, dry_run: bool) -> anyhow::Result<()> {
        let wanted = self.wanted();
        let mut remotes: BTreeMap<GithubDestination, BTreeMap<String, String>> = BTreeMap::new();
        let mut failed = Vec::new();
        // 書いたもの：届け先 → (名前、秘密、版、届け先の名前)
        let mut written: BTreeMap<GithubDestination, Vec<(String, String, u64, String)>> =
            BTreeMap::new();
        for (dest, to, there, here) in &wanted {
            let result: anyhow::Result<()> = async {
                let Some(version) = latest_enabled(self.store, here).await? else {
                    println!("{dest}: {here} has no value yet; not delivered");
                    return Ok(());
                };
                if !remotes.contains_key(*to) {
                    let r = self.remote(to).await?;
                    remotes.insert((*to).clone(), r);
                }
                let remote_at = remotes[*to].get(&there.to_ascii_uppercase());
                let meta = self.store.get(here).await?.unwrap_or_default();
                // Another environment's value must not reach this environment's destinations
                if let Some(other) = self.other_environment(&meta) {
                    anyhow::bail!("secret {here} belongs to environment {other}; not delivered");
                }
                let rec = deliveries(&meta).remove(*dest);
                let same = rec
                    .as_ref()
                    .filter(|r| r.github == **to && r.name.eq_ignore_ascii_case(there));
                let note = match (same, remote_at) {
                    (Some(r), Some(at)) if r.version == version && *at == r.updated_at => {
                        return Ok(())
                    }
                    (Some(r), Some(at)) if *at != r.updated_at => {
                        format!(" (it was changed on GitHub at {at}; overwriting)")
                    }
                    (Some(r), None) if r.version == version => {
                        " (it was missing on GitHub; writing it again)".to_owned()
                    }
                    (None, Some(_)) => {
                        " (it was already on GitHub, not written by tegata; overwriting)".to_owned()
                    }
                    _ => String::new(),
                };
                if dry_run {
                    println!("would deliver {here} v{version} to {to} as {there}{note}");
                    return Ok(());
                }
                let value = self
                    .store
                    .access(here, Some(version))
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("cannot read {here} v{version}"))?;
                Github::new(&self.github, to).put(there, &value).await?;
                println!("delivered {here} v{version} to {to} as {there}{note}");
                written.entry((*to).clone()).or_default().push((
                    dest.to_string(),
                    here.clone(),
                    version,
                    there.clone(),
                ));
                Ok(())
            }
            .await;
            if let Err(e) = result {
                eprintln!("error: {dest}: {here}: {e:#}");
                failed.push(format!("{dest}/{there}"));
            }
        }
        // 書いたあとの時刻を記録する（あとで、GitHub で書き換えられたかを見分けるため）。
        // 届け先の場所か名前を替えたなら、前の場所のものを消す（ほかの届け先が使っていなければ）
        let in_use: BTreeSet<(&GithubDestination, String)> = wanted
            .iter()
            .map(|(_, to, there, _)| (*to, there.to_ascii_uppercase()))
            .collect();
        for (to, items) in written {
            let result: anyhow::Result<()> = async {
                let remote = self.remote(&to).await?;
                for (dest, here, version, there) in items {
                    let updated_at = remote
                        .get(&there.to_ascii_uppercase())
                        .ok_or_else(|| {
                            anyhow::anyhow!("{there} is not on {to} right after writing it")
                        })?
                        .clone();
                    let meta = self.store.get(&here).await?.unwrap_or_default();
                    if let Some(prev) = deliveries(&meta).remove(&dest) {
                        let moved = prev.github != to || !prev.name.eq_ignore_ascii_case(&there);
                        if moved
                            && !in_use.contains(&(&prev.github, prev.name.to_ascii_uppercase()))
                        {
                            Github::new(&self.github, &prev.github)
                                .delete(&prev.name)
                                .await?;
                            println!(
                                "removed {} from {} (destination {dest} moved)",
                                prev.name, prev.github
                            );
                        }
                    }
                    self.update_deliveries(&here, |d| {
                        d.insert(
                            dest.clone(),
                            Delivered {
                                version,
                                github: to.clone(),
                                name: there.clone(),
                                updated_at,
                            },
                        );
                    })
                    .await?;
                }
                Ok(())
            }
            .await;
            if let Err(e) = result {
                eprintln!("error: {to}: could not record the deliveries: {e:#}");
                failed.push(to.to_string());
            }
        }
        if cleanup {
            if let Err(e) = self.remove_undelivered(&wanted, dry_run).await {
                eprintln!("error: removing from destinations: {e:#}");
                failed.push("removing".to_owned());
            }
        }
        anyhow::ensure!(failed.is_empty(), "not delivered: {}", failed.join(", "));
        Ok(())
    }

    /// Removes what tegata delivered and the configuration no longer lists, for the secrets this
    /// configuration owns (`owns`). Deliveries of other configurations and environments that share
    /// the store, and of secrets whose resource was removed from the YAML, are left in place
    async fn remove_undelivered(&self, wanted: &[Wanted<'a>], dry_run: bool) -> anyhow::Result<()> {
        let declared = self.declared();
        let keep: BTreeSet<(&str, &GithubDestination, String, &str)> = wanted
            .iter()
            .map(|(d, to, there, here)| (*d, *to, there.to_ascii_uppercase(), here.as_str()))
            .collect();
        // 同じ場所・同じ名前を、別の届け先がまだ使っているなら、GitHub からは消さない（記録だけ消す）
        let in_use: BTreeSet<(&GithubDestination, String)> = wanted
            .iter()
            .map(|(_, to, there, _)| (*to, there.to_ascii_uppercase()))
            .collect();
        for (secret, meta) in self.store.list().await? {
            if !self.owns(&declared, &secret, &meta) {
                continue;
            }
            let stale: Vec<(String, Delivered)> = deliveries(&meta)
                .into_iter()
                .filter(|(d, r)| {
                    !keep.contains(&(
                        d.as_str(),
                        &r.github,
                        r.name.to_ascii_uppercase(),
                        secret.as_str(),
                    ))
                })
                .collect();
            for (dest, r) in stale {
                let used = in_use.contains(&(&r.github, r.name.to_ascii_uppercase()));
                if dry_run {
                    if !used {
                        println!(
                            "would remove {} from {} ({secret}, destination {dest})",
                            r.name, r.github
                        );
                    }
                    continue;
                }
                if !used {
                    Github::new(&self.github, &r.github).delete(&r.name).await?;
                    println!(
                        "removed {} from {} ({secret}, destination {dest})",
                        r.name, r.github
                    );
                }
                self.update_deliveries(&secret, |d| {
                    d.remove(&dest);
                })
                .await?;
            }
        }
        Ok(())
    }

    async fn update_deliveries(
        &self,
        secret: &str,
        change: impl FnOnce(&mut crate::deliver::Deliveries),
    ) -> anyhow::Result<()> {
        let mut meta = self
            .store
            .get(secret)
            .await?
            .ok_or_else(|| anyhow::anyhow!("secret {secret} does not exist"))?;
        let mut d = deliveries(&meta);
        change(&mut d);
        if d.is_empty() {
            meta.annotations.remove(crate::deliver::ANNOTATION);
        } else {
            meta.annotations.insert(
                crate::deliver::ANNOTATION.to_owned(),
                serde_json::to_string(&d)?,
            );
        }
        self.store.update_meta(secret, &meta).await
    }

    /// 届け先ごとの状態（status の後半）
    async fn destinations_status(&self) -> anyhow::Result<()> {
        let wanted = self.wanted();
        let mut listed: BTreeMap<GithubDestination, anyhow::Result<BTreeMap<String, String>>> =
            BTreeMap::new();
        for (dest, d) in &self.loaded.config.destinations {
            if !listed.contains_key(&d.github) {
                listed.insert(d.github.clone(), self.remote(&d.github).await);
            }
            println!("destination {dest} ({})", d.github);
            let remote = match &listed[&d.github] {
                Ok(r) => r,
                Err(e) => {
                    println!("  cannot read: {e:#}");
                    continue;
                }
            };
            for (_, _, there, here) in wanted.iter().filter(|w| w.0 == dest) {
                let latest = latest_enabled(self.store, here).await?;
                let meta = self.store.get(here).await?.unwrap_or_default();
                let rec = deliveries(&meta)
                    .remove(dest.as_str())
                    .filter(|r| r.github == d.github && r.name.eq_ignore_ascii_case(there));
                let at = remote.get(&there.to_ascii_uppercase());
                let state = match (latest, rec, at) {
                    (None, _, _) => format!("{here} has no value yet"),
                    (_, Some(r), Some(at)) if *at != r.updated_at => {
                        format!(
                            "changed on GitHub at {at} (apply writes v{} again)",
                            latest.unwrap_or(r.version)
                        )
                    }
                    (_, Some(_), None) => "missing on GitHub (apply writes it again)".to_owned(),
                    (Some(v), Some(r), Some(_)) if r.version == v => format!("v{v}"),
                    (Some(v), Some(r), Some(_)) => {
                        format!("v{} (the store has v{v}; apply delivers it)", r.version)
                    }
                    (Some(_), None, Some(_)) => {
                        "on GitHub, not written by tegata (apply overwrites it)".to_owned()
                    }
                    (Some(_), None, None) => "not delivered yet".to_owned(),
                };
                let from = if there == here {
                    String::new()
                } else {
                    format!(" (from {here})")
                };
                println!("  {there:<32} {state}{from}");
            }
            let mine: BTreeSet<String> = wanted
                .iter()
                .filter(|w| *w.1 == d.github)
                .map(|w| w.2.to_ascii_uppercase())
                .collect();
            let others: Vec<&str> = remote
                .keys()
                .filter(|n| !mine.contains(*n))
                .map(String::as_str)
                .collect();
            if !others.is_empty() {
                println!(
                    "  other secrets there (not managed here): {}",
                    others.join(", ")
                );
            }
            // Environment の値は、同じ名前のリポジトリの値より勝つ
            if d.github.environment.is_some() {
                let repo = GithubDestination {
                    repo: d.github.repo.clone(),
                    environment: None,
                };
                if let Ok(at_repo) = self.remote(&repo).await {
                    let shadowed: Vec<&str> = mine
                        .iter()
                        .filter(|n| at_repo.contains_key(*n))
                        .map(String::as_str)
                        .collect();
                    if !shadowed.is_empty() {
                        println!(
                            "  also repository secrets of {} (this environment's values win): {}",
                            repo.repo,
                            shadowed.join(", ")
                        );
                    }
                }
            }
        }
        // Deliveries the configuration no longer lists, for the secrets it owns (what apply removes).
        // Those of secrets it does not declare are shown by `status` with the secret
        let keep: BTreeSet<(&str, &GithubDestination, String)> = wanted
            .iter()
            .map(|(d, to, there, _)| (*d, *to, there.to_ascii_uppercase()))
            .collect();
        let declared = self.declared();
        for (secret, meta) in self.store.list().await? {
            if !self.owns(&declared, &secret, &meta) {
                continue;
            }
            for (dest, r) in deliveries(&meta) {
                if !keep.contains(&(dest.as_str(), &r.github, r.name.to_ascii_uppercase())) {
                    println!(
                        "no longer in the config: {} on {} ({secret}, destination {dest}; apply removes it)",
                        r.name, r.github
                    );
                }
            }
        }
        Ok(())
    }

    /// リソースと秘密ごとの状態
    pub async fn status(&self) -> anyhow::Result<()> {
        let readers = self.loaded.readers()?;
        for (name, res) in &self.loaded.config.resources {
            let gens = match self.store.get(res.primary()).await? {
                Some(m) => generations(&m),
                None => BTreeMap::new(),
            };
            let kind = if res.kind.is_empty() {
                res.provider.clone()
            } else {
                format!("{}/{}", res.provider, res.kind)
            };
            let gen = match gens.keys().max() {
                Some(n) if gens.len() > 1 => {
                    format!("generation {n} (+{} to prune)", gens.len() - 1)
                }
                Some(n) => format!("generation {n}"),
                None => "-".to_owned(),
            };
            println!("{name} ({kind}) {gen}");
            for secret in res.all_secrets() {
                let v = latest_enabled(self.store, &secret).await?;
                let who: Vec<String> = readers
                    .get(&secret)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect();
                println!(
                    "  {secret:<32} {:<12} {}",
                    v.map_or("(no value)".to_owned(), |n| format!("v{n}")),
                    if who.is_empty() {
                        "-".to_owned()
                    } else {
                        who.join(",")
                    }
                );
            }
        }
        let known = self.declared();
        for (secret, meta) in self.store.list().await? {
            if meta.labels.get(MANAGED_BY).map(String::as_str) == Some("tegata")
                && !known.contains(&secret)
            {
                println!(
                    "not in the config: {secret} (resource {})",
                    meta.labels.get(RESOURCE).map_or("?", String::as_str)
                );
                // Its resource was removed, or another configuration sharing the store declares
                // it: apply cannot tell which, so it leaves these deliveries alone
                for (dest, r) in deliveries(&meta) {
                    println!(
                        "  delivered to {} as {} (destination {dest}; apply leaves it)",
                        r.github, r.name
                    );
                }
            }
        }
        self.destinations_status().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_for_the_environment_name() {
        let ask = |answer: &str, terminal: bool, approval: Approval| {
            confirm_name("production", approval, terminal, &mut answer.as_bytes())
        };
        assert!(ask("production\n", true, Approval::Ask).is_ok());
        assert!(ask("  production  \n", true, Approval::Ask).is_ok());
        let wrong = ask("yes\n", true, Approval::Ask).unwrap_err().to_string();
        assert_eq!(wrong, "cancelled");
        assert!(ask("", true, Approval::Ask).is_err(), "no answer");
        let no_terminal = ask("production\n", false, Approval::Ask)
            .unwrap_err()
            .to_string();
        assert!(no_terminal.contains("pass --yes"), "{no_terminal}");
        assert!(
            ask("", false, Approval::Yes).is_ok(),
            "--yes needs no terminal"
        );
    }
}
