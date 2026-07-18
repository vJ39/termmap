// タイル取得 (OSM/Carto) と表示窓の合成
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use image::RgbImage;
// 近傍優先の距離計算で、異なるズームのタイル中心を同一ズームへ再投影するために座標変換を使う。
use crate::geo::{TILE, pixel_to_deg, deg_to_pixel};

// タイルのディスクキャッシュ先: ~/.config/termmap/tiles/<style>/<z>/<x>/<y>.png
// 一度取得したタイルはここに残り、パン再訪・再起動でも再DLせず読み出す(通信最小化)。
fn tile_cache_path(style: &str, z: u32, x: i64, y: i64) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join(".config/termmap/tiles").join(style).join(z.to_string()).join(x.to_string()).join(format!("{y}.png")))
}

// キャッシュキーは style を含む(style違いのタイルが混ざらない。以前は clear() 頼みで危うかった)。
#[derive(Clone, PartialEq, Eq, Hash)]
struct TileKey { style: String, z: u32, x: i64, y: i64 }

// タイルキャッシュ。上限(cap)超過時は最終アクセスが最古のものから捨てる簡易LRU。
// 長時間パンし続けてもメモリが訪問範囲に比例して無制限に増えないようにする。
pub struct Cache { map: HashMap<TileKey, (RgbImage, u64)>, tick: u64, cap: usize }

impl Default for Cache { fn default() -> Self { Self::new() } }

