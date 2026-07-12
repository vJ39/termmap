// ファイルのアトミック保存。同ディレクトリの一時ファイルへ書いてから rename で置き換える。
// 書き込み中にクラッシュ/ディスクエラーが起きても、既存ファイルは壊れない(rename は原子的)。
// std のみ・crate:: 参照なし(単体で完結)。

use std::io::Write;
use std::path::Path;

/// `bytes` を `path` へアトミックに書く。親ディレクトリが無ければ作る。
/// `mode` を Some(0o600) 等にすると、unix で一時ファイルに権限を設定してから rename する
/// (APIキーを含む config 用)。非 unix では mode は無視。
pub fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(d) = dir {
        std::fs::create_dir_all(d)?;
    }
    let dir = dir.unwrap_or_else(|| Path::new("."));
    // 一時名は pid + 対象ファイル名で衝突回避(乱数は使わない)。
    let fname = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".into());
    let tmp = dir.join(format!(".{fname}.{}.tmp", std::process::id()));

    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?;
        #[cfg(unix)]
        if let Some(m) = mode {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(m))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        drop(f);
        std::fs::rename(&tmp, path)
    })();

    if res.is_err() {
        let _ = std::fs::remove_file(&tmp); // 失敗時は一時ファイルを残さない
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("termmap_fsutil_{}_{}", std::process::id(), tag))
    }

    #[test]
    fn write_then_read_roundtrip_and_creates_parent() {
        let base = tmp_path("rt");
        let path = base.join("sub").join("f.txt");
        let _ = std::fs::remove_dir_all(&base);
        write_atomic(&path, b"hello", None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn overwrite_replaces_existing() {
        let path = tmp_path("ow.txt");
        write_atomic(&path, b"old", None).unwrap();
        write_atomic(&path, b"new-longer", None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new-longer");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn mode_sets_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp_path("perm.txt");
        write_atomic(&path, b"secret", Some(0o600)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn no_tmp_file_left_behind() {
        let path = tmp_path("clean.txt");
        write_atomic(&path, b"x", None).unwrap();
        let tmp = std::env::temp_dir().join(format!(".{}.{}.tmp",
            path.file_name().unwrap().to_string_lossy(), std::process::id()));
        assert!(!tmp.exists());
        let _ = std::fs::remove_file(&path);
    }
}
