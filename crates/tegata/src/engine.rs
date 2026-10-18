//! サブコマンドの中身

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use tegata_plugin::Kind;

use crate::config::{Loaded, Resource};
use crate::provider::{is_builtin, Providers};
use crate::store::{latest_enabled, SecretMeta, Store};
use crate::template;

const MANAGED_BY: &str = "managed-by";
const RESOURCE: &str = "tegata-resource";
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
}

impl<'a> Engine<'a> {
    pub fn new(loaded: &'a Loaded, store: &'a dyn Store) -> Self {
        Self {
            loaded,
            store,
            providers: Providers::new(loaded),
            approval: Approval::default(),
        }
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
        let roles = self.loaded.role_names()?;
        let mut notice = String::new();
        for (name, res) in &replacing {
            notice.push_str(&format!(
                "{name} ({} {}: {})\n  the old value stops working at once; open connections stay, new ones with the old value fail\n",
                res.provider, res.kind, res.config
            ));
            let secrets = res.all_secrets();
            let readers: Vec<String> = roles
                .iter()
                .filter(|(_, n)| secrets.iter().any(|s| n.contains(s)))
                .map(|(role, _)| format!("{role} ({})", self.loaded.config.roles[role].member))
                .collect();
            if readers.is_empty() {
                notice.push_str("  no role reads it\n");
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

    fn labels(resource: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            (MANAGED_BY.to_owned(), "tegata".to_owned()),
            (RESOURCE.to_owned(), resource.to_owned()),
        ])
    }

    /// 秘密 → 読む相手（雛形にその名前を持つ役割のメンバー）
    fn desired_readers(&self) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
        let mut readers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (role, names) in self.loaded.role_names()? {
            let member = &self.loaded.config.roles[&role].member;
            for n in names {
                readers.entry(n).or_default().insert(member.clone());
            }
        }
        Ok(readers)
    }

    /// 雛形とリソースが合っているか、プロバイダーに型があるか。問題の一覧（空ならよい）
    pub async fn check(&self) -> anyhow::Result<Vec<String>> {
        let mut problems = Vec::new();
        let owned: BTreeSet<String> = self
            .loaded
            .config
            .resources
            .values()
            .flat_map(Resource::all_secrets)
            .collect();
        for (role, names) in self.loaded.role_names()? {
            for n in names.difference(&owned) {
                problems.push(format!(
                    "role {role}: {n} is in the templates but no resource makes it"
                ));
            }
        }
        for (name, res) in &self.loaded.config.resources {
            if let Err(e) = self.providers.kind(name, res).await {
                problems.push(format!("{e:#}"));
            }
        }
        Ok(problems)
    }

    /// 入れ物、ラベル、読む権限をそろえ、値のないリソースを作り、data を読み直す
    /// 失敗したリソースがあっても、ほかは続ける（鍵のないプロバイダーがあっても、ほかをそろえられるように）
    pub async fn apply(&self) -> anyhow::Result<()> {
        if !self.approve(&self.to_fill().await?).await? {
            return Ok(());
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
        anyhow::ensure!(failed.is_empty(), "failed: {}", failed.join(", "));
        Ok(())
    }

    async fn ensure_secret(
        &self,
        resource: &str,
        secret: &str,
        readers: BTreeSet<String>,
    ) -> anyhow::Result<()> {
        let labels = Self::labels(resource);
        match self.store.get(secret).await? {
            None => {
                self.store.create(secret, &labels).await?;
                println!("created secret {secret}");
            }
            Some(mut meta) => {
                if let Some(other) = meta.labels.get(RESOURCE).filter(|r| r.as_str() != resource) {
                    // 設定にないリソース（名前を替えた、まとめた）のものなら引き継ぐ。あるリソースのものは取らない
                    anyhow::ensure!(
                        !self.loaded.config.resources.contains_key(other),
                        "secret {secret} belongs to resource {other}, not {resource}"
                    );
                    println!("secret {secret}: moved from resource {other} to {resource}");
                }
                if meta.labels.get(MANAGED_BY) != labels.get(MANAGED_BY)
                    || meta.labels.get(RESOURCE) != labels.get(RESOURCE)
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
        if !self.approve(names).await? {
            return Ok(());
        }
        for name in names {
            self.new_generation(name, self.resource(name)?, true)
                .await?;
        }
        Ok(())
    }

    /// 値を手で入れる（端末なら聞く、そうでなければ標準入力）。manual のほか、控えておいた乱数の鍵を戻すときにも使う。
    /// 外のプロバイダーのものは、作ったものの記録がないので import と同じ扱い（prune で消さない）
    pub async fn set(&self, name: &str) -> anyhow::Result<()> {
        let res = self.resource(name)?;
        anyhow::ensure!(
            self.providers.kind(name, res).await? == Kind::Managed,
            "{name} is read from its provider (apply refreshes it)"
        );
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
        Ok(())
    }

    pub async fn settle(&self, names: &[String]) -> anyhow::Result<()> {
        for name in names {
            let res = self.resource(name)?;
            anyhow::ensure!(res.previous.is_some(), "{name} has no previous");
            self.copy_to_previous(res).await?;
        }
        Ok(())
    }

    /// 最新でない世代を片付ける（プロバイダーのものを消し、版を無効にする。版は消さない）
    pub async fn prune(&self, names: &[String]) -> anyhow::Result<()> {
        let names: Vec<String> = if names.is_empty() {
            self.loaded.config.resources.keys().cloned().collect()
        } else {
            names.to_vec()
        };
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

    /// 前の形（雛形を手で埋めたファイル）から、値のない秘密に値を入れる
    pub async fn import(&self, role: &str, dir: &Path) -> anyhow::Result<()> {
        let r = self
            .loaded
            .config
            .roles
            .get(role)
            .ok_or_else(|| anyhow::anyhow!("no role {role}"))?;
        let mut values = BTreeMap::new();
        for tpl in template::files(&self.loaded.path(&r.templates))? {
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
        Ok(())
    }

    /// リソースと秘密ごとの状態
    pub async fn status(&self) -> anyhow::Result<()> {
        let readers = self.desired_readers()?;
        let roles: BTreeMap<&String, &String> = self
            .loaded
            .config
            .roles
            .iter()
            .map(|(n, r)| (&r.member, n))
            .collect();
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
                    .map(|m| roles.get(m).map_or(m.clone(), |r| (*r).clone()))
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
        let known: BTreeSet<String> = self
            .loaded
            .config
            .resources
            .values()
            .flat_map(Resource::all_secrets)
            .collect();
        for (secret, meta) in self.store.list().await? {
            if meta.labels.get(MANAGED_BY).map(String::as_str) == Some("tegata")
                && !known.contains(&secret)
            {
                println!(
                    "not in the config: {secret} (resource {})",
                    meta.labels.get(RESOURCE).map_or("?", String::as_str)
                );
            }
        }
        Ok(())
    }
}
