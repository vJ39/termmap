//! Self-contained search-result cache for termmap.
//!
//! Standard library only, no external crates, no `crate::` references —
//! this file is designed to be compiled and tested on its own with:
//!
//!     rustc --edition 2021 --test src/searchcache.rs -o /tmp/tm_cache && /tmp/tm_cache
//!
//! Purpose: cache geocode/search results keyed by (provider, language,
//! keyword, position) so repeated searches near the same spot for the
//! same query don't re-hit the API. The cache is persisted as a flat TSV
//! file whose first line is a format-version header:
//!
//!     #termmap-search-cache v2
//!     key\tlat\tlon\tname\tcreated_at\tlast_used_at
//!
//! One line per cached result. Multiple results for the same key share
//! that key across multiple lines (they also share the same created_at /
//! last_used_at). `key` itself is produced by [`make_key`] and contains
//! embedded tab characters (it is
//! `provider\tlang\tquery\t{lat:.2}\t{lon:.2}`), so parsing splits each
//! line from the *right* (last_used_at, created_at, name, lon, lat, then
//! "everything else" = key) — that keeps round-tripping correct
//! regardless of what's inside `key`. `name` has any tab/newline
//! characters replaced with spaces before being written, since each cache
//! entry must stay on a single line.
//!
//! Version failover: if the header line does not match the current
//! [`CACHE_VERSION`] (this includes older, header-less files), the whole
//! file is ignored and an empty cache is returned — so a change in cache
//! semantics automatically invalidates on-disk data instead of mixing
//! incompatible formats.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 現行のキャッシュ形式バージョン。ロジック/形式を変えたら上げること(旧ファイルは自動失効する)。
pub const CACHE_VERSION: &str = "v2";
/// ファイル先頭のヘッダ行の接頭辞(このバージョンで書く/このバージョンだけ読む)。
const HEADER_PREFIX: &str = "#termmap-search-cache";

/// キャッシュに書く最大エントリ(行)数(ファイル肥大防止)。溢れたら last_used が古い順に捨てる。
const MAX_CACHE_ENTRIES: usize = 3000;
/// エントリの生存期間(秒)。created_at からこの期間を過ぎたら読み込み時に捨てる(90日)。
const TTL_SECS: u64 = 90 * 24 * 3600;

/// 1キャッシュキーに対する結果一式＋タイムスタンプ(epoch秒)。
/// created_at=最初に保存した時刻 / last_used_at=最後に参照した時刻(LRU破棄に使う)。
#[derive(Clone, Debug, PartialEq)]
pub struct CacheEntry {
    pub results: Vec<(f64, f64, String)>,
    pub created_at: u64,
    pub last_used_at: u64,
}

/// 現在時刻の epoch 秒。取得できない環境では 0 を返す(この module 内で完結させるため std のみ使用)。
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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

