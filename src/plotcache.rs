// 地図に重ねるプロットデータ(道路交通量・主要道路・道路ライブカメラ・通行規制)の
// ディスク層。キー1件=1ファイルのJSONで保存し、種別ごとに違うTTLで期限切れにする。
// 設計は docs/plot-data-disk-cache-design.md §3/§5/§8。
//
// 保存先: ~/.config/termmap/plot-cache/v1/{traffic,roads,camera,regulation}/{キー}.json
//   {"v":1,"key":"5339","fetched_at":1755330000,"data_at":1755328500,"items":[ ... ]}
//
// このファイルはディスクしか知らない(ネットワークにもデータ型にも触れない)。種別ごとの差
// (ディレクトリ名・TTL・上限)は Layer に集約してあるので、値の一覧はここを見れば分かる。
// tiles.rs と違い期限判定に mtime を使わない。あちらは期限切れを「無かった」扱いにできるが、
// こちらは期限切れ(stale)でも中身を表示するため結局ファイルを読むため。mtime は gc() が
// ファイルを開かずに古い順へ並べるためだけに使う。

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

// ファイル形式のバージョン。形式やTTLの意味を変えたら上げる。ディレクトリ名にも同じ値を
// 持たせてあるので、上げるだけで旧データは参照されなくなり gc() が旧ディレクトリを消す。
const FORMAT_VERSION: u32 = 1;
const VERSION_DIR: &str = "v1";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    Traffic,
    Roads,
    Camera,
    Regulation,
}

pub const ALL_LAYERS: [Layer; 4] = [Layer::Traffic, Layer::Roads, Layer::Camera, Layer::Regulation];

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const MB: u64 = 1024 * 1024;

impl Layer {
    pub fn dir_name(&self) -> &'static str {
        match self {
            Layer::Traffic => "traffic",
            Layer::Roads => "roads",
            Layer::Camera => "camera",
            Layer::Regulation => "regulation",
        }
    }

    /// この期間内なら再取得しない(通信を止める唯一の判断基準)。
    /// 交通量5分=元データが5分値でさらに観測から約25分のラグがあるため、これより短く叩いても
    /// 新しい値は存在しない。主要道路30日=OSMのtrunk/primary幾何は月単位でしか変わらない
    /// (tiles.rs のタイルTTLと同じ値)。カメラ7日=設置位置の新設・撤去は週〜月単位。
    /// 通行規制10分=数十分〜数日で変わり、かつ通行止めは安全に直結するので短くする。
    pub fn fresh_ttl(&self) -> Duration {
        Duration::from_secs(match self {
            Layer::Traffic => 5 * MINUTE,
            Layer::Roads => 30 * DAY,
            Layer::Camera => 7 * DAY,
            Layer::Regulation => 10 * MINUTE,
        })
    }

    /// オフライン/取得失敗時に「表示だけは許す」上限。None は無期限(gcで消えるまで)。
    /// 交通量60分=これを超えると時間帯による交通量プロファイルが変わるので現況として示さない。
    /// 通行規制24時間=災害・工事の通行止めは日単位で続くため、それまでは「N時間前時点の規制」
    /// として出す価値がある。道路とカメラは古くても実害が無い(新設が出ないだけ)ので上限なし。
    pub fn stale_limit(&self) -> Option<Duration> {
        match self {
            Layer::Traffic => Some(Duration::from_secs(60 * MINUTE)),
            Layer::Regulation => Some(Duration::from_secs(24 * HOUR)),
            Layer::Roads | Layer::Camera => None,
        }
    }

    /// ディスク肥大化の上限(件数)。gc() が mtime の古い順に削って満たす。
    pub fn max_entries(&self) -> usize {
        match self {
            Layer::Traffic => 200,
            Layer::Roads => 300,
            Layer::Camera => 16,
            Layer::Regulation => 200,
        }
    }

    /// ディスク肥大化の上限(バイト)。件数と両方を満たすまで削る。
    pub fn max_bytes(&self) -> u64 {
        match self {
            Layer::Traffic => 20 * MB,
            Layer::Roads => 64 * MB,
            Layer::Camera => 10 * MB,
            Layer::Regulation => 20 * MB,
        }
    }
}

/// 1セルぶんの取得結果と、その鮮度。
pub struct Cached<T> {
    pub items: Vec<T>,
    /// 取得完了時刻(epoch秒)。fresh/stale の判定はこの値で行う。
    pub fetched_at: u64,
    /// データ自身の時刻(epoch秒)。交通量のみ fetched_at より前になる(観測から約25分のラグ)。
    /// 画面に出す経過時間はこちらを使う(取得からの経過ではなく観測からの経過が知りたいため)。
    pub data_at: u64,
}

