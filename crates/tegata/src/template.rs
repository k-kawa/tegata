//! 雛形の `__名前__` と `__名前|sha256__`

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sha2::{Digest, Sha256};

/// 値の加工
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// SHA-256 の 16 進
    Sha256,
}

/// 雛形の中の 1 つ
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    /// `__` から `__` まで（バイトの位置）
    pub start: usize,
    pub end: usize,
    pub name: String,
    pub filter: Option<Filter>,
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// 雛形の中の `__名前__` をすべて
pub fn scan(text: &str) -> Vec<Placeholder> {
    let mut found = Vec::new();
    let mut i = 0;
    while let Some(off) = text[i..].find("__") {
        let start = i + off;
        let inner_start = start + 2;
        let Some(len) = text[inner_start..].find("__") else {
            break;
        };
        let inner = &text[inner_start..inner_start + len];
        let (name, filter) = match inner.split_once('|') {
            Some((n, "sha256")) => (n, Some(Filter::Sha256)),
            Some(_) => ("", None),
            None => (inner, None),
        };
        if valid_name(name) {
            let end = inner_start + len + 2;
            found.push(Placeholder {
                start,
                end,
                name: name.to_owned(),
                filter,
            });
            i = end;
        } else {
            i = start + 1;
        }
    }
    found
}

/// 名前（加工の前のもの）
pub fn names(text: &str) -> BTreeSet<String> {
    scan(text).into_iter().map(|p| p.name).collect()
}

/// ディレクトリの中のファイル（直下だけ）にある名前
pub fn names_in_dir(dir: &Path) -> anyhow::Result<BTreeSet<String>> {
    let mut all = BTreeSet::new();
    for path in files(dir)? {
        all.extend(names(&std::fs::read_to_string(&path)?));
    }
    Ok(all)
}

/// ディレクトリの直下のファイル（名前の順）。シンボリックリンクは、指す先がファイルなら含める
/// （役割どうしで同じ雛形を共に使うため。例：`deploy/env/example/node/` から `../app/`）
pub fn files(dir: &Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();
    for entry in
        std::fs::read_dir(dir).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", dir.display()))?
    {
        let entry = entry?;
        if entry.path().is_file() {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

pub fn sha256_hex(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 埋める。値のない名前は `__名前__` のまま残す
pub fn render(text: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for p in scan(text) {
        out.push_str(&text[last..p.start]);
        match (values.get(&p.name), p.filter) {
            (Some(v), None) => out.push_str(v),
            (Some(v), Some(Filter::Sha256)) => out.push_str(&sha256_hex(v)),
            (None, _) => out.push_str(&text[p.start..p.end]),
        }
        last = p.end;
    }
    out.push_str(&text[last..]);
    out
}

/// 埋めた行から、雛形の行に合わせて値を取り出す（前の形からの import）。雛形の行ごとに、形の合う行を探す
pub fn extract(template: &str, filled: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let filled_lines: Vec<&str> = filled.lines().collect();
    for t in template.lines() {
        let ps = scan(t);
        if ps.is_empty() || ps.iter().any(|p| p.filter.is_some()) {
            continue;
        }
        for line in &filled_lines {
            if let Some(values) = match_line(t, &ps, line) {
                for (p, v) in ps.iter().zip(values) {
                    // 埋めていないもの（前の形の __名前__）は取らない
                    if !(v.starts_with("__") && v.ends_with("__")) {
                        found.entry(p.name.clone()).or_insert(v);
                    }
                }
                break;
            }
        }
    }
    found
}

/// 雛形の行の、名前のあいだの文字が合えば、名前ごとの値（短いほうから合わせる）
fn match_line(template: &str, ps: &[Placeholder], line: &str) -> Option<Vec<String>> {
    let mut literals = Vec::new();
    let mut last = 0;
    for p in ps {
        literals.push(&template[last..p.start]);
        last = p.end;
    }
    let tail = &template[last..];
    let mut rest = line.strip_prefix(literals[0])?;
    let mut values = Vec::new();
    for (i, _) in ps.iter().enumerate() {
        let next = literals.get(i + 1).copied();
        match next {
            Some(lit) if !lit.is_empty() => {
                let at = rest.find(lit)?;
                values.push(rest[..at].to_owned());
                rest = &rest[at + lit.len()..];
            }
            Some(_) => return None, // 名前が続く行は、分けられない
            None => {
                let v = rest.strip_suffix(tail)?;
                values.push(v.to_owned());
                rest = "";
            }
        }
    }
    rest.is_empty().then_some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_and_renders() {
        let t =
            "A=__APP_TOKEN__\nH=__APP_TOKEN|sha256__\nX=__lower__ Y=____ Z=__NOPE|md5__\n__ONLY__";
        let names: Vec<_> = scan(t).into_iter().map(|p| (p.name, p.filter)).collect();
        assert_eq!(
            names,
            vec![
                ("APP_TOKEN".to_owned(), None),
                ("APP_TOKEN".to_owned(), Some(Filter::Sha256)),
                ("ONLY".to_owned(), None)
            ]
        );
        let values = BTreeMap::from([("APP_TOKEN".to_owned(), "t/&\\\"$x".to_owned())]);
        let out = render(t, &values);
        assert!(out.starts_with("A=t/&\\\"$x\nH="));
        assert!(out.contains(&sha256_hex("t/&\\\"$x")));
        assert!(out.ends_with("__ONLY__"));
        assert!(out.contains("X=__lower__ Y=____ Z=__NOPE|md5__"));
    }

    #[test]
    fn extracts_values_from_filled_files() {
        let t = "# c\nK=__KEY__\nJ='{\"a\":\"__A__\",\"b\":\"__B__\"}'\nNEW=__NEW__\n";
        let f = "# c\nJ='{\"a\":\"x\",\"b\":\"y,\\\"z\"}'\nK=v=1\n";
        let got = extract(t, f);
        assert_eq!(got["KEY"], "v=1");
        assert_eq!(got["A"], "x");
        assert_eq!(got["B"], "y,\\\"z");
        assert!(!got.contains_key("NEW"));
        assert_eq!(extract("__P__", "pw")["P"], "pw");
        assert!(extract("K=__K__", "K=__K__").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn lists_files_through_symlinks() {
        let tmp = std::env::temp_dir().join(format!("tegata-files-{}", std::process::id()));
        let (shared, role) = (tmp.join("shared"), tmp.join("role"));
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(role.join("sub")).unwrap();
        std::fs::write(shared.join("a.env"), "A=__A__").unwrap();
        std::fs::write(role.join("b.env"), "B=__B__").unwrap();
        std::os::unix::fs::symlink("../shared/a.env", role.join("a.env")).unwrap();
        std::os::unix::fs::symlink("../shared/gone.env", role.join("gone.env")).unwrap();
        let names: Vec<_> = files(&role)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.env", "b.env"]);
        assert_eq!(
            names_in_dir(&role).unwrap(),
            BTreeSet::from(["A".to_owned(), "B".to_owned()])
        );
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