/// Builds a cache key from a provider tag, language, query string and a
/// position. `provider` (例 "g"=Google / "n"=Nominatim) と `lang` (例 "ja")
/// を先頭に織り込むので、検索元/言語が違えばキャッシュ空間が分かれる。The
/// query is trimmed and lower-cased; the position is rounded to 2 decimal
/// places (~1km grid cells) so nearby lookups for the same query share a
/// cache entry. Format: `"{provider}\t{lang}\t{query}\t{lat:.2}\t{lon:.2}"`.
pub fn make_key(provider: &str, lang: &str, query: &str, lat: f64, lon: f64) -> String {
    let q = query.trim().to_lowercase();
    let rlat = round2(lat);
    let rlon = round2(lon);
    format!("{}\t{}\t{}\t{:.2}\t{:.2}", provider, lang, q, rlat, rlon)
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

/// Loads the cache from `path`. The first line must be the current version
/// header (`#termmap-search-cache v2`); if it doesn't match — including
/// older, header-less files — an empty cache is returned so incompatible
/// on-disk data is discarded. Each remaining line is
/// `key\tlat\tlon\tname\tcreated_at\tlast_used_at`, where `key` may itself
/// contain tab characters (see module docs), so lines are parsed from the
/// right: the last 5 tab-separated fields are last_used_at, created_at,
/// name, lon, lat (in that order), and whatever remains at the start of
/// the line — tabs and all — is the key. Lines that don't split into at
/// least 6 parts this way, or whose numeric fields don't parse, are
/// skipped. Entries older than [`TTL_SECS`] (by created_at) are dropped. A
/// missing file yields an empty map.
pub fn load_from(path: &Path) -> HashMap<String, CacheEntry> {
    let mut map: HashMap<String, CacheEntry> = HashMap::new();

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return map,
    };

    let mut lines = content.lines();
    // 先頭ヘッダ行のバージョン照合。不一致(旧形式のヘッダ無しファイル含む)は全無視して空にする。
    match lines.next() {
        Some(h) if h == format!("{HEADER_PREFIX} {CACHE_VERSION}") => {}
        _ => return map,
    }

    let now = now_secs();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        // Split from the right into at most 6 pieces:
        // [last_used_at, created_at, name, lon, lat, key]. rsplitn yields
        // the rightmost piece first; the final piece absorbs everything
        // left over (including any tabs inside key).
        let parts: Vec<&str> = line.rsplitn(6, '\t').collect();
        if parts.len() != 6 {
            continue;
        }
        let last_used_str = parts[0];
        let created_str = parts[1];
        let name = parts[2];
        let lon_str = parts[3];
        let lat_str = parts[4];
        let key = parts[5];

        let last_used: u64 = match last_used_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let created: u64 = match created_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let lon: f64 = match lon_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let lat: f64 = match lat_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        // TTL: created_at から TTL_SECS を過ぎたエントリは捨てる(クロック巻き戻し耐性で saturating)。
        if now.saturating_sub(created) > TTL_SECS {
            continue;
        }

        let e = map.entry(key.to_string()).or_insert_with(|| CacheEntry {
            results: Vec::new(),
            created_at: created,
            last_used_at: last_used,
        });
        e.results.push((lat, lon, name.to_string()));
        // 同一キーの複数行はタイムスタンプを共有する想定だが、念のため最古created/最新last_usedを採る。
        e.created_at = e.created_at.min(created);
        e.last_used_at = e.last_used_at.max(last_used);
    }

    map
}

/// Writes `map` to `path` as TSV, creating the parent directory if needed.
/// The current version header is written first. `name` is sanitized (tabs
/// and newlines become spaces) before writing so each entry stays on one
/// line and round-trips cleanly through [`load_from`]. If more than
/// [`MAX_CACHE_ENTRIES`] result lines would be written, keys are kept
/// newest-first by `last_used_at` (LRU: oldest-used keys are dropped).
pub fn save_to(path: &Path, map: &HashMap<String, CacheEntry>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create cache dir {}: {}", parent.display(), e))?;
        }
    }

    // last_used が新しいキー順に並べ、行数上限まで書く(溢れた=古いキーは捨てる)。
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_by(|a, b| {
        let la = map[*a].last_used_at;
        let lb = map[*b].last_used_at;
        lb.cmp(&la).then_with(|| a.cmp(b)) // 新しい順。同時刻はキーで安定化。
    });

    let mut out = String::new();
    out.push_str(HEADER_PREFIX);
    out.push(' ');
    out.push_str(CACHE_VERSION);
    out.push('\n');

    let mut written = 0usize;
    'outer: for key in keys {
        let e = &map[key];
        for (lat, lon, name) in &e.results {
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
            out.push('\t');
            out.push_str(&e.created_at.to_string());
            out.push('\t');
            out.push_str(&e.last_used_at.to_string());
            out.push('\n');
            written += 1;
        }
    }

    atomic_write(path, out.as_bytes())
        .map_err(|e| format!("failed to write cache {}: {}", path.display(), e))
}

/// Loads the cache from the default location ([`cache_path`]). Returns
/// an empty map if `HOME` is unset or the file doesn't exist/can't be read.
pub fn load() -> HashMap<String, CacheEntry> {
    match cache_path() {
        Some(p) => load_from(&p),
        None => HashMap::new(),
    }
}

