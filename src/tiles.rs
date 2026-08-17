// タイル取得 (OSM/Carto) と表示窓の合成
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use image::{RgbImage, RgbaImage};
// 近傍優先の距離計算で、異なるズームのタイル中心を同一ズームへ再投影するために座標変換を使う。
use crate::geo::{TILE, pixel_to_deg, deg_to_pixel};

// タイルのディスクキャッシュ先: ~/.config/termmap/tiles/<style>/<z>/<x>/<y>.png
// 一度取得したタイルはここに残り、パン再訪・再起動でも再DLせず読み出す(通信最小化)。
fn tile_cache_path(style: &str, z: u32, x: i64, y: i64) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join(".config/termmap/tiles").join(style).join(z.to_string()).join(x.to_string()).join(format!("{y}.png")))
}

// タイルの取得元。従来 TileKey.style: String が持っていた「どのタイル群か」の軸を型にする。
// 地図スタイル(Base)と雨雲フレーム(Radar)は直交する軸なので String 1本には押し込めない。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TileSource {
    // 通常の地図タイル。値は "osm" / "voyager" / "dark" / "light" / "topo"。
    Base(String),
    // 気象庁の降水系タイルの1コマ。basetime=発表時刻 / validtime=対象時刻(いずれもUTCの14桁)。
    // 文字列は radar.rs が JMA の応答から取り出したものをそのまま持つ(URLのパス要素に直接使う)。
    // product はどのプロダクト由来か(ナウキャスト/降水短時間予報)で、URLのelement名を左右する。
    Radar { basetime: String, validtime: String, product: crate::radar::RadarProduct },
}

impl TileSource {
    // 取得元のタイルURL。
    fn url(&self, z: u32, x: i64, y: i64) -> String {
        match self {
            TileSource::Base(style) => tile_url(style, z, x, y),
            TileSource::Radar { basetime, validtime, product } => radar_tile_url(basetime, validtime, *product, z, x, y),
        }
    }
    // ディスクキャッシュ先のパス(保存しない取得元では None)。
    fn cache_path(&self, z: u32, x: i64, y: i64) -> Option<PathBuf> {
        match self {
            TileSource::Base(style) => tile_cache_path(style, z, x, y),
            // 雨雲タイルはディスクに保存しない。パスに basetime/validtime が入るためヒット率が
            // ほぼゼロで書き捨てのファイルが無限に積み上がるうえ、30日TTLで期限内と判断された
            // 古い降水がそのまま地図に描かれる危険がある(ツーリング用途では実害)。
            TileSource::Radar { .. } => None,
        }
    }
    // 雨雲タイルか(キャッシュ予算の分離判定に使う)。
    fn is_radar(&self) -> bool { matches!(self, TileSource::Radar { .. }) }
}

// キャッシュキーは取得元(src)を含む(style違いのタイルが混ざらない。以前は clear() 頼みで危うかった)。
#[derive(Clone, PartialEq, Eq, Hash)]
struct TileKey { src: TileSource, z: u32, x: i64, y: i64 }

// 地図タイルのメモリ上限(枚)。従来の cap と同じ値(挙動を変えない)。
const BASE_CACHE_CAP: usize = 256;
// 雨雲タイルのメモリ上限(枚)。RGBA 256x256 = 256KB/枚 → 約48MB。1画面は概ね4〜9枚なので、
// タイムラインを端から端までスクラブしても大半のコマがメモリに残る。
const RADAR_CACHE_CAP: usize = 192;

// タイルキャッシュ。上限超過時は最終アクセスが最古のものから捨てる簡易LRU。
// 長時間パンし続けてもメモリが訪問範囲に比例して無制限に増えないようにする。
// 値は RGBA で持つ(雨雲タイルの透過を落とさないため。地図タイルは alpha=255 で入る)。
//
// 予算(LRU)は取得元の種別ごとに分ける。1つのHashMapに混ぜると、雨雲のタイムラインを端から端まで
// スクラブした瞬間に雨雲タイルが地図タイルを全部追い出し、地図が LOADING だらけになる。
pub struct Cache {
    base: HashMap<TileKey, (RgbaImage, u64)>,
    radar: HashMap<TileKey, (RgbaImage, u64)>,
    tick: u64,
    base_cap: usize,
    radar_cap: usize,
}

impl Default for Cache { fn default() -> Self { Self::new() } }

impl Cache {
    pub fn new() -> Self {
        Cache { base: HashMap::new(), radar: HashMap::new(), tick: 0, base_cap: BASE_CACHE_CAP, radar_cap: RADAR_CACHE_CAP }
    }
    // このキーが属する側のマップ(読み取り)。
    fn bucket(&self, k: &TileKey) -> &HashMap<TileKey, (RgbaImage, u64)> {
        if k.src.is_radar() { &self.radar } else { &self.base }
    }
    // このキーが属する側のマップ(書き込み)と、その予算。
    fn bucket_mut(&mut self, k: &TileKey) -> (&mut HashMap<TileKey, (RgbaImage, u64)>, usize) {
        if k.src.is_radar() {
            let cap = self.radar_cap;
            (&mut self.radar, cap)
        } else {
            let cap = self.base_cap;
            (&mut self.base, cap)
        }
    }
    fn contains(&self, k: &TileKey) -> bool { self.bucket(k).contains_key(k) }
    fn get(&mut self, k: &TileKey) -> Option<&RgbaImage> {
        self.tick += 1;
        let t = self.tick;
        let (m, _) = self.bucket_mut(k);
        match m.get_mut(k) { Some(e) => { e.1 = t; Some(&e.0) } None => None }
    }
    fn insert(&mut self, k: TileKey, img: RgbaImage) {
        self.tick += 1;
        let t = self.tick;
        let (m, cap) = self.bucket_mut(&k);
        if m.len() >= cap && !m.contains_key(&k) {
            if let Some(old) = m.iter().min_by_key(|(_, (_, t))| *t).map(|(kk, _)| kk.clone()) {
                m.remove(&old); // 最古を1つ退避
            }
        }
        m.insert(k, (img, t));
    }
    // keep に含まれないフレームの雨雲タイルを捨てる。targetTimes が更新されると古い basetime の
    // タイルは JMA 側から消えて二度と使えないため、放置するとLRUが「もう絶対に使わないタイル」で
    // 埋まる。地図タイル側には触れない。
    fn retain_radar_frames(&mut self, keep: &[crate::radar::Frame]) {
        self.radar.retain(|k, _| radar_key_is_kept(k, keep));
    }
}

// keep(新しいフレーム一覧)に残すべきキーか。コマの同一性は basetime と validtime の両方で決まる。
// 雨雲以外(地図タイル)は判定対象外なので常に残す。ネットワークにも状態にも触れない純粋関数。
fn radar_key_is_kept(k: &TileKey, keep: &[crate::radar::Frame]) -> bool {
    match &k.src {
        TileSource::Radar { basetime, validtime, .. } =>
            keep.iter().any(|f| &f.basetime == basetime && &f.validtime == validtime),
        _ => true,
    }
}

// タイルスタイル → URL。voyager/dark/light は CartoDB の label-free 系(端末で見やすい)。
// topo は OpenTopoMap(地形陰影・等高線入り。最大ズームz17程度、それより深いタイルは無い)。
fn tile_url(style: &str, z: u32, x: i64, y: i64) -> String {
    match style {
        "voyager" => format!("https://basemaps.cartocdn.com/rastertiles/voyager_nolabels/{z}/{x}/{y}.png"),
        "dark"    => format!("https://basemaps.cartocdn.com/dark_nolabels/{z}/{x}/{y}.png"),
        "light"   => format!("https://basemaps.cartocdn.com/light_nolabels/{z}/{x}/{y}.png"),
        "topo"    => format!("https://tile.opentopomap.org/{z}/{x}/{y}.png"),
        _         => format!("https://tile.openstreetmap.org/{z}/{x}/{y}.png"),
    }
}
// 気象庁の降水系タイルURL。背景透過PNGで、降水なしの領域は透明で返る。
// 非公式エンドポイント(開発者向けAPIとして文書化されていない)なので、URL構築はここと
// radar.rs の targetTimes 定数の2箇所だけに閉じる(壊れたら1箇所直せば済む)。
// basetime/validtime は radar.rs 側で「ASCII数字のみ」の検証を通ったものだけが渡る。
// どのz/x/yでもHTTP 200が返る(404は無い)。中身が入っているズームは限られるので、
// 要求するズームの決定は radar_source_zoom が行う。
// product で lv2/element が変わる(実測確認済み): ナウキャスト(hrpns)=nowc/hrpns、
// 降水短時間予報(rasrf)=rasrf/rasrf。パレット(4bit索引色10色・tRNS)は両者で完全一致するため
// デコード側は分岐不要。
fn radar_tile_url(basetime: &str, validtime: &str, product: crate::radar::RadarProduct, z: u32, x: i64, y: i64) -> String {
    match product {
        crate::radar::RadarProduct::Nowcast =>
            format!("https://www.jma.go.jp/bosai/jmatile/data/nowc/{basetime}/none/{validtime}/surf/hrpns/{z}/{x}/{y}.png"),
        crate::radar::RadarProduct::ShortTerm =>
            format!("https://www.jma.go.jp/bosai/jmatile/data/rasrf/{basetime}/none/{validtime}/surf/rasrf/{z}/{x}/{y}.png"),
    }
}

// ディスクキャッシュの有効期限。タイル自体は滅多に変わらないが、無期限だと地図更新(新道路等)が
// 反映されないため30日で区切る。期限切れは「無かった」扱いにしてネットワークから取り直す。
const TILE_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);

// mtimeからの経過時間がTTL未満か(ネットワーク/ファイルI/Oを伴わない純粋関数)。
fn is_tile_fresh(age: std::time::Duration) -> bool { age < TILE_TTL }