impl<T> Cached<T> {
    /// データ自身の時刻からの経過秒。時計が巻き戻っていても 0 に丸める。
    pub fn age_secs(&self, now: u64) -> u64 {
        now.saturating_sub(self.data_at)
    }
    /// 再取得を抑止してよいか(fresh TTL 以内か)。
    pub fn is_fresh(&self, layer: Layer, now: u64) -> bool {
        now.saturating_sub(self.fetched_at) < layer.fresh_ttl().as_secs()
    }
}

// ディスク上の1ファイル。key はファイル名との不一致を検出するための保険
// (取り違え・手作業コピー事故で別セルの中身を読まないようにする)。
#[derive(Deserialize)]
struct Envelope<T> {
    v: u32,
    key: String,
    fetched_at: u64,
    data_at: u64,
    items: Vec<T>,
}

// 書き出し用。items を借用で受けて、保存のためだけの複製を作らない。
#[derive(Serialize)]
struct EnvelopeRef<'a, T> {
    v: u32,
    key: &'a str,
    fetched_at: u64,
    data_at: u64,
    items: &'a [T],
}

/// 現在時刻(epoch秒)。システム時計が1970より前を指していたら 0 にする。
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `$HOME/.config/termmap/plot-cache`。HOME が未設定/空なら None(=キャッシュ無しで動く)。
/// `TERMMAP_PLOT_CACHE_DIR` が設定されていればそちらを使う(テストが実HOMEを汚さないための
/// 差し替え口。別マシンの外付けディスクへ逃がす等の用途にも使える)。
pub fn cache_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TERMMAP_PLOT_CACHE_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(Path::new(&home).join(".config").join("termmap").join("plot-cache"))
}

// キーはメッシュコード/整備局CDのような短い整数を想定している。ファイル名にそのまま使うので、
// パス区切りや親ディレクトリ参照が混ざらないことをここで確かめる。
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 16
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn entry_path(root: &Path, layer: Layer, key: &str) -> Option<PathBuf> {
    if !valid_key(key) {
        return None;
    }
    Some(root.join(VERSION_DIR).join(layer.dir_name()).join(format!("{key}.json")))
}

/// セル1件を読む。次のいずれかなら None(=手元に無い扱い)。
/// ファイルが無い/壊れている/形式バージョン違い/キー不一致/stale上限を超えている。
pub fn load<T: DeserializeOwned>(root: &Path, layer: Layer, key: &str, now: u64) -> Option<Cached<T>> {
    let path = entry_path(root, layer, key)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let env: Envelope<T> = serde_json::from_str(&text).ok()?;
    if env.v != FORMAT_VERSION || env.key != key {
        return None;
    }
    if let Some(limit) = layer.stale_limit() {
        if now.saturating_sub(env.fetched_at) > limit.as_secs() {
            return None;
        }
    }
    Some(Cached { items: env.items, fetched_at: env.fetched_at, data_at: env.data_at })
}

/// セル1件を書く(アトミック保存: 一時ファイル→rename)。呼び出し元はワーカースレッド想定で、
/// 失敗しても致命的ではない(次回また取りに行くだけ)。
pub fn store<T: Serialize>(
    root: &Path,
    layer: Layer,
    key: &str,
    items: &[T],
    fetched_at: u64,
    data_at: u64,
) -> Result<(), String> {
    let path = entry_path(root, layer, key).ok_or_else(|| format!("不正なキー: {key}"))?;
    let env = EnvelopeRef { v: FORMAT_VERSION, key, fetched_at, data_at, items };
    let bytes = serde_json::to_vec(&env).map_err(|e| format!("plot-cache 直列化: {e}"))?;
    crate::fsutil::write_atomic(&path, &bytes, None).map_err(|e| format!("plot-cache 保存: {e}"))
}

/// セル1件を捨てる。TTLを無視して取り直させたくなったときの入口
/// (現状UIからは呼んでいない。fresh が最長でも10分なので待てば必ず更新されるため)。
#[allow(dead_code)]
pub fn invalidate(root: &Path, layer: Layer, key: &str) {
    if let Some(p) = entry_path(root, layer, key) {
        let _ = std::fs::remove_file(p);
    }
}

/// 期限切れ・上限超過・旧バージョンの掃除。ディレクトリ走査を伴うので無操作中に呼ぶ。
pub fn gc() {
    let Some(root) = cache_root() else { return };
    gc_in(&root, now_secs());
}

