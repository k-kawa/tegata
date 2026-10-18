//! VM の上で、雛形を置き場所の値で埋める

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::store::Store;
use crate::template;

/// 埋めた結果
pub struct Rendered {
    pub files: usize,
    /// 置き場所になかった名前（`__名前__` のまま残した）
    pub missing: BTreeSet<String>,
    /// 中身が変わって書いたファイル（名前の順）。変わらないものは書かない
    pub changed: Vec<String>,
}

/// 埋めた中身（まだ書いていない）
pub struct Filled {
    /// ファイル名 → 中身（雛形の順）
    pub files: Vec<(String, String)>,
    pub missing: BTreeSet<String>,
}

/// `templates` の直下のファイルを埋めて、`out` に 0600 で書く（中身が変わったものだけ）
pub async fn render_dir(
    store: &dyn Store,
    templates: &Path,
    out: &Path,
) -> anyhow::Result<Rendered> {
    let filled = fill_dir(store, templates).await?;
    let changed = write_dir(&filled, out)?;
    Ok(Rendered {
        files: filled.files.len(),
        missing: filled.missing,
        changed,
    })
}

/// `templates` の直下のファイルを埋める。書くのは `write_dir`
pub async fn fill_dir(store: &dyn Store, templates: &Path) -> anyhow::Result<Filled> {
    let files = template::files(templates)?;
    let mut texts = Vec::new();
    let mut names = BTreeSet::new();
    for f in &files {
        let text = std::fs::read_to_string(f)?;
        names.extend(template::names(&text));
        texts.push(text);
    }
    let mut values = BTreeMap::new();
    let mut missing = BTreeSet::new();
    for n in names {
        match store.access(&n, None).await? {
            Some(v) => {
                values.insert(n, v);
            }
            None => {
                missing.insert(n);
            }
        }
    }
    let files = files
        .iter()
        .zip(texts)
        .map(|(f, text)| {
            let name = f
                .file_name()
                .expect("a file")
                .to_string_lossy()
                .into_owned();
            (name, template::render(&text, &values))
        })
        .collect();
    Ok(Filled { files, missing })
}

/// 埋めたものを `out` に書く。今あるファイルと中身が同じなら書かない（時刻も変わらない）。
/// 書いたファイルの名前を返す
pub fn write_dir(filled: &Filled, out: &Path) -> anyhow::Result<Vec<String>> {
    create_private_dir(out)?;
    let mut changed = Vec::new();
    for (name, text) in &filled.files {
        let dest = out.join(name);
        if std::fs::read(&dest).ok().as_deref() == Some(text.as_bytes()) {
            continue;
        }
        write_private(&dest, text.as_bytes())?;
        changed.push(name.clone());
    }
    changed.sort();
    Ok(changed)
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tegata-tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&tmp)?.write_all(contents)?;
    std::fs::rename(tmp, path)
}