/// Saves the cache to the default location ([`cache_path`]).
pub fn save(map: &HashMap<String, CacheEntry>) -> Result<(), String> {
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

    /// 指定タイムスタンプの CacheEntry を作るテスト用ヘルパ。
    fn entry(results: Vec<(f64, f64, String)>, created: u64, last_used: u64) -> CacheEntry {
        CacheEntry { results, created_at: created, last_used_at: last_used }
    }

    #[test]
    fn make_key_trims_and_lowercases_query() {
        let k = make_key("g", "ja", "  Tokyo Tower  ", 35.681236, 139.767125);
        assert_eq!(k, "g\tja\ttokyo tower\t35.68\t139.77");
    }

    #[test]
    fn make_key_rounds_to_two_decimals_about_1km_grid() {
        // 35.681236 -> 35.68, 139.767125 -> 139.77
        assert_eq!(make_key("g", "ja", "x", 35.681236, 139.767125), "g\tja\tx\t35.68\t139.77");
        // A value that rounds down.
        assert_eq!(make_key("g", "ja", "x", 35.684, 139.764), "g\tja\tx\t35.68\t139.76");
    }

    #[test]
    fn make_key_separates_provider_and_language() {
        // provider/言語が違えばキー空間が分かれる(Google と Nominatim の結果が混ざらない)。
        let g = make_key("g", "ja", "cafe", 35.68, 139.76);
        let n = make_key("n", "ja", "cafe", 35.68, 139.76);
        assert_ne!(g, n);
        let en = make_key("g", "en", "cafe", 35.68, 139.76);
        assert_ne!(g, en);
    }

    #[test]
    fn make_key_is_stable_and_groups_nearby_points() {
        let k1 = make_key("g", "ja", "cafe", 35.6812, 139.7671);
        let k2 = make_key("g", "ja", "cafe", 35.6812, 139.7671);
        // Same call twice yields identical output.
        assert_eq!(k1, k2);

        // Two points within the same ~1km grid cell collapse to the same key.
        let a = make_key("g", "ja", "Cafe", 35.6809, 139.7671);
        let b = make_key("g", "ja", "cafe", 35.6812, 139.7674);
        assert_eq!(a, b);
    }

    #[test]
    fn save_and_load_round_trip() {
        let path = unique_temp_path("roundtrip");
        cleanup(&path);

        let now = now_secs();
        let mut map: HashMap<String, CacheEntry> = HashMap::new();
        map.insert(
            make_key("g", "ja", "cafe", 35.68, 139.76),
            entry(
                vec![
                    (35.6809, 139.7671, "Cafe A".to_string()),
                    (35.6812, 139.7674, "Cafe B".to_string()),
                ],
                now,
                now,
            ),
        );
        map.insert(
            make_key("n", "ja", "station", 34.99, 135.75),
            entry(vec![(34.9855, 135.7588, "Kyoto Station".to_string())], now, now),
        );

        save_to(&path, &map).expect("save_to should succeed");
        let loaded = load_from(&path);

        assert_eq!(loaded.len(), map.len());
        for (key, mut expected) in map {
            let got = loaded.get(&key).expect("key present after round trip");
            assert_eq!(got.created_at, expected.created_at);
            assert_eq!(got.last_used_at, expected.last_used_at);
            let mut got_results = got.results.clone();
            expected.results.sort_by(|a, b| a.2.cmp(&b.2));
            got_results.sort_by(|a, b| a.2.cmp(&b.2));
            assert_eq!(got_results, expected.results);
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
    fn load_from_wrong_version_header_returns_empty_map() {
        // 形式バージョンが違う(=ロジック変更で失効すべき)ファイルは全無視して空になる。
        let path = unique_temp_path("badversion");
        cleanup(&path);

        let key = make_key("g", "ja", "shop", 35.0, 139.0);
        let now = now_secs();
        let content = format!(
            "#termmap-search-cache v1\n{key}\t35.00\t139.00\tOld Shop\t{now}\t{now}\n"
        );
        std::fs::write(&path, content).expect("write test file");

        let loaded = load_from(&path);
        assert!(loaded.is_empty(), "version mismatch must invalidate the whole file");

        cleanup(&path);
    }

    #[test]
    fn load_from_legacy_headerless_file_returns_empty_map() {
        // 旧形式(ヘッダ無し)は先頭行がヘッダに一致しないため空扱い(=作り直される)。
        let path = unique_temp_path("legacy");
        cleanup(&path);

        std::fs::write(&path, "somequery\t35.68\t139.76\tName\n").expect("write test file");
        let loaded = load_from(&path);
        assert!(loaded.is_empty());

        cleanup(&path);
    }

    #[test]
    fn load_from_drops_ttl_expired_entries() {
        // created_at が TTL を大きく過ぎたエントリは読み込み時に捨てられる。
        let path = unique_temp_path("ttl");
        cleanup(&path);

        let now = now_secs();
        let fresh_key = make_key("g", "ja", "fresh", 35.0, 139.0);
        let stale_key = make_key("g", "ja", "stale", 36.0, 140.0);
        let stale_created = now.saturating_sub(TTL_SECS + 10_000);
        let content = format!(
            "#termmap-search-cache {CACHE_VERSION}\n\
             {fresh_key}\t35.00\t139.00\tFresh\t{now}\t{now}\n\
             {stale_key}\t36.00\t140.00\tStale\t{stale_created}\t{stale_created}\n"
        );
        std::fs::write(&path, content).expect("write test file");

        let loaded = load_from(&path);
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains_key(&fresh_key));
        assert!(!loaded.contains_key(&stale_key));

        cleanup(&path);
    }

    #[test]
    fn load_from_skips_malformed_lines() {
        let path = unique_temp_path("malformed");
        cleanup(&path);

        let good_key = make_key("g", "ja", "shop", 35.0, 139.0);
        let now = now_secs();
        let content = format!(
            "#termmap-search-cache {CACHE_VERSION}\n\
             {good_key}\t35.00\t139.00\tGood Shop\t{now}\t{now}\n\
             not enough fields\n\
             {good_key}\tNaNlat\t139.00\tBad Lat\t{now}\t{now}\n\
             \n"
        );
        std::fs::write(&path, content).expect("write test file");

        let loaded = load_from(&path);
        // Only the well-formed line should survive.
        assert_eq!(loaded.len(), 1);
        let e = loaded.get(&good_key).expect("good key present");
        assert_eq!(e.results.len(), 1);
        assert_eq!(e.results[0], (35.0, 139.0, "Good Shop".to_string()));

        cleanup(&path);
    }

    #[test]
    fn save_to_sanitizes_tabs_and_newlines_in_name() {
        let path = unique_temp_path("sanitize");
        cleanup(&path);

        let now = now_secs();
        let mut map: HashMap<String, CacheEntry> = HashMap::new();
        let key = make_key("g", "ja", "weird", 1.0, 2.0);
        map.insert(key.clone(), entry(vec![(1.0, 2.0, "Hello\tWorld\nFoo\r\nBar".to_string())], now, now));

        save_to(&path, &map).expect("save_to should succeed");
        let loaded = load_from(&path);

        let e = loaded.get(&key).expect("key present");
        assert_eq!(e.results.len(), 1);
        assert_eq!(e.results[0].2, "Hello World Foo  Bar");
        assert!(!e.results[0].2.contains('\t'));
        assert!(!e.results[0].2.contains('\n'));
        assert!(!e.results[0].2.contains('\r'));

        cleanup(&path);
    }

    #[test]
    fn save_to_evicts_oldest_used_when_over_capacity() {
        // 上限超過時は last_used が新しいキーを残し、古いキーを捨てる。
        let path = unique_temp_path("evict");
        cleanup(&path);

        let now = now_secs();
        let mut map: HashMap<String, CacheEntry> = HashMap::new();
        // MAX_CACHE_ENTRIES + 1 個のキー(各1件)を作る。created は全て新しく(TTLで落ちない)、last_used を昇順に振る。
        for i in 0..=MAX_CACHE_ENTRIES {
            let key = make_key("g", "ja", &format!("q{i}"), 35.0, 139.0);
            map.insert(key, entry(vec![(35.0, 139.0, format!("N{i}"))], now, now.saturating_sub(MAX_CACHE_ENTRIES as u64) + i as u64));
        }
        save_to(&path, &map).expect("save_to should succeed");
        let loaded = load_from(&path);

        // 行数は上限ちょうど。最も古い last_used=0 のキーが落ちている。
        assert_eq!(loaded.len(), MAX_CACHE_ENTRIES);
        let oldest = make_key("g", "ja", "q0", 35.0, 139.0);
        assert!(!loaded.contains_key(&oldest), "oldest-used key must be evicted");
        let newest = make_key("g", "ja", &format!("q{MAX_CACHE_ENTRIES}"), 35.0, 139.0);
        assert!(loaded.contains_key(&newest), "newest-used key must be kept");

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

        let map: HashMap<String, CacheEntry> = HashMap::new();
        save_to(&path, &map).expect("save_to should create parent dir and succeed");
        assert!(path.exists());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