impl Cache {
    pub fn new() -> Self { Cache { map: HashMap::new(), tick: 0, cap: 256 } }
    fn contains(&self, k: &TileKey) -> bool { self.map.contains_key(k) }
    fn get(&mut self, k: &TileKey) -> Option<&RgbImage> {
        self.tick += 1;
        let t = self.tick;
        match self.map.get_mut(k) { Some(e) => { e.1 = t; Some(&e.0) } None => None }
    }
    fn insert(&mut self, k: TileKey, img: RgbImage) {
        if self.map.len() >= self.cap && !self.map.contains_key(&k) {
            if let Some(old) = self.map.iter().min_by_key(|(_, (_, t))| *t).map(|(kk, _)| kk.clone()) {
                self.map.remove(&old); // 最古を1つ退避
            }
        }
        self.tick += 1;
        let t = self.tick;
        self.map.insert(k, (img, t));
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
// ディスクキャッシュの有効期限。タイル自体は滅多に変わらないが、無期限だと地図更新(新道路等)が
// 反映されないため30日で区切る。期限切れは「無かった」扱いにしてネットワークから取り直す。
const TILE_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);

// mtimeからの経過時間がTTL未満か(ネットワーク/ファイルI/Oを伴わない純粋関数)。
fn is_tile_fresh(age: std::time::Duration) -> bool { age < TILE_TTL }

pub fn fetch_tile(style: &str, z: u32, x: i64, y: i64) -> Result<RgbImage, String> {
    // 1) ディスクキャッシュを先に見る(在って、かつ30日以内ならネット無しで読む)
    let cache_path = tile_cache_path(style, z, x, y);
    if let Some(p) = &cache_path {
        let fresh = std::fs::metadata(p).ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.elapsed().ok())
            .map(is_tile_fresh)
            .unwrap_or(false); // mtime取得失敗時は期限切れ扱い(安全側)にして取り直す
        if fresh {
            if let Ok(buf) = std::fs::read(p) {
                if let Ok(img) = image::load_from_memory(&buf) { return Ok(img.to_rgb8()); }
                // 壊れたキャッシュは無視して取り直す
            }
        }
    }
    // 2) ネットワーク取得
    let url = tile_url(style, z, x, y);
    let resp = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .timeout(std::time::Duration::from_secs(20)).call().map_err(|e| format!("fetch tile {z}/{x}/{y}: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&buf).map_err(|e| format!("decode tile {z}/{x}/{y}: {e}"))?.to_rgb8();
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

    // 未キャッシュのタイルを列挙
    let mut missing: Vec<(i64, i64)> = Vec::new();
    for ty in ty_min..=ty_max {
        if ty < 0 || ty >= max_t { continue; }
        for tx in tx_min..=tx_max {
            let wx = ((tx % max_t) + max_t) % max_t;
            if !cache.contains(&TileKey { style: style.to_string(), z, x: wx, y: ty }) { missing.push((wx, ty)); }
        }
    }
    missing.sort_unstable();
    missing.dedup();
    // OpenTopoMapは個人利用で最大2req/秒程度を求める利用ポリシーがあり、他スタイルと同じ8並列で
    // 叩くとサーバー側スロットリングを誘発し1タイルあたり数秒かかることを実測で確認した。
    // topoのみ並列数を落とす(他スタイルのCDNは8並列でも問題ない)。
    let concurrency: usize = if style == "topo" { 2 } else { 8 };
    for chunk in missing.chunks(concurrency) {
        let got: Vec<((i64, i64), Result<RgbImage, String>)> = std::thread::scope(|s| {
            let hs: Vec<_> = chunk.iter().map(|&(wx, ty)| s.spawn(move || ((wx, ty), fetch_tile(style, z, wx, ty)))).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for ((wx, ty), r) in got { cache.insert(TileKey { style: style.to_string(), z, x: wx, y: ty }, r?); }
    }

    let cols = (tx_max - tx_min + 1) as u32;
    let rows = (ty_max - ty_min + 1) as u32;
    let bg = if style == "dark" { image::Rgb([26, 26, 26]) } else { image::Rgb([221, 221, 221]) };
    let mut canvas = RgbImage::from_pixel(cols * TILE, rows * TILE, bg);
    for ty in ty_min..=ty_max {
        if ty < 0 || ty >= max_t { continue; }
        for tx in tx_min..=tx_max {
            let wx = ((tx % max_t) + max_t) % max_t;
            if let Some(t) = cache.get(&TileKey { style: style.to_string(), z, x: wx, y: ty }) {
                let ox = (tx - tx_min) as u32 * TILE;
                let oy = (ty - ty_min) as u32 * TILE;
                for (px, py, p) in t.enumerate_pixels() { canvas.put_pixel(ox + px, oy + py, *p); }
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
// 取得依頼中のタイル集合。queued=未着手 / inflight=取得中。両方にもcacheにも無いものだけ新規登録して二重取得を防ぐ。
struct PendingSet { queued: HashSet<TileKey>, inflight: HashSet<TileKey> }

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
        let pending = Arc::new(Mutex::new(PendingSet { queued: HashSet::new(), inflight: HashSet::new() }));
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
    // 既に queued/inflight にあるものは弾く=二重リクエスト防止。
    fn request_tiles(&self, keys: Vec<TileKey>) {
        let mut p = self.pending.lock().unwrap();
        for k in keys {
            if !p.queued.contains(&k) && !p.inflight.contains(&k) { p.queued.insert(k); }
        }
    }

    // ルート確定時、その経路が通るタイルを先読み依頼として登録する(#34)。既存のrequest_tilesを
    // そのまま使う=画面表示中のタイルの方が常にview距離で優先されるため、専用の優先度階層は不要。
    pub fn request_route_tiles(&self, style: &str, z: u32, tile_coords: &[(i64, i64)]) {
        let keys = tile_coords.iter()
            .map(|&(x, y)| TileKey { style: style.to_string(), z, x, y })
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
                if let Ok(img) = fetch_tile(&k.style, k.z, k.x, k.y) {
                    shared.lock().unwrap().insert(k.clone(), img);
                    generation.fetch_add(1, Ordering::Relaxed); // 届いた→次フレームで再描画させる
                }
                // 成否に関わらず inflight から外す。失敗時は cache 未挿入のまま残り、次フレームでメインが
                // 再登録して自然にリトライされる。
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
fn find_fallback_tile(cache: &mut Cache, current_style: &str, z: u32, x: i64, y: i64) -> Option<RgbImage> {
    for &s in FALLBACK_STYLES.iter() {
        if s == current_style { continue; }
        let key = TileKey { style: s.to_string(), z, x, y };
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
pub fn build_window_nowait(cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32, style: &str, loader: &TileLoader) -> Result<RgbImage, String> {
    if style == "topo" && z > TOPO_MAX_Z {
        // 同期版と同じオーバーズーム。z17相当のサブ窓を非ブロッキングで組み(未取得はグレー)、拡大→中央クロップ。
        // z17タイルはローダーへ登録され順次埋まっていく。
        let shift = z - TOPO_MAX_Z;
        let scale = (1u32 << shift) as f64;
        let (base_w, base_h, scaled_w, scaled_h, crop_x, crop_y) = overzoom_geometry(win_w, win_h, shift);
        let base_img = build_window_nowait(cx / scale, cy / scale, TOPO_MAX_Z, base_w, base_h, style, loader)?;
        let resized = image::imageops::resize(&base_img, scaled_w, scaled_h, image::imageops::FilterType::Nearest);
        return Ok(image::imageops::crop_imm(&resized, crop_x, crop_y, win_w, win_h).to_image());
    }
    let tf = TILE as f64;
    let (tx_min, tx_max, ty_min, ty_max, left, top) = window_tile_range(cx, cy, win_w, win_h);
    let max_t = 2i64.pow(z);
    let cols = (tx_max - tx_min + 1) as u32;
    let rows = (ty_max - ty_min + 1) as u32;
    // 世界の端(範囲外タイル)は bg、範囲内で未取得のタイルは placeholder。bg(221 or 26)と見分けが付くグレー。
    let bg = if style == "dark" { image::Rgb([26, 26, 26]) } else { image::Rgb([221, 221, 221]) };
    let placeholder = image::Rgb([200u8, 200, 200]);
    // 透かしのink色は背景(グレー200 or 他style代用タイル)より少し暗いグレーにして薄く目立たせない。
    let watermark_ink = image::Rgb([150u8, 150, 150]);
    let mut canvas = RgbImage::from_pixel(cols * TILE, rows * TILE, bg);

    // cacheロックは1回だけ取り、範囲内タイルの描画/欠落判定をまとめて行いすぐ離す(1タイルごとの取り直しをしない)。
    let mut missing: Vec<TileKey> = Vec::new();
    {
        let mut cache = loader.shared.lock().unwrap();
        for ty in ty_min..=ty_max {
            if ty < 0 || ty >= max_t { continue; }
            for tx in tx_min..=tx_max {
                let wx = ((tx % max_t) + max_t) % max_t;
                let key = TileKey { style: style.to_string(), z, x: wx, y: ty };
                let ox = (tx - tx_min) as u32 * TILE;
                let oy = (ty - ty_min) as u32 * TILE;
                if let Some(t) = cache.get(&key) {
                    for (px, py, p) in t.enumerate_pixels() { canvas.put_pixel(ox + px, oy + py, *p); }
                } else {
                    // 未取得: 他styleの同一タイルがキャッシュにあれば仮表示に流用、無ければ薄いグレー。
                    // いずれの場合も本来のスタイルが未達であることが分かるよう LOADING 透かしを必ず重ねる。
                    if let Some(fb) = find_fallback_tile(&mut cache, style, z, wx, ty) {
                        for (px, py, p) in fb.enumerate_pixels() { canvas.put_pixel(ox + px, oy + py, *p); }
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

    let crop_x = (left - tx_min as f64 * tf).max(0.0) as u32;
    let crop_y = (top - ty_min as f64 * tf).max(0.0) as u32;
    Ok(image::imageops::crop_imm(&canvas, crop_x, crop_y, win_w, win_h).to_image())
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
        cache.insert(TileKey { style: "dark".to_string(), z, x, y }, RgbImage::from_pixel(TILE, TILE, image::Rgb([1, 2, 3])));
        assert!(find_fallback_tile(&mut cache, "dark", z, x, y).is_none());

        // (b) current_style="osm" なら他style "dark" のタイルがそのまま返る(色も維持)。
        let got = find_fallback_tile(&mut cache, "osm", z, x, y);
        assert!(got.is_some());
        assert_eq!(*got.unwrap().get_pixel(0, 0), image::Rgb([1, 2, 3]));

        // (c) 空キャッシュならどのstyleにも無く None。
        let mut empty = Cache::new();
        assert!(find_fallback_tile(&mut empty, "osm", z, x, y).is_none());
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
