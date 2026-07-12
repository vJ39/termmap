//! Self-contained search-result cache for termmap.
//!
//! Standard library only, no external crates, no `crate::` references —
//! this file is designed to be compiled and tested on its own with:
//!
//!     rustc --edition 2021 --test src/searchcache.rs -o /tmp/tm_cache && /tmp/tm_cache
//!
//! Purpose: cache geocode/search results keyed by (keyword, position) so
//! repeated searches near the same spot for the same query don't re-hit
//! the API. The cache is persisted as a flat TSV file:
//!
//!     key\tlat\tlon\tname
//!
//! One line per cached result. Multiple results for the same key share
//! that key across multiple lines. `key` itself is produced by
//! [`make_key`] and may contain embedded tab characters (it is
//! `query\t{lat:.2}\t{lon:.2}`), so parsing splits each line from the
//! *right* (name, then lon, then lat, then "everything else" = key) —
//! that keeps round-tripping correct regardless of what's inside `key`.
//! `name` has any tab/newline characters replaced with spaces before
//! being written, since each cache entry must stay on a single line.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// キャッシュに書く最大エントリ数(ファイル肥大防止)。座標は経年で陳腐化しないので TTL は設けない。
const MAX_CACHE_ENTRIES: usize = 3000;

/// tmp へ書いてから rename する原子的保存(このファイルは crate:: 非依存で単体テスト可能に保つため自前)。
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(d) = dir {
        std::fs::create_dir_all(d)?;
    }
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "cache".into());
    let tmp = dir.join(format!(".{fname}.{}.tmp", std::process::id()));
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Returns `$HOME/.config/termmap/search-cache.tsv`, or `None` if `HOME`
/// is unset/empty.
pub fn cache_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("termmap");
    p.push("search-cache.tsv");
    Some(p)
}

/// Rounds to 2 decimal places (~1km grid at typical latitudes).
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Builds a cache key from a query string and a position. The query is
/// trimmed and lower-cased; the position is rounded to 2 decimal places
/// (~1km grid cells) so nearby lookups for the same query share a cache
/// entry. Format: `"{query}\t{lat:.2}\t{lon:.2}"`.
pub fn make_key(query: &str, lat: f64, lon: f64) -> String {
    let q = query.trim().to_lowercase();
    let rlat = round2(lat);
    let rlon = round2(lon);
    format!("{}\t{:.2}\t{:.2}", q, rlat, rlon)
}

/// Replaces tab/newline/carriage-return characters with spaces so a name
/// can never break the one-entry-per-line TSV format.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '\t' | '\n' | '\r' => ' ',
            other => other,
        })
        .collect()
}

/// Loads the cache from `path`. Each line is `key\tlat\tlon\tname`, where
/// `key` may itself contain tab characters (see module docs), so lines
/// are parsed from the right: the last 3 tab-separated fields are name,
/// lon, lat (in that order), and whatever remains at the start of the
/// line — tabs and all — is the key. Lines that don't split into at
/// least 4 parts this way, or whose lat/lon fields don't parse as
/// numbers, are skipped. A missing file yields an empty map.
pub fn load_from(path: &Path) -> HashMap<String, Vec<(f64, f64, String)>> {
    let mut map: HashMap<String, Vec<(f64, f64, String)>> = HashMap::new();

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return map,
    };

    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        // Split from the right into at most 4 pieces: [name, lon, lat, key].
        // rsplitn yields the rightmost piece first; the final piece
        // absorbs everything left over (including any tabs inside key).
        let parts: Vec<&str> = line.rsplitn(4, '\t').collect();
        if parts.len() != 4 {
            continue;
        }
        let name = parts[0];
        let lon_str = parts[1];
        let lat_str = parts[2];
        let key = parts[3];

        let lon: f64 = match lon_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let lat: f64 = match lat_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        map.entry(key.to_string())
            .or_insert_with(Vec::new)
            .push((lat, lon, name.to_string()));
    }

    map
}

/// Writes `map` to `path` as TSV (`key\tlat\tlon\tname` per entry),
/// creating the parent directory if needed. `name` is sanitized (tabs
/// and newlines become spaces) before writing so each entry stays on one
/// line and round-trips cleanly through [`load_from`].
pub fn save_to(
    path: &Path,
    map: &HashMap<String, Vec<(f64, f64, String)>>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create cache dir {}: {}", parent.display(), e))?;
        }
    }

    // ファイル肥大防止の件数上限。座標は経年で陳腐化しないため per-entry TTL は設けず、
    // 総エントリ数だけ MAX_CACHE_ENTRIES で頭打ちにする(超過分は書かない)。
    let mut out = String::new();
    let mut written = 0usize;
    'outer: for (key, entries) in map {
        for (lat, lon, name) in entries {
            if written >= MAX_CACHE_ENTRIES {
                break 'outer;
            }
            out.push_str(key);
            out.push('\t');
            out.push_str(&lat.to_string());
            out.push('\t');
            out.push_str(&lon.to_string());
            out.push('\t');
            out.push_str(&sanitize_name(name));
            out.push('\n');
            written += 1;
        }
    }

    atomic_write(path, out.as_bytes())
        .map_err(|e| format!("failed to write cache {}: {}", path.display(), e))
}

