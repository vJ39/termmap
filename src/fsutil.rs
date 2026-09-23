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

    // rename が失敗しても一時ファイルを残さず、置き換え先も壊さない。
    #[test]
    fn failed_rename_removes_the_tmp_file_and_keeps_the_target() {
        let base = tmp_path("renamefail");
        let _ = std::fs::remove_dir_all(&base);
        let target = base.join("dir_in_the_way");
        std::fs::create_dir_all(target.join("child")).unwrap(); // ディレクトリへは rename できない
        assert!(write_atomic(&target, b"data", None).is_err());
        let tmp = base.join(format!(".dir_in_the_way.{}.tmp", std::process::id()));
        assert!(!tmp.exists(), "失敗時に一時ファイルが残っている");
        assert!(target.join("child").is_dir(), "置き換え先が壊れた");
        let _ = std::fs::remove_dir_all(&base);
    }
}

// HOME 依存の保存/読込を実HOMEに触れずに試すためのテスト補助(他モジュールのテストからも使う)。
// 同じプロセス内で HOME を書き換えると並列に走る他のテストまで一時HOMEを見てしまうので、
// HOME を一時ディレクトリにした子プロセスでそのテスト1本だけを走らせ直す。
#[cfg(test)]
pub(crate) mod testing {
    use std::path::Path;

    const MARK: &str = "TERMMAP_TEST_TEMP_HOME";

    // 子プロセスの中(HOME が親の用意した一時ディレクトリ)か。
    fn in_temp_home() -> bool {
        match (std::env::var_os(MARK), std::env::var_os("HOME")) {
            (Some(m), Some(h)) => m == h && Path::new(&h).starts_with(std::env::temp_dir()),
            _ => false,
        }
    }

    // 子プロセスの中なら true を返す(呼び出し側はそのまま本体を実行する)。
    // 親では子プロセスでこのテストを実行し、通ったことを確かめて false を返す。
    // module には module_path!()、test にはテスト関数名を渡す。env は子プロセスへ足す環境変数。
    pub(crate) fn reexec_in_temp_home(module: &str, test: &str, env: &[(&str, &str)]) -> bool {
        if in_temp_home() {
            return true;
        }
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!("termmap_home_{}_{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // libtest のテスト名はクレート名を含まない("termmap::spots::tests" → "spots::tests")。
        let module = module.split_once("::").map_or(module, |(_, m)| m);
        let name = format!("{module}::{test}");
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([name.as_str(), "--exact", "--test-threads=1"]).env("HOME", &home).env(MARK, &home);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let _ = std::fs::remove_dir_all(&home);
        let stdout = String::from_utf8_lossy(&out.stdout);
        // 名前の打ち間違いで0件実行になっても成功扱いにしない。
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "子プロセスのテスト {name} が通らない:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        false
    }
}
