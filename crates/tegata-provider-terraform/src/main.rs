//! tegata のプロバイダー：Terraform の出力を読む
//!
//! 型 `outputs`（data）：`terraform -chdir=<dir> output -json <output>` の、文字列の値の対応（名前 → 値）を返す。
//! config：`dir`（YAML からの相対）、`output`（出力の名前）。状態のバケットを読むだけで、apply はしない

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use tegata_plugin::{Kind, Provider, Request};

struct Terraform;

#[async_trait::async_trait]
impl Provider for Terraform {
    fn describe(&self) -> BTreeMap<String, Kind> {
        BTreeMap::from([("outputs".to_owned(), Kind::Data)])
    }

    async fn read(&self, req: &Request) -> anyhow::Result<BTreeMap<String, String>> {
        let dir = Path::new(&req.base_dir).join(req.config_str("dir")?);
        let output = req.config_str("output")?;
        let bin = std::env::var("TEGATA_TERRAFORM").unwrap_or_else(|_| "terraform".to_owned());
        let out = tokio::process::Command::new(&bin)
            .arg(format!("-chdir={}", dir.display()))
            .args(["output", "-json", output])
            .stderr(std::process::Stdio::inherit())
            .output()
            .await
            .with_context(|| format!("cannot run {bin}"))?;
        anyhow::ensure!(
            out.status.success(),
            "terraform output failed ({})",
            out.status
        );
        parse(&out.stdout)
    }
}

/// 文字列の値だけの対応にする
fn parse(json: &[u8]) -> anyhow::Result<BTreeMap<String, String>> {
    let v: serde_json::Value =
        serde_json::from_slice(json).context("terraform output is not JSON")?;
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("the output must be a map of strings"))?;
    obj.iter()
        .map(|(k, v)| {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => anyhow::bail!("output {k} is not a string"),
            };
            Ok((k.clone(), s))
        })
        .collect()
}

fn main() {
    tegata_plugin::run(Terraform)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_a_map() {
        let m = super::parse(br#"{"A":"x","N":3}"#).unwrap();
        assert_eq!(m["A"], "x");
        assert_eq!(m["N"], "3");
        assert!(super::parse(br#"{"A":{"b":1}}"#).is_err());
        assert!(super::parse(br#"["a"]"#).is_err());
    }
}