/// Loads the cache from the default location ([`cache_path`]). Returns
/// an empty map if `HOME` is unset or the file doesn't exist/can't be read.
pub fn load() -> HashMap<String, Vec<(f64, f64, String)>> {
    match cache_path() {
        Some(p) => load_from(&p),
        None => HashMap::new(),
    }
}

/// Saves the cache to the default location ([`cache_path`]).
pub fn save(map: &HashMap<String, Vec<(f64, f64, String)>>) -> Result<(), String> {
    match cache_path() {
        Some(p) => save_to(&p, map),
        None => Err("cannot determine cache path: HOME is unset".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Generates a unique path under the OS temp dir so tests never touch
    /// the real `$HOME/.config/termmap/search-cache.tsv` and never
    /// collide with each other when run in parallel.
    fn unique_temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let mut p = std::env::temp_dir();
        p.push(format!("termmap_searchcache_test_{}_{}_{}.tsv", tag, pid, n));
        p
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn make_key_trims_and_lowercases_query() {
        let k = make_key("  Tokyo Tower  ", 35.681236, 139.767125);
        assert_eq!(k, "tokyo tower\t35.68\t139.77");
    }

    #[test]
    fn make_key_rounds_to_two_decimals_about_1km_grid() {
        // 35.681236 -> 35.68, 139.767125 -> 139.77
        assert_eq!(make_key("x", 35.681236, 139.767125), "x\t35.68\t139.77");
        // A value that rounds down.
        assert_eq!(make_key("x", 35.684, 139.764), "x\t35.68\t139.76");
    }

    #[test]
    fn make_key_is_stable_and_groups_nearby_points() {
        let k1 = make_key("cafe", 35.6812, 139.7671);
        let k2 = make_key("cafe", 35.6812, 139.7671);
        // Same call twice yields identical output.
        assert_eq!(k1, k2);

        // Two points within the same ~1km grid cell collapse to the same key.
        let a = make_key("Cafe", 35.6809, 139.7671);
        let b = make_key("cafe", 35.6812, 139.7674);
        assert_eq!(a, b);
    }

    #[test]
    fn save_and_load_round_trip() {
        let path = unique_temp_path("roundtrip");
        cleanup(&path);

        let mut map: HashMap<String, Vec<(f64, f64, String)>> = HashMap::new();
        map.insert(
            make_key("cafe", 35.68, 139.76),
            vec![
                (35.6809, 139.7671, "Cafe A".to_string()),
                (35.6812, 139.7674, "Cafe B".to_string()),
            ],
        );
        map.insert(
            make_key("station", 34.99, 135.75),
            vec![(34.9855, 135.7588, "Kyoto Station".to_string())],
        );

        save_to(&path, &map).expect("save_to should succeed");
        let loaded = load_from(&path);

        assert_eq!(loaded.len(), map.len());
        for (key, mut entries) in map {
            let mut loaded_entries = loaded.get(&key).expect("key present after round trip").clone();
            entries.sort_by(|a, b| a.2.cmp(&b.2));
            loaded_entries.sort_by(|a, b| a.2.cmp(&b.2));
            assert_eq!(loaded_entries, entries);
        }

        cleanup(&path);
    }

    #[test]
    fn load_from_missing_file_returns_empty_map() {
        let path = unique_temp_path("missing");
        cleanup(&path); // ensure it doesn't exist

        let loaded = load_from(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn load_from_skips_malformed_lines() {
        let path = unique_temp_path("malformed");
        cleanup(&path);

        let good_key = make_key("shop", 35.0, 139.0);
        let content = format!(
            "{}\t35.00\t139.00\tGood Shop\nnot enough fields\n{}\tNaNlat\t139.00\tBad Lat\n\n",
            good_key, good_key
        );
        std::fs::write(&path, content).expect("write test file");

        let loaded = load_from(&path);
        // Only the well-formed line should survive.
        assert_eq!(loaded.len(), 1);
        let entries = loaded.get(&good_key).expect("good key present");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], (35.0, 139.0, "Good Shop".to_string()));

        cleanup(&path);
    }

    #[test]
    fn save_to_sanitizes_tabs_and_newlines_in_name() {
        let path = unique_temp_path("sanitize");
        cleanup(&path);

        let mut map: HashMap<String, Vec<(f64, f64, String)>> = HashMap::new();
        let key = make_key("weird", 1.0, 2.0);
        map.insert(key.clone(), vec![(1.0, 2.0, "Hello\tWorld\nFoo\r\nBar".to_string())]);

        save_to(&path, &map).expect("save_to should succeed");
        let loaded = load_from(&path);

        let entries = loaded.get(&key).expect("key present");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].2, "Hello World Foo  Bar");
        assert!(!entries[0].2.contains('\t'));
        assert!(!entries[0].2.contains('\n'));
        assert!(!entries[0].2.contains('\r'));

        cleanup(&path);
    }

    #[test]
    fn save_to_creates_parent_directory() {
        let mut path = std::env::temp_dir();
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        path.push(format!(
            "termmap_searchcache_test_mkdir_{}_{}",
            std::process::id(),
            n
        ));
        let dir = path.clone();
        path.push("search-cache.tsv");

        let _ = std::fs::remove_dir_all(&dir);
        assert!(!dir.exists());

        let map: HashMap<String, Vec<(f64, f64, String)>> = HashMap::new();
        save_to(&path, &map).expect("save_to should create parent dir and succeed");
        assert!(path.exists());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