/// gc() の実体(テストから任意のrootと時刻で呼べるように分離)。ファイルの中身は読まない。
pub fn gc_in(root: &Path, now: u64) {
    // 1. バージョン移行の後片付け。現行バージョン以外のディレクトリ/ファイルを丸ごと消す。
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            if e.file_name() == VERSION_DIR {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    for layer in ALL_LAYERS {
        let dir = root.join(VERSION_DIR).join(layer.dir_name());
        // 2. (パス, mtime, サイズ)を集める。
        let mut entries: Vec<(PathBuf, u64, u64)> = Vec::new();
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if !md.is_file() {
                continue;
            }
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            entries.push((e.path(), mtime, md.len()));
        }
        // 3. stale上限を持つ種別は、上限より古いものを消す。
        if let Some(limit) = layer.stale_limit() {
            entries.retain(|(p, mtime, _)| {
                if now.saturating_sub(*mtime) > limit.as_secs() {
                    let _ = std::fs::remove_file(p);
                    false
                } else {
                    true
                }
            });
        }
        // 4. 残りを古い順に並べ、件数上限とバイト上限の両方を満たすまで先頭から消す。
        //    真のLRU(最終参照順)にしないのは、参照のたびにファイルを触ると mtime が
        //    「取得時刻」の意味を失いTTL判定が壊れるため。キー空間は地理的に有界で、
        //    必要なセルはどうせ再取得されるので取得が古い順の破棄で足りる。
        entries.sort_by_key(|(_, m, _)| *m);
        let mut total: u64 = entries.iter().map(|(_, _, s)| *s).sum();
        let mut count = entries.len();
        for (p, _, size) in &entries {
            if count <= layer.max_entries() && total <= layer.max_bytes() {
                break;
            }
            let _ = std::fs::remove_file(p);
            count -= 1;
            total = total.saturating_sub(*size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 実際の $HOME/.config/termmap には触れないよう、テストは毎回一意な一時ディレクトリを使う
    // (searchcache.rs のテストと同じ方針)。
    fn temp_root(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("termmap_plotcache_{}_{}_{}", std::process::id(), tag, n));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
    struct Pt {
        lat: f64,
        lon: f64,
    }

    fn pts() -> Vec<Pt> {
        vec![Pt { lat: 35.0, lon: 139.0 }, Pt { lat: 35.1, lon: 139.1 }]
    }

    #[test]
    fn store_then_load_roundtrips_items_and_timestamps() {
        let root = temp_root("rt");
        store(&root, Layer::Traffic, "5339", &pts(), 1000, 500).unwrap();
        let got: Cached<Pt> = load(&root, Layer::Traffic, "5339", 1000).unwrap();
        assert_eq!(got.items, pts());
        assert_eq!(got.fetched_at, 1000);
        assert_eq!(got.data_at, 500);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stored_file_lands_under_the_versioned_layer_directory() {
        let root = temp_root("path");
        store(&root, Layer::Regulation, "5339", &pts(), 1000, 1000).unwrap();
        assert!(root.join("v1").join("regulation").join("5339.json").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stored_json_has_the_designed_envelope_fields() {
        let root = temp_root("shape");
        store(&root, Layer::Traffic, "5339", &pts(), 1755330000, 1755328500).unwrap();
        let text = std::fs::read_to_string(root.join("v1/traffic/5339.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["key"], "5339");
        assert_eq!(v["fetched_at"], 1755330000u64);
        assert_eq!(v["data_at"], 1755328500u64);
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_returns_none_for_a_missing_entry() {
        let root = temp_root("missing");
        assert!(load::<Pt>(&root, Layer::Traffic, "5339", 0).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_returns_none_and_does_not_panic_on_broken_json() {
        let root = temp_root("broken");
        let p = root.join("v1/traffic/5339.json");
        crate::fsutil::write_atomic(&p, b"{not json at all", None).unwrap();
        assert!(load::<Pt>(&root, Layer::Traffic, "5339", 0).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_rejects_a_file_whose_key_does_not_match_its_name() {
        let root = temp_root("keymismatch");
        store(&root, Layer::Traffic, "5339", &pts(), 1000, 1000).unwrap();
        // 5339.json の中身を 5340 のものへ差し替える(コピー事故の再現)。
        let text = std::fs::read_to_string(root.join("v1/traffic/5339.json")).unwrap();
        let swapped = text.replace("\"5339\"", "\"5340\"");
        crate::fsutil::write_atomic(&root.join("v1/traffic/5339.json"), swapped.as_bytes(), None).unwrap();
        assert!(load::<Pt>(&root, Layer::Traffic, "5339", 1000).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_rejects_a_different_format_version() {
        let root = temp_root("ver");
        store(&root, Layer::Traffic, "5339", &pts(), 1000, 1000).unwrap();
        let text = std::fs::read_to_string(root.join("v1/traffic/5339.json")).unwrap();
        let bumped = text.replace("\"v\":1", "\"v\":2");
        crate::fsutil::write_atomic(&root.join("v1/traffic/5339.json"), bumped.as_bytes(), None).unwrap();
        assert!(load::<Pt>(&root, Layer::Traffic, "5339", 1000).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn entries_written_under_another_version_directory_are_invisible() {
        let root = temp_root("otherver");
        let p = root.join("v0").join("traffic").join("5339.json");
        crate::fsutil::write_atomic(
            &p,
            br#"{"v":1,"key":"5339","fetched_at":0,"data_at":0,"items":[]}"#,
            None,
        )
        .unwrap();
        assert!(load::<Pt>(&root, Layer::Traffic, "5339", 0).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_keys_are_refused_by_both_store_and_load() {
        let root = temp_root("badkey");
        for bad in ["../evil", "a/b", "", "with space", "0123456789abcdefg"] {
            assert!(store(&root, Layer::Traffic, bad, &pts(), 0, 0).is_err(), "key={bad:?}");
            assert!(load::<Pt>(&root, Layer::Traffic, bad, 0).is_none(), "key={bad:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn traffic_entry_is_fresh_up_to_five_minutes_and_stale_after() {
        let root = temp_root("freshttl");
        store(&root, Layer::Traffic, "5339", &pts(), 1_000_000, 1_000_000).unwrap();
        // 境界値: 5分ちょうど(300秒)は fresh ではない(< で判定する)。
        let at = |now: u64| load::<Pt>(&root, Layer::Traffic, "5339", now).unwrap().is_fresh(Layer::Traffic, now);
        assert!(at(1_000_000), "取得直後は fresh");
        assert!(at(1_000_299), "299秒後はまだ fresh");
        assert!(!at(1_000_300), "300秒(=5分)ちょうどで stale へ切り替わる");
        assert!(!at(1_000_301));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn traffic_entry_disappears_once_it_passes_the_sixty_minute_stale_limit() {
        let root = temp_root("stalelimit");
        store(&root, Layer::Traffic, "5339", &pts(), 1_000_000, 1_000_000).unwrap();
        // 境界値: 60分ちょうど(3600秒)はまだ読める。1秒でも超えたら読めない。
        assert!(load::<Pt>(&root, Layer::Traffic, "5339", 1_003_600).is_some());
        assert!(load::<Pt>(&root, Layer::Traffic, "5339", 1_003_601).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn regulation_entry_survives_a_day_but_not_more() {
        let root = temp_root("regstale");
        store(&root, Layer::Regulation, "5339", &pts(), 1_000_000, 1_000_000).unwrap();
        assert!(load::<Pt>(&root, Layer::Regulation, "5339", 1_000_000 + 24 * 3600).is_some());
        assert!(load::<Pt>(&root, Layer::Regulation, "5339", 1_000_001 + 24 * 3600).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn layers_without_a_stale_limit_stay_readable_forever() {
        let root = temp_root("nolimit");
        store(&root, Layer::Roads, "533946", &pts(), 1_000, 1_000).unwrap();
        store(&root, Layer::Camera, "83", &pts(), 1_000, 1_000).unwrap();
        let far_future = 1_000 + 3650 * DAY;
        let roads = load::<Pt>(&root, Layer::Roads, "533946", far_future).unwrap();
        assert!(!roads.is_fresh(Layer::Roads, far_future), "10年後は fresh ではない");
        assert!(load::<Pt>(&root, Layer::Camera, "83", far_future).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn age_uses_data_at_so_traffic_reports_the_observation_age() {
        let c = Cached { items: pts(), fetched_at: 1_000_000, data_at: 1_000_000 - 25 * 60 };
        // 取得直後でもデータ自身は25分前(JARTICの取得窓)。
        assert_eq!(c.age_secs(1_000_000), 25 * 60);
    }

    #[test]
    fn timestamps_from_the_future_do_not_underflow() {
        let c = Cached { items: pts(), fetched_at: 2_000_000, data_at: 2_000_000 };
        assert_eq!(c.age_secs(1_000_000), 0);
        assert!(c.is_fresh(Layer::Traffic, 1_000_000));
    }

    #[test]
    fn gc_removes_directories_of_other_format_versions() {
        let root = temp_root("gcver");
        crate::fsutil::write_atomic(&root.join("v0/traffic/5339.json"), b"{}", None).unwrap();
        crate::fsutil::write_atomic(&root.join("stray.txt"), b"x", None).unwrap();
        store(&root, Layer::Traffic, "5339", &pts(), now_secs(), now_secs()).unwrap();
        gc_in(&root, now_secs());
        assert!(!root.join("v0").exists());
        assert!(!root.join("stray.txt").exists());
        assert!(root.join("v1/traffic/5339.json").is_file(), "現行バージョンは残る");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gc_drops_entries_older_than_the_stale_limit() {
        let root = temp_root("gctime");
        let now = now_secs();
        store(&root, Layer::Traffic, "5339", &pts(), now, now).unwrap();
        store(&root, Layer::Roads, "533946", &pts(), now, now).unwrap();
        // mtime は「今」なので、時刻の方を stale上限(60分)より先へ進めて判定させる。
        gc_in(&root, now + 61 * 60);
        assert!(!root.join("v1/traffic/5339.json").exists(), "交通量は60分で消える");
        assert!(root.join("v1/roads/533946.json").is_file(), "主要道路は時間では消えない");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gc_enforces_the_entry_count_cap_oldest_first() {
        let root = temp_root("gccount");
        let now = now_secs();
        // カメラの上限は16件。20件書いて4件削られることを見る。
        for i in 0..20u32 {
            store(&root, Layer::Camera, &format!("{i}"), &pts(), now, now).unwrap();
            // mtime の順序を確実に分けるため、書いた順に過去→現在へずらす。
            let path = root.join("v1/camera").join(format!("{i}.json"));
            let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(now - (20 - i) as u64 * 10);
            filetime_set(&path, t);
        }
        gc_in(&root, now);
        let left: Vec<String> = std::fs::read_dir(root.join("v1/camera"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 16, "上限16件まで削られる: {left:?}");
        assert!(!left.contains(&"0.json".to_string()), "最も古い0が最初に消える");
        assert!(left.contains(&"19.json".to_string()), "最も新しい19は残る");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gc_enforces_the_byte_cap_even_when_the_entry_count_fits() {
        let root = temp_root("gcbytes");
        let now = now_secs();
        // カメラのバイト上限は10MB。6MBのファイルを2枚置くと、件数(16)には収まるが
        // バイト上限を超えるので古い方が消える。
        let big = vec![0u8; 6 * 1024 * 1024];
        for (i, off) in [("81", 20u64), ("82", 10)] {
            let path = root.join("v1/camera").join(format!("{i}.json"));
            crate::fsutil::write_atomic(&path, &big, None).unwrap();
            filetime_set(&path, std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(now - off));
        }
        gc_in(&root, now);
        assert!(!root.join("v1/camera/81.json").exists(), "古い方が消える");
        assert!(root.join("v1/camera/82.json").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gc_on_a_nonexistent_root_is_a_no_op() {
        let root = temp_root("gcempty");
        gc_in(&root, now_secs()); // panic しないこと
        assert!(!root.exists());
    }

    #[test]
    fn invalidate_removes_only_the_named_entry() {
        let root = temp_root("inv");
        store(&root, Layer::Traffic, "5339", &pts(), 1000, 1000).unwrap();
        store(&root, Layer::Traffic, "5340", &pts(), 1000, 1000).unwrap();
        invalidate(&root, Layer::Traffic, "5339");
        assert!(load::<Pt>(&root, Layer::Traffic, "5339", 1000).is_none());
        assert!(load::<Pt>(&root, Layer::Traffic, "5340", 1000).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    // mtime を任意時刻へ寄せる(gcの並び順テスト用)。外部crateを足さずに済ませるため、
    // unix では utimes(2) を直接呼ぶ。他OSでは何もしない(そのテストは書いた順=mtime順になる)。
    #[cfg(unix)]
    fn filetime_set(path: &Path, t: std::time::SystemTime) {
        use std::os::unix::ffi::OsStrExt;
        let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // struct timeval { tv_sec, tv_usec } を2つ(atime, mtime)。
        #[repr(C)]
        struct Timeval {
            tv_sec: i64,
            tv_usec: i64,
        }
        extern "C" {
            fn utimes(path: *const std::ffi::c_char, times: *const Timeval) -> i32;
        }
        let tv = [Timeval { tv_sec: secs, tv_usec: 0 }, Timeval { tv_sec: secs, tv_usec: 0 }];
        unsafe {
            utimes(c_path.as_ptr(), tv.as_ptr());
        }
    }
    #[cfg(not(unix))]
    fn filetime_set(_path: &Path, _t: std::time::SystemTime) {}
}