// 取得結果は RGBA で返す(地図タイルは alpha=255。将来の半透明タイルで透過情報を落とさないため)。
pub fn fetch_tile(src: &TileSource, z: u32, x: i64, y: i64) -> Result<RgbaImage, String> {
    // 1) ディスクキャッシュを先に見る(在って、かつ30日以内ならネット無しで読む)
    let cache_path = src.cache_path(z, x, y);
    if let Some(p) = &cache_path {
        let fresh = std::fs::metadata(p).ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.elapsed().ok())
            .map(is_tile_fresh)
            .unwrap_or(false); // mtime取得失敗時は期限切れ扱い(安全側)にして取り直す
        if fresh {
            if let Ok(buf) = std::fs::read(p) {
                if let Ok(img) = image::load_from_memory(&buf) { return Ok(img.to_rgba8()); }
                // 壊れたキャッシュは無視して取り直す
            }
        }
    }
    // 2) ネットワーク取得
    let url = src.url(z, x, y);
    let resp = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .timeout(std::time::Duration::from_secs(20)).call().map_err(|e| format!("fetch tile {z}/{x}/{y}: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&buf).map_err(|e| format!("decode tile {z}/{x}/{y}: {e}"))?.to_rgba8();
    // 3) ディスクへ保存(元PNGバイトのまま・ベストエフォート)
    if let Some(p) = &cache_path {
        if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
        let _ = std::fs::write(p, &buf);
    }
    Ok(img)
}

// OpenTopoMap(topo)の実際の最大ズーム。これより深いズームをリクエストすると404ではなく
// 「max zoom layer = 17」という文字だけのプレースホルダー画像がHTTP 200で返ってくるため、
// エラーとして検知できずそのまま地図に貼られ、道路等が全く描かれない壊れた表示になる。
const TOPO_MAX_Z: u32 = 17;

// z17タイルをオーバーズームで拡大表示する際の画素幾何(ネットワークを伴わない純粋関数)。
// shift=要求ズーム-17。base_w/base_h=z17相当で取得すべき窓サイズ(端数対策の余白+4込み)、
// scaled_w/scaled_h=それを scale=2^shift 倍した後のサイズ、crop_x/crop_y=中央クロップの開始位置。
// 呼び出し側は scaled_w >= win_w (同 h) が常に成り立つ前提でcrop_imm(パニック回避)する。
fn overzoom_geometry(win_w: u32, win_h: u32, shift: u32) -> (u32, u32, u32, u32, u32, u32) {
    let scale = (1u32 << shift) as f64;
    let base_w = (win_w as f64 / scale).ceil() as u32 + 4;
    let base_h = (win_h as f64 / scale).ceil() as u32 + 4;
    let scaled_w = (base_w as f64 * scale) as u32;
    let scaled_h = (base_h as f64 * scale) as u32;
    let crop_x = scaled_w.saturating_sub(win_w) / 2;
    let crop_y = scaled_h.saturating_sub(win_h) / 2;
    (base_w, base_h, scaled_w, scaled_h, crop_x, crop_y)
}

// 窓(中心cx,cy グローバルpx / win_w×win_h)が覆うタイルのx/y範囲と、窓左上のグローバルpx(left/top)。
// build_window と build_window_nowait でタイル列挙のジオメトリを二重管理しない(ズレ防止)ため純粋関数に切り出す。
fn window_tile_range(cx: f64, cy: f64, win_w: u32, win_h: u32) -> (i64, i64, i64, i64, f64, f64) {
    let left = cx - win_w as f64 / 2.0;
    let top = cy - win_h as f64 / 2.0;
    let tf = TILE as f64;
    let tx_min = (left / tf).floor() as i64;
    let tx_max = ((left + win_w as f64 - 1.0) / tf).floor() as i64;
    let ty_min = (top / tf).floor() as i64;
    let ty_max = ((top + win_h as f64 - 1.0) / tf).floor() as i64;
    (tx_min, tx_max, ty_min, ty_max, left, top)
}

// 窓の切り出し(docs/web-pan-smoothness-design.md §5.1 対策A)。
//
// 従来は left/top を整数へ切り捨てて切り出していた。X と Y が独立に切り捨てられるため、
// 描かれる位置の誤差は X 成分と Y 成分が無関係に ±1ピクセル未満で揺れる。真横のドラッグでは
// 進行方向と平行な速度のむらにしか見えないが、斜めのドラッグでは誤差に進行方向と直交する
// 成分が乗り、地図が左右へ振れて軌跡が階段状に見える(設計 §3.1)。
//
// ここでは canvas 上の窓左上 (ox, oy) の小数部を捨てず、右下方向の隣接ピクセルとの
// 2×2 バイリニアで win_w×win_h を作る。出力セルの色が連続的に変化するので、1セル未満の
// 動きも色の遷移として見え、X と Y が同時に連続になって直交方向のブレが消える。
//
// 参照先は canvas の範囲内へクランプする。呼び出し側はバイリニアが右下へ1ピクセル余分に
// 参照するぶんタイル範囲を広げてあるので通常はクランプに掛からないが、世界の端や壊れた値でも
// 範囲外参照しないようにしてある。
fn crop_window_subpixel(canvas: &RgbImage, ox: f64, oy: f64, win_w: u32, win_h: u32) -> RgbImage {
    let (cw, ch) = canvas.dimensions();
    let mut out = RgbImage::new(win_w, win_h);
    if cw == 0 || ch == 0 { return out; }
    // NaN 等が来ても 0 へ落として必ず描く(地図が消えるより端がずれる方がまし)。
    let ox = if ox.is_finite() { ox.max(0.0) } else { 0.0 };
    let oy = if oy.is_finite() { oy.max(0.0) } else { 0.0 };
    let (bx, by) = (ox.floor(), oy.floor());
    let (fx, fy) = (ox - bx, oy - by);
    let (bxi, byi) = (bx as i64, by as i64);
    // 2×2 の重み。fx=fy=0 のときは w00=1・他0 になり、従来の整数切り出しと完全に一致する。
    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;
    let cx_max = cw as i64 - 1;
    let cy_max = ch as i64 - 1;
    for j in 0..win_h {
        let sy = byi + j as i64;
        let y0 = sy.clamp(0, cy_max) as u32;
        let y1 = (sy + 1).clamp(0, cy_max) as u32;
        for i in 0..win_w {
            let sx = bxi + i as i64;
            let x0 = sx.clamp(0, cx_max) as u32;
            let x1 = (sx + 1).clamp(0, cx_max) as u32;
            let p00 = canvas.get_pixel(x0, y0).0;
            let p10 = canvas.get_pixel(x1, y0).0;
            let p01 = canvas.get_pixel(x0, y1).0;
            let p11 = canvas.get_pixel(x1, y1).0;
            let mut px = [0u8; 3];
            for c in 0..3 {
                let v = p00[c] as f64 * w00 + p10[c] as f64 * w10
                      + p01[c] as f64 * w01 + p11[c] as f64 * w11;
                px[c] = v.round().clamp(0.0, 255.0) as u8;
            }
            out.put_pixel(i, j, image::Rgb(px));
        }
    }
    out
}

// 中心(cx,cy グローバルpx)から win_w×win_h の矩形窓を組み立てる。タイルは cache 経由。
pub fn build_window(cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32, style: &str, cache: &mut Cache) -> Result<RgbImage, String> {
    if style == "topo" && z > TOPO_MAX_Z {
        // z17のタイルを取得し、要求されたズームぶん拡大(オーバーズーム)して代用する。
        // プレースホルダー画像を貼るよりは、ぼやけていても実際の地形図が見える方がまし。
        let shift = z - TOPO_MAX_Z;
        let scale = (1u32 << shift) as f64;
        let (base_w, base_h, scaled_w, scaled_h, crop_x, crop_y) = overzoom_geometry(win_w, win_h, shift);
        let base_img = build_window(cx / scale, cy / scale, TOPO_MAX_Z, base_w, base_h, style, cache)?;
        let resized = image::imageops::resize(&base_img, scaled_w, scaled_h, image::imageops::FilterType::Nearest);
        return Ok(image::imageops::crop_imm(&resized, crop_x, crop_y, win_w, win_h).to_image());
    }
    let tf = TILE as f64;
    let (tx_min, tx_max, ty_min, ty_max, left, top) = window_tile_range(cx, cy, win_w, win_h);
    let max_t = 2i64.pow(z);
    // 呼び出し側は従来どおり style 文字列で呼ぶ。取得元の型はここで組み立てる。
    let src = TileSource::Base(style.to_string());

    // 未キャッシュのタイルを列挙
    let mut missing: Vec<(i64, i64)> = Vec::new();
    for ty in ty_min..=ty_max {
        if ty < 0 || ty >= max_t { continue; }
        for tx in tx_min..=tx_max {
            let wx = ((tx % max_t) + max_t) % max_t;
            if !cache.contains(&TileKey { src: src.clone(), z, x: wx, y: ty }) { missing.push((wx, ty)); }
        }
    }
    missing.sort_unstable();
    missing.dedup();
    // OpenTopoMapは個人利用で最大2req/秒程度を求める利用ポリシーがあり、他スタイルと同じ8並列で
    // 叩くとサーバー側スロットリングを誘発し1タイルあたり数秒かかることを実測で確認した。
    // topoのみ並列数を落とす(他スタイルのCDNは8並列でも問題ない)。
    let concurrency: usize = if style == "topo" { 2 } else { 8 };
    for chunk in missing.chunks(concurrency) {
        let src_ref = &src;
        let got: Vec<((i64, i64), Result<RgbaImage, String>)> = std::thread::scope(|s| {
            let hs: Vec<_> = chunk.iter().map(|&(wx, ty)| s.spawn(move || ((wx, ty), fetch_tile(src_ref, z, wx, ty)))).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for ((wx, ty), r) in got { cache.insert(TileKey { src: src.clone(), z, x: wx, y: ty }, r?); }
    }

    let cols = (tx_max - tx_min + 1) as u32;
    let rows = (ty_max - ty_min + 1) as u32;
    let bg = if style == "dark" { image::Rgb([26, 26, 26]) } else { image::Rgb([221, 221, 221]) };
    let mut canvas = RgbImage::from_pixel(cols * TILE, rows * TILE, bg);
    for ty in ty_min..=ty_max {
        if ty < 0 || ty >= max_t { continue; }
        for tx in tx_min..=tx_max {
            let wx = ((tx % max_t) + max_t) % max_t;
            if let Some(t) = cache.get(&TileKey { src: src.clone(), z, x: wx, y: ty }) {
                let ox = (tx - tx_min) as u32 * TILE;
                let oy = (ty - ty_min) as u32 * TILE;
                // 地図タイルは alpha=255 の不透明画像なので、RGB 3成分だけ取って貼る。
                for (px, py, p) in t.enumerate_pixels() { canvas.put_pixel(ox + px, oy + py, image::Rgb([p[0], p[1], p[2]])); }
            }
        }
    }
    let crop_x = (left - tx_min as f64 * tf).max(0.0) as u32;
    let crop_y = (top - ty_min as f64 * tf).max(0.0) as u32;
    Ok(image::imageops::crop_imm(&canvas, crop_x, crop_y, win_w, win_h).to_image())
}

// ---- 非ブロッキング・タイルローダー ----
// あるタイル(tile_z/x/y)の中心が、現在view(view_z上の view_cx,view_cy)からどれだけ離れているか。
// タイル中心をそのズームで緯度経度へ戻し view_z へ再投影して同一ズーム上のユークリッド距離にする。
// これで「要求ズーム」と「実際に取得するタイルのズーム(topoオーバーズーム時はz17固定)」が食い違っても
// 近さを一貫して比較でき、ワーカーが現在地に近いタイルから埋められる。
fn tile_distance_to_view(tile_z: u32, tile_x: i64, tile_y: i64, view_cx: f64, view_cy: f64, view_z: u32) -> f64 {
    let tf = TILE as f64;
    let (lat, lon) = pixel_to_deg((tile_x as f64 + 0.5) * tf, (tile_y as f64 + 0.5) * tf, tile_z);
    let (gx, gy) = deg_to_pixel(lat, lon, view_z);
    let dx = gx - view_cx;
    let dy = gy - view_cy;
    (dx * dx + dy * dy).sqrt()
}

// ワーカーが「今どこを見ているか」を知るための共有view。メインが毎フレーム最新化する。
struct ViewState { cx: f64, cy: f64, z: u32, style: String }
// 取得依頼中のタイル集合。queued=未着手 / inflight=取得中 / failed=直近に失敗し再試行クールダウン中。
// queued/inflightにもcacheにも無いものだけ新規登録して二重取得を防ぐ。failedはさらに、404等の恒久的
// 失敗をクールダウン明けまで再登録しない(#56)ためのネガティブキャッシュ。
struct PendingSet { queued: HashSet<TileKey>, inflight: HashSet<TileKey>, failed: HashMap<TileKey, std::time::Instant> }

// 失敗クールダウン期限。直近の失敗からこの時間未満は再登録しない(404等の恒久失敗を~20ms間隔で
// 無限リトライし続けるのを防ぐ)。期限を過ぎれば通常通り再試行される(一時的な障害からは回復できる)。
const FAILED_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

// 失敗からの経過時間がクールダウン未満か(時刻取得を伴わない純粋関数)。
fn in_cooldown(elapsed: std::time::Duration) -> bool { elapsed < FAILED_COOLDOWN }

// 常駐ワーカー数(=非topoの並列上限)。topo時は inflight 数で2に絞るので、余ったワーカーは待機する。
const LOADER_WORKERS: usize = 8;

// 対話ループの裏で常駐し、未取得タイルを「現在viewに近い順」で取得し続けるローダー。
// メインは build_window_nowait 経由で欠落タイルを積むだけ、実取得はワーカー群が行い cache へ入れて
// generation を増やす(=メイン側の再描画トリガ)。ネットワーク待ちでメイン描画をブロックしない。
pub struct TileLoader {
    shared: Arc<Mutex<Cache>>,
    view: Arc<Mutex<ViewState>>,
    pending: Arc<Mutex<PendingSet>>,
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl TileLoader {
    // 共有キャッシュを受け取ってワーカー群を起こす。以後 interactive の生存中ずっと動く。
    pub fn start(shared: Arc<Mutex<Cache>>) -> Self {
        let view = Arc::new(Mutex::new(ViewState { cx: 0.0, cy: 0.0, z: 0, style: String::new() }));
        let pending = Arc::new(Mutex::new(PendingSet { queued: HashSet::new(), inflight: HashSet::new(), failed: HashMap::new() }));
        let generation = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        for _ in 0..LOADER_WORKERS {
            let (shared, view, pending) = (Arc::clone(&shared), Arc::clone(&view), Arc::clone(&pending));
            let (generation, stop) = (Arc::clone(&generation), Arc::clone(&stop));
            std::thread::spawn(move || worker_loop(shared, view, pending, generation, stop));
        }
        TileLoader { shared, view, pending, generation, stop }
    }

    // メインが毎フレーム呼ぶ。ワーカーの近傍優先の基準を最新の表示位置に更新する(重い処理はしない)。
    pub fn set_view(&self, cx: f64, cy: f64, z: u32, style: &str) {
        let mut v = self.view.lock().unwrap();
        v.cx = cx; v.cy = cy; v.z = z;
        if v.style != style { v.style = style.to_string(); }
    }

    // build_window_nowait から欠落タイルをまとめて積む(cacheロックは呼び出し側で解放済み)。
    // 既に queued/inflight にあるものは弾く=二重リクエスト防止。直近失敗してクールダウン中のものも
    // 弾く(#56)。クールダウンを過ぎていればfailedから外して通常通り再試行する。
    fn request_tiles(&self, keys: Vec<TileKey>) {
        let mut p = self.pending.lock().unwrap();
        let now = std::time::Instant::now();
        for k in keys {
            if p.queued.contains(&k) || p.inflight.contains(&k) { continue; }
            if let Some(&failed_at) = p.failed.get(&k) {
                if in_cooldown(now.duration_since(failed_at)) { continue; }
                p.failed.remove(&k);
            }
            p.queued.insert(k);
        }
    }

    // ルート確定時、その経路が通るタイルを先読み依頼として登録する(#34)。既存のrequest_tilesを
    // そのまま使う=画面表示中のタイルの方が常にview距離で優先されるため、専用の優先度階層は不要。
    pub fn request_route_tiles(&self, style: &str, z: u32, tile_coords: &[(i64, i64)]) {
        let src = TileSource::Base(style.to_string());
        let keys = tile_coords.iter()
            .map(|&(x, y)| TileKey { src: src.clone(), z, x, y })
            .collect();
        self.request_tiles(keys);
    }

    // 1タイルでも取得が進むと増える世代。ui側が map_sig に混ぜて次フレームの再描画を誘発する。
    pub fn generation(&self) -> u64 { self.generation.load(Ordering::Relaxed) }

    // まだ取得すべきタイルが残っているか。残っている間はメインループをポーリング(read()でブロックしない)
    // 側に倒し、届いたタイルを次フレームで反映させるために使う。
    pub fn is_busy(&self) -> bool {
        let p = self.pending.lock().unwrap();
        !p.queued.is_empty() || !p.inflight.is_empty()
    }

    // スタイル切替時(cache.clear相当のタイミング): 未着手の取得依頼を捨てる(旧スタイルのゴミを溜めない)。
    // inflight は各ワーカーが取得完了で自然に外すのでそのまま流す。
    pub fn clear_pending(&self) {
        let mut p = self.pending.lock().unwrap();
        p.queued.clear();
    }

    // targetTimes 更新時に呼ぶ。新しいフレーム一覧(keep)に含まれない雨雲タイルを、メモリキャッシュ
    // からも未着手の取得依頼からも捨てる。古い basetime のタイルは JMA 側から消えており二度と
    // 使えないため、残すとLRUと取得キューが「もう絶対に使わないタイル」で埋まる。
    // inflight は取得完了で自然に外れるのでそのまま流す(結果はキャッシュに入るが次回の掃除で落ちる)。
    pub fn drop_radar_frames_except(&self, keep: &[crate::radar::Frame]) {
        self.shared.lock().unwrap().retain_radar_frames(keep);
        let mut p = self.pending.lock().unwrap();
        p.queued.retain(|k| radar_key_is_kept(k, keep));
        p.failed.retain(|k, _| radar_key_is_kept(k, keep));
    }
}

impl Drop for TileLoader {
    // interactive 終了時にワーカーへ停止を伝える(次周で抜ける)。join はしない=取得中(最大20秒)を待たない。
    fn drop(&mut self) { self.stop.store(true, Ordering::Relaxed); }
}

// 常駐ワーカー本体。queued の中から現在viewに最も近い1枚を確保して取得→cacheへ→世代を上げる、を繰り返す。
fn worker_loop(shared: Arc<Mutex<Cache>>, view: Arc<Mutex<ViewState>>, pending: Arc<Mutex<PendingSet>>, generation: Arc<AtomicU64>, stop: Arc<AtomicBool>) {
    loop {
        if stop.load(Ordering::Relaxed) { break; }
        // 現在view(近傍優先の基準)とスタイル上限を読む。topoは配信元ポリシー上2並列に絞る(build_window同様)。
        let (vcx, vcy, vz, limit) = {
            let v = view.lock().unwrap();
            (v.cx, v.cy, v.z, if v.style == "topo" { 2 } else { 8 })
        };
        // 上限内なら queued から view に最も近いタイルを1つ確保(queued→inflight へ移す)。
        // 選定〜移動を pending ロック下で原子的に行い、複数ワーカーが同じタイルを掴まないようにする。
        let picked = {
            let mut p = pending.lock().unwrap();
            if p.inflight.len() >= limit || p.queued.is_empty() {
                None
            } else {
                let best = p.queued.iter().min_by(|a, b| {
                    let da = tile_distance_to_view(a.z, a.x, a.y, vcx, vcy, vz);
                    let db = tile_distance_to_view(b.z, b.x, b.y, vcx, vcy, vz);
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                }).cloned();
                if let Some(k) = &best { p.queued.remove(k); p.inflight.insert(k.clone()); }
                best
            }
        };
        match picked {
            // 何も無い/上限に達している間は少し待つ(CPUスピン防止)。
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
            Some(k) => {
                // ネットワーク取得中はどのロックも握らない(メイン描画をブロックしないため)。
                match fetch_tile(&k.src, k.z, k.x, k.y) {
                    Ok(img) => {
                        shared.lock().unwrap().insert(k.clone(), img);
                        generation.fetch_add(1, Ordering::Relaxed); // 届いた→次フレームで再描画させる
                    }
                    // 失敗はfailedへ記録し、クールダウン明けまで再登録させない(#56)。cache未挿入のままなので
                    // クールダウンが明けて再登録されれば通常通りリトライされる(一時的な障害からは回復できる)。
                    Err(_) => { pending.lock().unwrap().failed.insert(k.clone(), std::time::Instant::now()); }
                }
                // 成否に関わらず inflight から外す。
                pending.lock().unwrap().inflight.remove(&k);
            }
        }
    }
}

// 仮表示フォールバックで試すスタイル一覧(現styleを除いてこの順に探す)。settings.rs のスタイル定義と揃える。
const FALLBACK_STYLES: [&str; 5] = ["osm", "voyager", "dark", "light", "topo"];

// 未取得タイル(現styleではメモリキャッシュに無い)について、他styleの同一z/x/yがキャッシュにあれば
// それを仮表示として流用する。Cache::get は &mut self かつ返り値の借用が cache を可変借用し続けるため、
// 見つかった画像は即 clone して所有権付きで返し、呼び出し側の借用スコープを単純に保つ(256x256のcloneは安価)。
fn find_fallback_tile(cache: &mut Cache, current_style: &str, z: u32, x: i64, y: i64) -> Option<RgbaImage> {
    for &s in FALLBACK_STYLES.iter() {
        if s == current_style { continue; }
        let key = TileKey { src: TileSource::Base(s.to_string()), z, x, y };
        if let Some(img) = cache.get(&key) { return Some(img.clone()); }
    }
    None
}

// "LOADING" 透かし用の 5x7 ドットマトリクスフォント。1=点灯、上位ビットが左端の列。
fn glyph_bits(c: char) -> [u8; 7] {
    match c {
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        'I' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111],
        'N' => [0b10001, 0b11001, 0b10101, 0b10101, 0b10011, 0b10001, 0b10001],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        _ => [0; 7],
    }
}

// フォント寸法と描画拡大率。1ドット=SCALE×SCALE実ピクセル。
const GLYPH_W: u32 = 5;
const GLYPH_H: u32 = 7;
const GLYPH_SCALE: u32 = 4;

// 未取得タイル(グレー or 他style代用)の中央に "LOADING" を1回描く薄い透かし。
// ox,oy=このタイルのキャンバス上左上オフセット。ink=文字色(背景より少し暗いグレーで控えめに)。
// 総幅 = 7文字 × (GLYPH_W+1)×SCALE = 168px、高さ = GLYPH_H×SCALE = 28px を 256px 角の中央へ置く。
fn draw_loading_watermark(canvas: &mut RgbImage, ox: u32, oy: u32, ink: image::Rgb<u8>) {
    let word = "LOADING";
    let char_w = (GLYPH_W + 1) * GLYPH_SCALE; // 字幅 + 1列ぶんの字間
    let word_w = word.chars().count() as u32 * char_w;
    let word_h = GLYPH_H * GLYPH_SCALE;
    let start_x = ox + (TILE - word_w) / 2;
    let start_y = oy + (TILE - word_h) / 2;
    for (ci, ch) in word.chars().enumerate() {
        let bits = glyph_bits(ch);
        let cx0 = start_x + ci as u32 * char_w;
        for (row, rowbits) in bits.iter().enumerate() {
            for col in 0..GLYPH_W {
                // 上位ビットが左端の列: col=0 は bit(GLYPH_W-1)。
                if (rowbits >> (GLYPH_W - 1 - col)) & 1 == 1 {
                    let px0 = cx0 + col * GLYPH_SCALE;
                    let py0 = start_y + row as u32 * GLYPH_SCALE;
                    for dy in 0..GLYPH_SCALE {
                        for dx in 0..GLYPH_SCALE {
                            canvas.put_pixel(px0 + dx, py0 + dy, ink);
                        }
                    }
                }
            }
        }
    }
}

// build_window の非ブロッキング版。未取得タイルはネットワークを待たずグレーのプレースホルダーで埋め、
// ローダーへ取得依頼だけ出して即座に返す。届いたタイルは次フレームで cache から拾われ自動的に地図へ反映される。
//
// subpixel=true のとき、窓の切り出しに cx/cy の小数部を使う(crop_window_subpixel・設計 §5.1 対策A)。
// false のときは従来どおり整数位置で切り出す。呼び出し側が描画モードで選ぶ。
pub fn build_window_nowait(cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32, style: &str, subpixel: bool, loader: &TileLoader) -> Result<RgbImage, String> {
    if style == "topo" && z > TOPO_MAX_Z {
        // 同期版と同じオーバーズーム。z17相当のサブ窓を非ブロッキングで組み(未取得はグレー)、拡大→中央クロップ。
        // z17タイルはローダーへ登録され順次埋まっていく。
        let shift = z - TOPO_MAX_Z;
        let scale = (1u32 << shift) as f64;
        let (base_w, base_h, scaled_w, scaled_h, crop_x, crop_y) = overzoom_geometry(win_w, win_h, shift);
        let base_img = build_window_nowait(cx / scale, cy / scale, TOPO_MAX_Z, base_w, base_h, style, subpixel, loader)?;
        let resized = image::imageops::resize(&base_img, scaled_w, scaled_h, image::imageops::FilterType::Nearest);
        return Ok(image::imageops::crop_imm(&resized, crop_x, crop_y, win_w, win_h).to_image());
    }
    let tf = TILE as f64;
    let (tx_min, mut tx_max, ty_min, mut ty_max, left, top) = window_tile_range(cx, cy, win_w, win_h);
    if subpixel {
        // バイリニアは右下方向へ1ピクセル余分に参照する。その1ピクセルが隣のタイルへ掛かる
        // ときだけ列/行を1つ広げる(窓の右端がちょうどタイル境界に載ったときにだけ起きる)。
        // 広げないとその1列/1行だけクランプで隣と混ざらず、パン中に端が固まって見える。
        // 同じズームのタイルなので取得の枠組みは変わらない(設計 §5.1)。
        tx_max = tx_max.max(((left.floor() + win_w as f64) / tf).floor() as i64);
        ty_max = ty_max.max(((top.floor() + win_h as f64) / tf).floor() as i64);
    }
    let max_t = 2i64.pow(z);
    let cols = (tx_max - tx_min + 1) as u32;
    let rows = (ty_max - ty_min + 1) as u32;
    // 世界の端(範囲外タイル)は bg、範囲内で未取得のタイルは placeholder。bg(221 or 26)と見分けが付くグレー。
    let bg = if style == "dark" { image::Rgb([26, 26, 26]) } else { image::Rgb([221, 221, 221]) };
    let placeholder = image::Rgb([200u8, 200, 200]);
    // 透かしのink色は背景(グレー200 or 他style代用タイル)より少し暗いグレーにして薄く目立たせない。
    let watermark_ink = image::Rgb([150u8, 150, 150]);
    let mut canvas = RgbImage::from_pixel(cols * TILE, rows * TILE, bg);
    // 呼び出し側は従来どおり style 文字列で呼ぶ。取得元の型はここで組み立てる。
    let src = TileSource::Base(style.to_string());

    // cacheロックは1回だけ取り、範囲内タイルの描画/欠落判定をまとめて行いすぐ離す(1タイルごとの取り直しをしない)。
    let mut missing: Vec<TileKey> = Vec::new();
    {
        let mut cache = loader.shared.lock().unwrap();
        for ty in ty_min..=ty_max {
            if ty < 0 || ty >= max_t { continue; }
            for tx in tx_min..=tx_max {
                let wx = ((tx % max_t) + max_t) % max_t;
                let key = TileKey { src: src.clone(), z, x: wx, y: ty };
                let ox = (tx - tx_min) as u32 * TILE;
                let oy = (ty - ty_min) as u32 * TILE;
                // 地図タイルは alpha=255 の不透明画像なので、RGB 3成分だけ取って貼る。
                if let Some(t) = cache.get(&key) {
                    for (px, py, p) in t.enumerate_pixels() { canvas.put_pixel(ox + px, oy + py, image::Rgb([p[0], p[1], p[2]])); }
                } else {
                    // 未取得: 他styleの同一タイルがキャッシュにあれば仮表示に流用、無ければ薄いグレー。
                    // いずれの場合も本来のスタイルが未達であることが分かるよう LOADING 透かしを必ず重ねる。
                    if let Some(fb) = find_fallback_tile(&mut cache, style, z, wx, ty) {
                        for (px, py, p) in fb.enumerate_pixels() { canvas.put_pixel(ox + px, oy + py, image::Rgb([p[0], p[1], p[2]])); }
                    } else {
                        for py in 0..TILE { for px in 0..TILE { canvas.put_pixel(ox + px, oy + py, placeholder); } }
                    }
                    draw_loading_watermark(&mut canvas, ox, oy, watermark_ink);
                    missing.push(key);
                }
            }
        }
    }
    // 取得依頼はcacheロック解放後にまとめて登録(二重登録はローダー側で弾く)。
    if !missing.is_empty() { loader.request_tiles(missing); }

    // canvas 上での窓左上。window_tile_range の定義から常に 0 以上だが、念のため抑える。
    let ox = (left - tx_min as f64 * tf).max(0.0);
    let oy = (top - ty_min as f64 * tf).max(0.0);
    if subpixel {
        Ok(crop_window_subpixel(&canvas, ox, oy, win_w, win_h))
    } else {
        Ok(image::imageops::crop_imm(&canvas, ox as u32, oy as u32, win_w, win_h).to_image())
    }
}

// ---- 雨雲レーダー(気象庁ナウキャスト)レイヤ ----

// 降水ナウキャストのタイルは「偶数ズームの z4〜z10」にしか中身が無い。
// 奇数ズーム(z5/z7/z9…)と z11 以上は HTTP 200 で返るが全透明の空PNG(334バイト)で、
// これは実際に降っている場所でも同じ(2026/08/14 実測: 埼玉・ときがわ 8.0mm/10min の地点で
// z6/z8/z10 は 2〜4KB、z5/z7/z9/z11/z12 はいずれも 334バイト。他2地点でも同じ並び)。
// したがって要求ズームをそのまま投げると、ツーリングで常用する z11 以上では雨雲が一切出ない。
// データのあるズームで取得し、表示ズームへ最近傍で拡大して重ねる(元が250mメッシュの粗い面
// データなので、拡大でぼやけても情報は失われない)。
const RADAR_DATA_MIN_Z: u32 = 4;
const RADAR_DATA_MAX_Z: u32 = 10;

// 表示ズーム z に対して、実際にタイルを取りに行くズーム。
// z4未満(日本全体が画面に収まらない広域)は None = 雨雲を出さない。ここで z4 のタイルへ
// 引き上げてしまうと、世界全体の窓に対して z4 のタイルを何百枚も要求することになるため。
fn radar_source_zoom(z: u32) -> Option<u32> {
    if z < RADAR_DATA_MIN_Z { return None; }
    let sz = z.min(RADAR_DATA_MAX_Z);
    Some(sz - (sz % 2)) // 偶数へ切り下げ
}

// 雨雲レイヤ1枚ぶんのタイル配置。build_radar_window_nowait と radar_progress で
// タイル列挙のジオメトリを二重管理しない(ズレ防止)ため純粋関数に切り出す。
// tiles = (取得するタイルのキー, 折り返し前のタイルx, タイルy)。x は経度方向の折り返し前の値を
// 保持する(キーは折り返し後・表示位置の計算は折り返し前を使うため)。
struct RadarLayout {
    tiles: Vec<(TileKey, i64, i64)>,
    scale: f64,      // 表示px / ソースpx = 2^(z - source_z)
    left: f64, top: f64, // 表示ズームでの窓左上のグローバルpx
}

impl RadarLayout {
    // ソースタイル(tx,ty)が覆う表示座標の矩形 [x0,x1) × [y0,y1)(窓内にクリップ済み)。
    // 表示画素 d の中心は「表示グローバルpx = left + d + 0.5」、その位置のソースpx は /scale。
    fn dest_rect(&self, tx: i64, ty: i64, win_w: u32, win_h: u32) -> (u32, u32, u32, u32) {
        let tf = TILE as f64;
        let span = |t: i64, origin: f64, limit: u32| -> (u32, u32) {
            let a = ((t as f64 * tf) * self.scale - origin - 0.5).ceil();
            let b = (((t + 1) as f64 * tf) * self.scale - origin - 0.5).ceil();
            let a = a.max(0.0).min(limit as f64) as u32;
            let b = b.max(0.0).min(limit as f64) as u32;
            (a, b)
        };
        let (x0, x1) = span(tx, self.left, win_w);
        let (y0, y1) = span(ty, self.top, win_h);
        (x0, x1, y0, y1)
    }
}

// 窓(中心cx,cy グローバルpx / win_w×win_h・表示ズームz)に対して、どのタイルをどう貼るかを決める。
// 視野がナウキャストの提供範囲(日本)に全くかからない場合、および広域すぎる場合は None を返す
// = 1枚もリクエストしない(公共サービスへの無駄打ちを避ける)。
fn radar_layout(cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32, frame: &crate::radar::Frame) -> Option<RadarLayout> {
    let sz = radar_source_zoom(z)?;
    let tf = TILE as f64;
    let left = cx - win_w as f64 / 2.0;
    let top = cy - win_h as f64 / 2.0;
    // 窓の対角2隅の緯度経度で圏域判定する(海外を表示しているときに無駄打ちしない)。
    let (lat_top, lon_left) = pixel_to_deg(left, top, z);
    let (lat_bottom, lon_right) = pixel_to_deg(left + win_w as f64, top + win_h as f64, z);
    if !crate::radar::covers_japan(lat_bottom, lon_left, lat_top, lon_right) { return None; }

    let scale = 2f64.powi(z as i32 - sz as i32);
    // ソースズームでの窓範囲 → タイル範囲
    let (s_left, s_top) = (left / scale, top / scale);
    let (s_right, s_bottom) = ((left + win_w as f64) / scale, (top + win_h as f64) / scale);
    let tx_min = (s_left / tf).floor() as i64;
    let tx_max = ((s_right - 1e-9) / tf).floor() as i64;
    let ty_min = (s_top / tf).floor() as i64;
    let ty_max = ((s_bottom - 1e-9) / tf).floor() as i64;
    let max_t = 2i64.pow(sz);
    let src = TileSource::Radar { basetime: frame.basetime.clone(), validtime: frame.validtime.clone(), product: frame.product };
    let mut tiles = Vec::new();
    for ty in ty_min..=ty_max {
        if ty < 0 || ty >= max_t { continue; } // 世界の上下端の外はタイルが存在しない
        for tx in tx_min..=tx_max {
            let wx = ((tx % max_t) + max_t) % max_t; // 経度方向は一周ぶんで折り返す
            tiles.push((TileKey { src: src.clone(), z: sz, x: wx, y: ty }, tx, ty));
        }
    }
    Some(RadarLayout { tiles, scale, left, top })
}

// 雨雲レイヤの窓を組む(非ブロッキング)。取得済みタイルだけを貼り、未取得タイルの領域は
// 「全透明」のまま返す。グレーのプレースホルダーも LOADING 透かしも描かない: このレイヤの下には
// 既に地図が描かれており、そこにグレーの箱や文字を重ねると地図が読めなくなるため。
// 読込中であることはステータス行(radar_progress の枚数)で伝える。
// 視野が日本国外/広域すぎる場合は None(1枚もリクエストしない)。
//
// データのあるズーム(偶数z4〜z10)で取得したタイルを、表示ズームへ最近傍で拡大しながら
// 表示画素へ直接書く。中間の巨大キャンバスを作らないので、拡大率が大きくても
// 確保するのは出力サイズぶんだけで済み、位置ズレも生じない。
pub fn build_radar_window_nowait(
    cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32,
    frame: &crate::radar::Frame, loader: &TileLoader,
) -> Option<RgbaImage> {
    let layout = radar_layout(cx, cy, z, win_w, win_h, frame)?;
    let tf = TILE as f64;
    let mut canvas = RgbaImage::from_pixel(win_w, win_h, image::Rgba([0, 0, 0, 0]));
    // cacheロックは1回だけ取り、描画/欠落判定をまとめて行いすぐ離す。
    let mut missing: Vec<TileKey> = Vec::new();
    {
        let mut cache = loader.shared.lock().unwrap();
        for (key, tx, ty) in &layout.tiles {
            let (x0, x1, y0, y1) = layout.dest_rect(*tx, *ty, win_w, win_h);
            let Some(t) = cache.get(key) else { missing.push(key.clone()); continue };
            let (iw, ih) = t.dimensions();
            if iw == 0 || ih == 0 { continue; }
            // タイル左上のソースpx。ここからの相対位置で元画素を引く。
            let (tsx, tsy) = (*tx as f64 * tf, *ty as f64 * tf);
            for dy in y0..y1 {
                let sy = ((layout.top + dy as f64 + 0.5) / layout.scale - tsy).floor();
                let py = (sy.max(0.0) as u32).min(ih - 1);
                for dx in x0..x1 {
                    let sx = ((layout.left + dx as f64 + 0.5) / layout.scale - tsx).floor();
                    let px = (sx.max(0.0) as u32).min(iw - 1);
                    canvas.put_pixel(dx, dy, *t.get_pixel(px, py));
                }
            }
        }
    }
    // 取得依頼はcacheロック解放後にまとめて登録(二重登録・失敗クールダウンはローダー側で弾く)。
    if !missing.is_empty() { loader.request_tiles(missing); }
    Some(canvas)
}

// 表示中フレームの読込進捗(ステータス行用)。(取得済み枚数, 必要枚数)。
// 視野が日本国外/広域すぎる場合は (0, 0) を返す = 呼び出し側はこれを「範囲外」の表示に使う
// (圏内なら窓は必ず1枚以上のタイルを覆うので、必要枚数0にはならない)。
pub fn radar_progress(loader: &TileLoader, cx: f64, cy: f64, z: u32,
                      win_w: u32, win_h: u32, frame: &crate::radar::Frame) -> (usize, usize) {
    let Some(layout) = radar_layout(cx, cy, z, win_w, win_h, frame) else { return (0, 0) };
    let cache = loader.shared.lock().unwrap();
    let got = layout.tiles.iter().filter(|(k, _, _)| cache.contains(k)).count();
    (got, layout.tiles.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 30日未満は新鮮、30日以上は期限切れ。
    #[test]
    fn is_tile_fresh_boundary() {
        assert!(is_tile_fresh(std::time::Duration::from_secs(29 * 24 * 60 * 60)));
        assert!(!is_tile_fresh(std::time::Duration::from_secs(30 * 24 * 60 * 60)));
        assert!(!is_tile_fresh(std::time::Duration::from_secs(31 * 24 * 60 * 60)));
        assert!(is_tile_fresh(std::time::Duration::from_secs(0)));
    }

    // 失敗クールダウン境界(#56): 30秒未満はクールダウン中(再登録しない)、30秒以上は明け(再試行してよい)。
    #[test]
    fn in_cooldown_boundary() {
        assert!(in_cooldown(std::time::Duration::from_secs(0)));
        assert!(in_cooldown(std::time::Duration::from_secs(29)));
        assert!(!in_cooldown(std::time::Duration::from_secs(30)));
        assert!(!in_cooldown(std::time::Duration::from_secs(31)));
    }

    // 各種 win_w/win_h/shift の組み合わせで、拡大後(scaled_w/h)が要求サイズ(win_w/h)以上に
    // なること(=crop_immがパニックしない前提条件)を確認する。
    #[test]
    fn overzoom_geometry_scaled_size_covers_requested_window() {
        for win_w in [1u32, 2, 111, 448, 1024] {
            for win_h in [1u32, 43, 344, 900] {
                for shift in 1u32..=6 {
                    let (base_w, base_h, scaled_w, scaled_h, crop_x, crop_y) = overzoom_geometry(win_w, win_h, shift);
                    assert!(scaled_w >= win_w, "win_w={win_w} shift={shift}: scaled_w={scaled_w}");
                    assert!(scaled_h >= win_h, "win_h={win_h} shift={shift}: scaled_h={scaled_h}");
                    assert!(crop_x + win_w <= scaled_w);
                    assert!(crop_y + win_h <= scaled_h);
                    assert!(base_w > 0 && base_h > 0);
                }
            }
        }
    }

    // shiftが大きいほど、z17相当で取得する窓(base_w/h)は小さくなる(オーバーズーム倍率が上がるため)。
    #[test]
    fn overzoom_geometry_base_window_shrinks_as_shift_grows() {
        let (base_w1, base_h1, ..) = overzoom_geometry(1000, 1000, 1);
        let (base_w3, base_h3, ..) = overzoom_geometry(1000, 1000, 3);
        assert!(base_w3 < base_w1);
        assert!(base_h3 < base_h1);
    }

    // クロップは中央寄せ(左右/上下の余白がほぼ等しい)。
    #[test]
    fn overzoom_geometry_crop_is_centered() {
        let (_, _, scaled_w, scaled_h, crop_x, crop_y) = overzoom_geometry(300, 200, 2);
        let right_margin = scaled_w - crop_x - 300;
        let bottom_margin = scaled_h - crop_y - 200;
        assert!(crop_x.abs_diff(right_margin) <= 1);
        assert!(crop_y.abs_diff(bottom_margin) <= 1);
    }

    // タイル自身の中心にちょうどviewを置き同一ズームで見ると距離は(丸め誤差内で)0。
    // pixel_to_deg→deg_to_pixel の往復が恒等であることも兼ねて確認する。
    #[test]
    fn tile_distance_zero_at_tile_center_same_zoom() {
        let z = 14u32;
        let (x, y) = (1000i64, 2000i64);
        let vcx = (x as f64 + 0.5) * TILE as f64;
        let vcy = (y as f64 + 0.5) * TILE as f64;
        let d = tile_distance_to_view(z, x, y, vcx, vcy, z);
        assert!(d < 1e-6, "d={d}");
    }

    // 同一ズームでは、view中心から遠いタイルほど距離が大きい(近傍優先の基本性質)。
    #[test]
    fn tile_distance_grows_with_offset_same_zoom() {
        let z = 12u32;
        let vcx = 500.0 * TILE as f64;
        let vcy = 500.0 * TILE as f64;
        let near = tile_distance_to_view(z, 500, 500, vcx, vcy, z);
        let far = tile_distance_to_view(z, 520, 500, vcx, vcy, z);
        assert!(far > near, "near={near} far={far}");
    }

    // 異なるズーム(topoオーバーズーム相当: 実タイルz17 / view z19)でも、view中心を覆うz17タイルが
    // 遠いz17タイルより近いと判定される(再投影して比較できていること)。
    #[test]
    fn tile_distance_cross_zoom_prioritizes_center() {
        let vz = 19u32;
        let (lat, lon) = (35.68, 139.76); // 東京近辺の適当な地点
        let (vcx, vcy) = deg_to_pixel(lat, lon, vz);
        let (px17, py17) = deg_to_pixel(lat, lon, 17);
        let (cx17, cy17) = ((px17 / TILE as f64).floor() as i64, (py17 / TILE as f64).floor() as i64);
        let near = tile_distance_to_view(17, cx17, cy17, vcx, vcy, vz);
        let far = tile_distance_to_view(17, cx17 + 5, cy17 + 5, vcx, vcy, vz);
        assert!(far > near, "near={near} far={far}");
    }

    // タイル中央に中心を置いた窓は、少なくともそのタイルを範囲に含み、left/top は窓左上に一致する。
    #[test]
    fn window_tile_range_covers_center_tile() {
        let cx = 10.5 * TILE as f64;
        let cy = 10.5 * TILE as f64;
        let (tx_min, tx_max, ty_min, ty_max, left, top) = window_tile_range(cx, cy, 100, 100);
        assert!(tx_min <= 10 && 10 <= tx_max);
        assert!(ty_min <= 10 && 10 <= ty_max);
        assert!((left - (cx - 50.0)).abs() < 1e-9);
        assert!((top - (cy - 50.0)).abs() < 1e-9);
    }

    // タイル境界(px=256)をまたぐ窓は列・行が2つ以上になる。
    #[test]
    fn window_tile_range_spans_boundary() {
        let (tx_min, tx_max, ty_min, ty_max, ..) = window_tile_range(256.0, 256.0, 200, 200);
        assert!(tx_max > tx_min);
        assert!(ty_max > ty_min);
    }

    // "LOADING" を構成する7文字は全て何らかのドットが点灯している(空グリフだと透かしが出ない)。
    #[test]
    fn glyph_bits_non_empty_for_word_letters() {
        for c in ['L', 'O', 'A', 'D', 'I', 'N', 'G'] {
            let bits = glyph_bits(c);
            assert!(bits.iter().any(|&b| b != 0), "glyph {c} has no lit dots");
        }
    }

    // find_fallback_tile: (a)同一styleは候補にしない (b)他styleがあれば返す (c)どこにも無ければNone。
    #[test]
    fn find_fallback_tile_behaviors() {
        let z = 10u32;
        let (x, y) = (100i64, 200i64);

        // (a) キャッシュには "dark" だけ。current_style も "dark" なら自分自身は候補外 → None。
        let mut cache = Cache::new();
        cache.insert(TileKey { src: TileSource::Base("dark".to_string()), z, x, y }, RgbaImage::from_pixel(TILE, TILE, image::Rgba([1, 2, 3, 255])));
        assert!(find_fallback_tile(&mut cache, "dark", z, x, y).is_none());

        // (b) current_style="osm" なら他style "dark" のタイルがそのまま返る(色も維持)。
        let got = find_fallback_tile(&mut cache, "osm", z, x, y);
        assert!(got.is_some());
        assert_eq!(*got.unwrap().get_pixel(0, 0), image::Rgba([1, 2, 3, 255]));

        // (c) 空キャッシュならどのstyleにも無く None。
        let mut empty = Cache::new();
        assert!(find_fallback_tile(&mut empty, "osm", z, x, y).is_none());
    }

    // TileSource::Base の url() は従来の tile_url と同じURLを組む(取得元の型化で叩き先が変わっていないこと)。
    #[test]
    fn tile_source_base_url_matches_tile_url() {
        for style in ["osm", "voyager", "dark", "light", "topo", "unknown"] {
            let src = TileSource::Base(style.to_string());
            assert_eq!(src.url(10, 20, 30), tile_url(style, 10, 20, 30), "style={style}");
        }
    }

    // Cache は RGBA のまま値を保持する(アルファが潰れない)。
    #[test]
    fn cache_roundtrip_preserves_rgba() {
        let mut cache = Cache::new();
        let key = TileKey { src: TileSource::Base("osm".to_string()), z: 5, x: 1, y: 2 };
        cache.insert(key.clone(), RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 128])));
        assert!(cache.contains(&key));
        assert_eq!(*cache.get(&key).unwrap().get_pixel(0, 0), image::Rgba([10, 20, 30, 128]));
    }

    // 取得元が違えば別エントリ(style違いのタイルが混ざらない = 従来 style フィールドが担っていた性質)。
    #[test]
    fn cache_key_separates_sources() {
        let mut cache = Cache::new();
        let osm = TileKey { src: TileSource::Base("osm".to_string()), z: 5, x: 1, y: 2 };
        let dark = TileKey { src: TileSource::Base("dark".to_string()), z: 5, x: 1, y: 2 };
        cache.insert(osm.clone(), RgbaImage::from_pixel(1, 1, image::Rgba([1, 1, 1, 255])));
        assert!(cache.contains(&osm));
        assert!(!cache.contains(&dark));
    }

    // 上限超過で最終アクセスが最古のものから落ちる(簡易LRUが型変更後も効いている)。
    #[test]
    fn cache_evicts_oldest_over_cap() {
        let mut cache = Cache::new();
        let cap = cache.base_cap;
        let key = |i: i64| TileKey { src: TileSource::Base("osm".to_string()), z: 5, x: i, y: 0 };
        for i in 0..cap as i64 { cache.insert(key(i), RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]))); }
        assert!(cache.contains(&key(0)));
        cache.insert(key(cap as i64), RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255])));
        assert!(!cache.contains(&key(0)), "最古のエントリが退避されていない");
        assert!(cache.contains(&key(cap as i64)));
    }

    // ---- 雨雲レーダー ----

    fn radar_frame(basetime: &str, validtime: &str) -> crate::radar::Frame {
        crate::radar::Frame {
            basetime: basetime.to_string(),
            validtime: validtime.to_string(),
            kind: crate::radar::FrameKind::Observed,
            product: crate::radar::RadarProduct::Nowcast,
        }
    }
    fn radar_key(basetime: &str, validtime: &str, x: i64) -> TileKey {
        TileKey { src: TileSource::Radar { basetime: basetime.to_string(), validtime: validtime.to_string(), product: crate::radar::RadarProduct::Nowcast }, z: 10, x, y: 0 }
    }
    fn px(v: u8) -> RgbaImage { RgbaImage::from_pixel(1, 1, image::Rgba([v, v, v, 255])) }
    // 東京(35.68,139.76)を中心にした窓のグローバルpx。
    fn tokyo_center(z: u32) -> (f64, f64) { deg_to_pixel(35.68, 139.76, z) }

    // 雨雲タイルのURLは JMA ナウキャストの形式で、basetime/validtime が正しい位置に入る。
    #[test]
    fn radar_tile_url_has_basetime_and_validtime_in_place() {
        let src = TileSource::Radar { basetime: "20260814125500".into(), validtime: "20260814131000".into(), product: crate::radar::RadarProduct::Nowcast };
        assert_eq!(
            src.url(10, 909, 403),
            "https://www.jma.go.jp/bosai/jmatile/data/nowc/20260814125500/none/20260814131000/surf/hrpns/10/909/403.png"
        );
    }

    // 降水短時間予報(延長分)のタイルURLは lv2/element が rasrf になる。
    #[test]
    fn radar_tile_url_short_term_uses_rasrf_path() {
        let src = TileSource::Radar { basetime: "20260816050000".into(), validtime: "20260816090000".into(), product: crate::radar::RadarProduct::ShortTerm };
        assert_eq!(
            src.url(8, 227, 100),
            "https://www.jma.go.jp/bosai/jmatile/data/rasrf/20260816050000/none/20260816090000/surf/rasrf/8/227/100.png"
        );
    }

    // 雨雲タイルはディスクキャッシュに乗せない(basetime入りでヒットせず、TTLで古い雨を出す危険)。
    #[test]
    fn radar_tiles_are_not_disk_cached() {
        let src = TileSource::Radar { basetime: "20260814125500".into(), validtime: "20260814125500".into(), product: crate::radar::RadarProduct::Nowcast };
        assert!(src.cache_path(10, 1, 2).is_none());
        assert!(src.is_radar());
        assert!(!TileSource::Base("osm".to_string()).is_radar());
    }

    // basetime/validtime が違えば別エントリ(コマを跨いでタイルが混ざらない)。
    #[test]
    fn cache_key_separates_radar_frames() {
        let mut cache = Cache::new();
        let a = radar_key("20260814125500", "20260814125500", 1);
        let b = radar_key("20260814125500", "20260814130000", 1); // validtime違い
        let c = radar_key("20260814125000", "20260814125500", 1); // basetime違い
        cache.insert(a.clone(), px(1));
        assert!(cache.contains(&a));
        assert!(!cache.contains(&b));
        assert!(!cache.contains(&c));
    }

    // 種別ごとにLRU予算が独立している: 雨雲を上限いっぱい投入しても地図タイルは落ちない。
    #[test]
    fn cache_budgets_are_independent_per_source() {
        let mut cache = Cache::new();
        let base = TileKey { src: TileSource::Base("osm".to_string()), z: 5, x: 0, y: 0 };
        cache.insert(base.clone(), px(9));
        for i in 0..(cache.radar_cap as i64 * 2) {
            cache.insert(radar_key("20260814125500", "20260814125500", i), px(1));
        }
        assert!(cache.contains(&base), "雨雲の大量投入で地図タイルが追い出された");
        // 雨雲側は自分の予算内に収まっている。
        assert!(cache.radar.len() <= cache.radar_cap);
    }

    // retain_radar_frames: 新しい一覧に無いコマだけ捨て、残るコマと地図タイルには触れない。
    #[test]
    fn retain_radar_frames_drops_only_stale_frames() {
        let mut cache = Cache::new();
        let base = TileKey { src: TileSource::Base("osm".to_string()), z: 5, x: 0, y: 0 };
        let keep = radar_key("20260814130000", "20260814130000", 1);
        let stale = radar_key("20260814125500", "20260814125500", 1);
        cache.insert(base.clone(), px(9));
        cache.insert(keep.clone(), px(1));
        cache.insert(stale.clone(), px(2));
        cache.retain_radar_frames(&[radar_frame("20260814130000", "20260814130000")]);
        assert!(cache.contains(&keep));
        assert!(!cache.contains(&stale));
        assert!(cache.contains(&base), "地図タイルを巻き込んで捨てている");
    }

    // 空の keep(異常系)でも雨雲だけが全消しになり、地図タイルは残る。
    #[test]
    fn retain_radar_frames_with_empty_keep_clears_radar_only() {
        let mut cache = Cache::new();
        let base = TileKey { src: TileSource::Base("osm".to_string()), z: 5, x: 0, y: 0 };
        let r = radar_key("20260814125500", "20260814125500", 1);
        cache.insert(base.clone(), px(9));
        cache.insert(r.clone(), px(1));
        cache.retain_radar_frames(&[]);
        assert!(!cache.contains(&r));
        assert!(cache.contains(&base));
    }

    // 日本国外(ハワイ)の視野では1枚も要求しない = レイアウトが None。
    #[test]
    fn radar_layout_is_none_outside_japan() {
        let z = 10u32;
        let (cx, cy) = deg_to_pixel(21.3, -157.8, z); // ホノルル
        assert!(radar_layout(cx, cy, z, 256, 256, &radar_frame("20260814125500", "20260814125500")).is_none());
    }

    // 取得ズームの決定: データがあるのは偶数ズームの z4〜z10 だけ。奇数/z11以上は切り下げ、
    // z4未満は None(広域では雨雲を出さない = z4タイルを何百枚も要求しない)。
    #[test]
    fn radar_source_zoom_snaps_to_even_zoom_up_to_ten() {
        assert_eq!(radar_source_zoom(0), None);
        assert_eq!(radar_source_zoom(3), None);
        assert_eq!(radar_source_zoom(4), Some(4));
        assert_eq!(radar_source_zoom(5), Some(4));
        assert_eq!(radar_source_zoom(6), Some(6));
        assert_eq!(radar_source_zoom(9), Some(8));
        assert_eq!(radar_source_zoom(10), Some(10));
        assert_eq!(radar_source_zoom(11), Some(10));
        assert_eq!(radar_source_zoom(16), Some(10));
        assert_eq!(radar_source_zoom(19), Some(10));
    }

    // 広域(z4未満)では日本が視野に入っていても1枚も要求しない。
    #[test]
    fn radar_layout_is_none_on_very_wide_view() {
        let (cx, cy) = tokyo_center(3);
        assert!(radar_layout(cx, cy, 3, 300, 200, &radar_frame("20260814125500", "20260814125500")).is_none());
    }

    // 日本国内なら窓を覆うタイルが列挙され、キーのズームは取得ズーム(偶数z4〜z10)になる。
    #[test]
    fn radar_layout_covers_window_inside_japan() {
        let z = 10u32;
        let (cx, cy) = tokyo_center(z);
        let lay = radar_layout(cx, cy, z, 300, 200, &radar_frame("20260814125500", "20260814125500")).unwrap();
        assert!(!lay.tiles.is_empty());
        assert!(lay.tiles.iter().all(|(k, _, _)| matches!(k.src, TileSource::Radar { .. })));
        assert!(lay.tiles.iter().all(|(k, _, _)| k.z == 10));
        assert_eq!(lay.scale, 1.0);
        // 深いズームでは取得ズームがz10に張り付き、必要タイル数はむしろ減る(拡大表示するため)。
        let (dcx, dcy) = tokyo_center(16);
        let deep = radar_layout(dcx, dcy, 16, 300, 200, &radar_frame("20260814125500", "20260814125500")).unwrap();
        assert!(deep.tiles.iter().all(|(k, _, _)| k.z == 10));
        assert_eq!(deep.scale, 64.0);
        assert!(deep.tiles.len() <= lay.tiles.len(), "拡大表示なのにタイル数が増えている");
    }

    // 表示ズーム=取得ズームのとき、貼られる画素は同じ位置のタイル画素と1:1で一致する。
    #[test]
    fn build_radar_window_nowait_maps_pixels_one_to_one_at_source_zoom() {
        let z = 10u32; // 取得ズームと同じ = 等倍
        let frame = radar_frame("20260814125500", "20260814125500");
        let (cx, cy) = tokyo_center(z);
        let (tx, ty) = ((cx / TILE as f64).floor() as i64, (cy / TILE as f64).floor() as i64);
        // 画素ごとに違う値を持つタイル(位置の対応が崩れたら検出できる)
        let mut tile = RgbaImage::new(TILE, TILE);
        for (x, y, p) in tile.enumerate_pixels_mut() { *p = image::Rgba([x as u8, y as u8, 0, 255]); }
        let cache = Arc::new(Mutex::new(Cache::new()));
        cache.lock().unwrap().insert(
            TileKey { src: TileSource::Radar { basetime: frame.basetime.clone(), validtime: frame.validtime.clone(), product: frame.product }, z, x: tx, y: ty },
            tile,
        );
        let loader = TileLoader::start(Arc::clone(&cache));
        // タイル左上から16px内側に窓の左上が来るように中心を置く
        let ccx = tx as f64 * TILE as f64 + 16.0 + 8.0;
        let ccy = ty as f64 * TILE as f64 + 32.0 + 8.0;
        let img = build_radar_window_nowait(ccx, ccy, z, 16, 16, &frame, &loader).unwrap();
        assert_eq!(img.dimensions(), (16, 16));
        assert_eq!(*img.get_pixel(0, 0), image::Rgba([16, 32, 0, 255]));
        assert_eq!(*img.get_pixel(1, 0), image::Rgba([17, 32, 0, 255]));
        assert_eq!(*img.get_pixel(0, 1), image::Rgba([16, 33, 0, 255]));
        assert_eq!(*img.get_pixel(15, 15), image::Rgba([31, 47, 0, 255]));
    }

    // 表示ズームが取得ズームより深いときは、最近傍で拡大される(1ソース画素が scale×scale の塊になる)。
    #[test]
    fn build_radar_window_nowait_magnifies_when_zoomed_in() {
        let z = 12u32; // 取得はz10 → scale=4
        let frame = radar_frame("20260814125500", "20260814125500");
        let (cx, cy) = tokyo_center(z);
        let (stx, sty) = ((cx / 4.0 / TILE as f64).floor() as i64, (cy / 4.0 / TILE as f64).floor() as i64);
        let mut tile = RgbaImage::new(TILE, TILE);
        for (x, y, p) in tile.enumerate_pixels_mut() { *p = image::Rgba([x as u8, y as u8, 0, 255]); }
        let cache = Arc::new(Mutex::new(Cache::new()));
        cache.lock().unwrap().insert(
            TileKey { src: TileSource::Radar { basetime: frame.basetime.clone(), validtime: frame.validtime.clone(), product: frame.product }, z: 10, x: stx, y: sty },
            tile,
        );
        let loader = TileLoader::start(Arc::clone(&cache));
        // 表示ズームでの窓左上が、ソースタイル内の (10, 20) px ちょうどに来るように中心を置く。
        let ccx = (stx as f64 * TILE as f64 + 10.0) * 4.0 + 8.0;
        let ccy = (sty as f64 * TILE as f64 + 20.0) * 4.0 + 8.0;
        let img = build_radar_window_nowait(ccx, ccy, z, 16, 16, &frame, &loader).unwrap();
        // 左上4x4は全てソース(10,20)、その右隣4x4は(11,20)。
        for dy in 0..4 { for dx in 0..4 {
            assert_eq!(*img.get_pixel(dx, dy), image::Rgba([10, 20, 0, 255]), "({dx},{dy})");
        }}
        assert_eq!(*img.get_pixel(4, 0), image::Rgba([11, 20, 0, 255]));
        assert_eq!(*img.get_pixel(0, 4), image::Rgba([10, 21, 0, 255]));
        assert_eq!(*img.get_pixel(15, 15), image::Rgba([13, 23, 0, 255]));
    }

    // 未取得タイルの領域は「全透明」で返る(グレーの箱もLOADING透かしも描かない)。
    // 必要タイルを事前に failed(クールダウン中)へ入れておき、テストが実際にJMAを叩かないようにする。
    #[test]
    fn build_radar_window_nowait_fills_missing_with_transparent() {
        let z = 10u32;
        let (cx, cy) = tokyo_center(z);
        let cache = Arc::new(Mutex::new(Cache::new()));
        let loader = TileLoader::start(Arc::clone(&cache));
        let frame = radar_frame("20260814125500", "20260814125500");
        {
            let lay = radar_layout(cx, cy, z, 64, 64, &frame).unwrap();
            let mut p = loader.pending.lock().unwrap();
            for (k, _, _) in &lay.tiles { p.failed.insert(k.clone(), std::time::Instant::now()); }
        }
        let img = build_radar_window_nowait(cx, cy, z, 64, 64, &frame, &loader).unwrap();
        assert_eq!(img.dimensions(), (64, 64));
        assert!(img.pixels().all(|p| p[3] == 0), "未取得の領域が透明になっていない");
        assert!(loader.pending.lock().unwrap().queued.is_empty(), "クールダウン中のタイルを再登録している");
    }

    // キャッシュ済みの雨雲タイルはアルファを保ったまま窓へ貼られる。
    #[test]
    fn build_radar_window_nowait_copies_cached_alpha() {
        let z = 10u32;
        let (cx, cy) = tokyo_center(z);
        let (tx, ty) = ((cx / TILE as f64).floor() as i64, (cy / TILE as f64).floor() as i64);
        let frame = radar_frame("20260814125500", "20260814125500");
        let cache = Arc::new(Mutex::new(Cache::new()));
        cache.lock().unwrap().insert(
            TileKey { src: TileSource::Radar { basetime: frame.basetime.clone(), validtime: frame.validtime.clone(), product: frame.product }, z, x: tx, y: ty },
            RgbaImage::from_pixel(TILE, TILE, image::Rgba([10, 20, 30, 128])),
        );
        let loader = TileLoader::start(Arc::clone(&cache));
        // タイル中心に窓を置き、1タイルの内側に収める(未取得タイルが混ざらない)。
        let ccx = (tx as f64 + 0.5) * TILE as f64;
        let ccy = (ty as f64 + 0.5) * TILE as f64;
        let img = build_radar_window_nowait(ccx, ccy, z, 32, 32, &frame, &loader).unwrap();
        assert!(img.pixels().all(|p| *p == image::Rgba([10, 20, 30, 128])));
    }

    // 同じ地点を別ズームで見たとき、窓の中心には同じ地理点(=同じソース画素)が来る。
    // 拡大時に位置がずれていないこと(オフセットの取り違え)の回帰テスト。
    #[test]
    fn build_radar_window_nowait_center_is_same_place_across_zooms() {
        let frame = radar_frame("20260814125500", "20260814125500");
        let (lat, lon) = (35.99, 139.2067);
        let (bx, by) = deg_to_pixel(lat, lon, 10);
        let (stx, sty) = ((bx / TILE as f64).floor() as i64, (by / TILE as f64).floor() as i64);
        let mut tile = RgbaImage::new(TILE, TILE);
        for (x, y, p) in tile.enumerate_pixels_mut() { *p = image::Rgba([x as u8, y as u8, 0, 255]); }
        let cache = Arc::new(Mutex::new(Cache::new()));
        cache.lock().unwrap().insert(
            TileKey { src: TileSource::Radar { basetime: frame.basetime.clone(), validtime: frame.validtime.clone(), product: frame.product }, z: 10, x: stx, y: sty },
            tile,
        );
        let loader = TileLoader::start(Arc::clone(&cache));
        let center_px = |z: u32| -> image::Rgba<u8> {
            let (cx, cy) = deg_to_pixel(lat, lon, z);
            let img = build_radar_window_nowait(cx, cy, z, 16, 16, &frame, &loader).unwrap();
            *img.get_pixel(8, 8)
        };
        let at10 = center_px(10);
        assert_eq!(at10[3], 255, "z10でソース画素が引けていない");
        // 等倍(z10)・4倍(z12)・64倍(z16)のいずれでも中心は同じ地理点を指す。
        assert_eq!(center_px(12), at10);
        assert_eq!(center_px(16), at10);
        // 奇数ズーム(取得はz10へ切り下げ)でも同じ。
        assert_eq!(center_px(13), at10);
    }

    // radar_progress: 圏外は(0,0)、圏内は必要枚数を返し、キャッシュに入るほど取得済みが増える。
    #[test]
    fn radar_progress_counts_cached_tiles() {
        let z = 10u32;
        let (cx, cy) = tokyo_center(z);
        let frame = radar_frame("20260814125500", "20260814125500");
        let cache = Arc::new(Mutex::new(Cache::new()));
        let loader = TileLoader::start(Arc::clone(&cache));
        let (got, need) = radar_progress(&loader, cx, cy, z, 300, 200, &frame);
        assert_eq!(got, 0);
        assert!(need >= 1);
        // レイアウトが列挙するキーを1枚だけ入れると取得済みが1になる。
        let lay = radar_layout(cx, cy, z, 300, 200, &frame).unwrap();
        cache.lock().unwrap().insert(lay.tiles[0].0.clone(), RgbaImage::from_pixel(TILE, TILE, image::Rgba([0, 0, 0, 0])));
        let (got2, need2) = radar_progress(&loader, cx, cy, z, 300, 200, &frame);
        assert_eq!(got2, 1);
        assert_eq!(need2, need);
        // 圏外は (0,0)。
        let (hcx, hcy) = deg_to_pixel(21.3, -157.8, z);
        assert_eq!(radar_progress(&loader, hcx, hcy, z, 300, 200, &frame), (0, 0));
    }

    // 掃除対象の判定(純粋関数): 一覧に無い雨雲コマだけが false、残すコマと地図タイルは true。
    #[test]
    fn radar_key_is_kept_only_for_listed_frames() {
        let keep = [radar_frame("20260814130000", "20260814130000")];
        assert!(radar_key_is_kept(&radar_key("20260814130000", "20260814130000", 1), &keep));
        assert!(!radar_key_is_kept(&radar_key("20260814125500", "20260814125500", 1), &keep));
        // basetime だけ / validtime だけ一致でも残さない(コマの同一性は両方で決まる)。
        assert!(!radar_key_is_kept(&radar_key("20260814130000", "20260814131000", 1), &keep));
        assert!(!radar_key_is_kept(&radar_key("20260814125500", "20260814130000", 1), &keep));
        // 地図タイルは keep が空でも常に残す。
        let base = TileKey { src: TileSource::Base("osm".to_string()), z: 5, x: 0, y: 0 };
        assert!(radar_key_is_kept(&base, &keep));
        assert!(radar_key_is_kept(&base, &[]));
    }

    // TileLoader 経由の掃除: キャッシュと未着手キュー・失敗記録の全部から古いコマが消える。
    // ワーカーが queued の中身を実際に取りに行かないよう、inflight を上限まで埋めてから積む
    // (テストがJMAを叩かないようにするため。ダミーの inflight エントリは誰も fetch しない)。
    #[test]
    fn loader_drop_radar_frames_except_cleans_cache_and_pending() {
        let cache = Arc::new(Mutex::new(Cache::new()));
        let loader = TileLoader::start(Arc::clone(&cache));
        let keep = radar_key("20260814130000", "20260814130000", 1);
        let stale = radar_key("20260814125500", "20260814125500", 1);
        cache.lock().unwrap().insert(keep.clone(), px(1));
        cache.lock().unwrap().insert(stale.clone(), px(2));
        {
            let mut p = loader.pending.lock().unwrap();
            for i in 0..LOADER_WORKERS as i64 {
                p.inflight.insert(TileKey { src: TileSource::Base("osm".to_string()), z: 5, x: i, y: 99 });
            }
            p.queued.insert(stale.clone());
            p.failed.insert(stale.clone(), std::time::Instant::now());
        }
        loader.drop_radar_frames_except(&[radar_frame("20260814130000", "20260814130000")]);
        {
            let c = cache.lock().unwrap();
            assert!(c.contains(&keep));
            assert!(!c.contains(&stale));
        }
        let p = loader.pending.lock().unwrap();
        assert!(!p.queued.contains(&stale));
        assert!(!p.failed.contains_key(&stale));
    }

    // build_window_nowait: キャッシュ済みタイル(RGBA)の画素が、そのままのRGB値で窓へ貼られる。
    // 窓は1タイルの内側に収まるサイズにするので未取得タイルは無く、ローダーはネットワークに触れない。
    #[test]
    fn build_window_nowait_copies_cached_tile_pixels() {
        let z = 3u32; // 2^3=8タイル四方。中央付近のタイルを使い範囲外判定に掛からないようにする
        let (tx, ty) = (4i64, 4i64);
        let cache = Arc::new(Mutex::new(Cache::new()));
        cache.lock().unwrap().insert(
            TileKey { src: TileSource::Base("osm".to_string()), z, x: tx, y: ty },
            RgbaImage::from_pixel(TILE, TILE, image::Rgba([7, 8, 9, 255])),
        );
        let loader = TileLoader::start(Arc::clone(&cache));
        let cx = (tx as f64 + 0.5) * TILE as f64;
        let cy = (ty as f64 + 0.5) * TILE as f64;
        let img = build_window_nowait(cx, cy, z, 64, 64, "osm", false, &loader).unwrap();
        assert_eq!(img.dimensions(), (64, 64));
        for p in img.pixels() { assert_eq!(*p, image::Rgb([7, 8, 9])); }
    }

    // ---- サブピクセル切り出し(docs/web-pan-smoothness-design.md §5.1 対策A) ----

    // 横方向にグラデーションを持つ canvas(x がそのまま R 成分)。バイリニアの重みが
    // どう効いたかを画素値から直接読めるようにするための下地。
    fn ramp_canvas(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 0]))
    }

    // 小数部 0.0 なら従来の整数切り出しと完全に一致する(見た目を変えずに入れられることの確認)。
    #[test]
    fn crop_window_subpixel_matches_the_integer_crop_at_zero_fraction() {
        let canvas = ramp_canvas(64, 64);
        for &(ox, oy) in &[(0.0, 0.0), (5.0, 7.0), (13.0, 2.0)] {
            let sub = crop_window_subpixel(&canvas, ox, oy, 16, 16);
            let int = image::imageops::crop_imm(&canvas, ox as u32, oy as u32, 16, 16).to_image();
            assert_eq!(sub, int, "ox={ox} oy={oy}");
        }
    }

    // 小数部 0.5 で隣接ピクセルの中間色になる。
    #[test]
    fn crop_window_subpixel_blends_half_way_between_neighbours() {
        let canvas = ramp_canvas(64, 64);
        // 横だけ 0.5 ずらす: 出力(i,j) = (canvas[10+i] + canvas[11+i]) / 2 = 10+i+0.5 → 四捨五入で 11+i
        let img = crop_window_subpixel(&canvas, 10.5, 4.0, 8, 8);
        for i in 0..8u32 {
            assert_eq!(img.get_pixel(i, 0).0[0], 11 + i as u8, "x={i}");
            assert_eq!(img.get_pixel(i, 0).0[1], 4, "y stays put at fy=0");
        }
        // 縦だけ 0.5 ずらす。
        let img = crop_window_subpixel(&canvas, 4.0, 10.5, 8, 8);
        for j in 0..8u32 {
            assert_eq!(img.get_pixel(0, j).0[1], 11 + j as u8, "y={j}");
            assert_eq!(img.get_pixel(0, j).0[0], 4, "x stays put at fx=0");
        }
    }

    // 2色の境目が、小数部に応じて連続的に遷移する(階段ではなく色の遷移として見える)。
    #[test]
    fn crop_window_subpixel_moves_an_edge_continuously() {
        // x=0 が黒 / x=1 が白の縦エッジ。
        let canvas = RgbImage::from_fn(4, 4, |x, _| if x == 0 { image::Rgb([0, 0, 0]) } else { image::Rgb([255, 255, 255]) });
        let at = |f: f64| crop_window_subpixel(&canvas, f, 0.0, 1, 1).get_pixel(0, 0).0[0];
        assert_eq!(at(0.0), 0);
        assert_eq!(at(0.25), 64);  // 255*0.25 = 63.75 → 64
        assert_eq!(at(0.5), 128);  // 255*0.5  = 127.5 → 128
        assert_eq!(at(0.75), 191); // 255*0.75 = 191.25 → 191
        assert_eq!(at(1.0), 255);
        // 単調に増える = 進行方向と直交する揺り戻しが無い。
        let seq: Vec<u8> = (0..=8).map(|k| at(k as f64 / 8.0)).collect();
        assert!(seq.windows(2).all(|w| w[0] <= w[1]), "{seq:?}");
    }

    // 端(canvas の右下)で範囲外参照しない。クランプされて panic しない。
    #[test]
    fn crop_window_subpixel_clamps_at_the_canvas_edge() {
        let canvas = ramp_canvas(8, 8);
        // 窓が canvas の右下いっぱいに載っており、隣接ピクセルが存在しない位置。
        let img = crop_window_subpixel(&canvas, 7.5, 7.5, 1, 1);
        assert_eq!(img.dimensions(), (1, 1));
        assert_eq!(img.get_pixel(0, 0).0, [7, 7, 0]); // 4点とも (7,7) へクランプされる
        // 窓が canvas をはみ出すサイズでも panic しない。
        let img = crop_window_subpixel(&canvas, 6.3, 6.7, 4, 4);
        assert_eq!(img.dimensions(), (4, 4));
    }

    // 壊れた値でも panic せず、必ず窓サイズの画像を返す。
    #[test]
    fn crop_window_subpixel_survives_broken_offsets() {
        let canvas = ramp_canvas(8, 8);
        for &(ox, oy) in &[(f64::NAN, 0.0), (0.0, f64::INFINITY), (-3.0, -3.0)] {
            let img = crop_window_subpixel(&canvas, ox, oy, 4, 4);
            assert_eq!(img.dimensions(), (4, 4));
        }
        // 空の canvas でも窓サイズは守る。
        assert_eq!(crop_window_subpixel(&RgbImage::new(0, 0), 0.0, 0.0, 3, 2).dimensions(), (3, 2));
    }

    // build_window_nowait: subpixel=false は従来どおり。subpixel=true では中心の小数部が絵に効く。
    #[test]
    fn build_window_nowait_uses_the_fractional_centre_only_when_asked() {
        let z = 3u32;
        let (tx, ty) = (4i64, 4i64);
        let cache = Arc::new(Mutex::new(Cache::new()));
        // タイル内に横グラデーションを入れて、1px 未満のずれが画素値に出るようにする。
        cache.lock().unwrap().insert(
            TileKey { src: TileSource::Base("osm".to_string()), z, x: tx, y: ty },
            RgbaImage::from_fn(TILE, TILE, |x, _| image::Rgba([(x % 256) as u8, 0, 0, 255])),
        );
        let loader = TileLoader::start(Arc::clone(&cache));
        let cx = (tx as f64 + 0.5) * TILE as f64;
        let cy = (ty as f64 + 0.5) * TILE as f64;
        let base_int = build_window_nowait(cx, cy, z, 32, 32, "osm", false, &loader).unwrap();
        let moved_int = build_window_nowait(cx + 0.5, cy, z, 32, 32, "osm", false, &loader).unwrap();
        assert_eq!(base_int, moved_int, "整数切り出しでは 0.5px の移動は捨てられる");

        let base_sub = build_window_nowait(cx, cy, z, 32, 32, "osm", true, &loader).unwrap();
        let moved_sub = build_window_nowait(cx + 0.5, cy, z, 32, 32, "osm", true, &loader).unwrap();
        assert_eq!(base_sub, base_int, "小数部 0 なら両者は一致する");
        assert_ne!(base_sub, moved_sub, "サブピクセルでは 0.5px の移動が絵に出る");
        // 中間色になっている(隣の値との平均)。
        assert_eq!(moved_sub.get_pixel(0, 0).0[0], base_int.get_pixel(0, 0).0[0] + 1);
    }

    // 窓の右端がちょうどタイル境界に載るとき、バイリニアが参照する隣のタイルまで
    // 列を広げる(広げないとその1列だけ混ざらず、パン中に端が固まって見える)。
    #[test]
    fn build_window_nowait_widens_the_tile_range_for_the_bilinear_neighbour() {
        let z = 3u32;
        let (tx, ty) = (4i64, 4i64);
        let cache = Arc::new(Mutex::new(Cache::new()));
        {
            let mut c = cache.lock().unwrap();
            // 左のタイルは黒、右隣は白。境界をまたぐ位置で切り出す。
            c.insert(TileKey { src: TileSource::Base("osm".to_string()), z, x: tx, y: ty },
                     RgbaImage::from_pixel(TILE, TILE, image::Rgba([0, 0, 0, 255])));
            c.insert(TileKey { src: TileSource::Base("osm".to_string()), z, x: tx + 1, y: ty },
                     RgbaImage::from_pixel(TILE, TILE, image::Rgba([255, 255, 255, 255])));
        }
        let loader = TileLoader::start(Arc::clone(&cache));
        // 窓の右端(left + win_w)がちょうどタイル境界(tx+1 の先頭)に来るように置く。
        let win_w = 16u32;
        let left = (tx + 1) as f64 * TILE as f64 - win_w as f64;
        let cx = left + win_w as f64 / 2.0 + 0.5; // 右へ 0.5px ずらす
        let cy = (ty as f64 + 0.5) * TILE as f64;
        let img = build_window_nowait(cx, cy, z, win_w, 16, "osm", true, &loader).unwrap();
        // 一番右の列は、黒(0)と隣タイルの白(255)の中間になる。範囲を広げていないと 0 のまま。
        assert_eq!(img.get_pixel(win_w - 1, 0).0[0], 128);
    }

    // draw_loading_watermark: 描画後、ink色のピクセルが1つ以上存在する(=実際に何か描かれている)。
    #[test]
    fn draw_loading_watermark_marks_pixels() {
        let ink = image::Rgb([150u8, 150, 150]);
        let mut canvas = RgbImage::from_pixel(TILE, TILE, image::Rgb([200u8, 200, 200]));
        draw_loading_watermark(&mut canvas, 0, 0, ink);
        let count = canvas.pixels().filter(|p| **p == ink).count();
        assert!(count > 0, "no ink pixels were drawn");
    }
}
