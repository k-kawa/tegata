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
}

/// `templates` の直下のファイルを埋めて、`out` に 0600 で書く
pub async fn render_dir(
    store: &dyn Store,
    templates: &Path,
    out: &Path,
) -> anyhow::Result<Rendered> {
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
    create_private_dir(out)?;
    for (f, text) in files.iter().zip(texts) {
        let dest = out.join(f.file_name().expect("a file"));
        write_private(&dest, template::render(&text, &values).as_bytes())?;
    }
    Ok(Rendered {
        files: files.len(),
        missing,
    })
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
