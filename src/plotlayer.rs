// 地図に重ねるプロットデータ(道路交通量・主要道路・道路ライブカメラ・通行規制・過去災害・
// 市区町村境界・500mメッシュ人口)の取得段取り。ui.rs にほぼ同一の取得ブロックが4つ並んでいた
// ものを1つにまとめたもの。
// 設計は docs/plot-data-disk-cache-design.md §6.3/§7。
//
// 旧実装との違いは3点。
//   1. 取得単位が「視野中心±900pxのbbox」から「取得元が持つ自然な単位のセル」
//      (1次/2次メッシュ・地方整備局)に変わった。bboxを生でキーにすると1pxパンで別キーになり
//      ディスクキャッシュがヒットしないため。
//   2. 再取得の判定が「90秒経過 or 中心がbboxの外」から「そのセルが fresh TTL 以内か」に変わった。
//   3. ディスクの読み書きを全部ワーカースレッド側に置いた。UIスレッドは mpsc から受け取るだけで、
//      受信コードの形(try_recv + Disconnected で畳む)は旧実装と同じ。

use crate::plotcache::{self, Cached, Layer};
use crate::{camera, disaster, mesh, muni, population, regulation, roadsearch, traffic};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

/// (lat_min, lon_min, lat_max, lon_max)。geo.rs の pixel_to_deg と同じ緯度経度。
pub type Bbox = (f64, f64, f64, f64);

/// 視野bbox → 覆うセルキー列。
type CellsFn = fn(Bbox) -> Vec<String>;
/// セルキー → 取得結果。第2引数は1ジョブ内だけ生きるスクラッチ領域で、
/// 通行規制が配信元パスの発見(毎回変わるので保存できない)を1ジョブ1回で済ませるために使う。
/// 他のレイヤは触らない。
type FetchFn<T> = fn(&str, &mut Option<String>) -> Result<Vec<T>, String>;

// セル被覆を計算するときの視野の半径(px)。旧実装の MARGIN_PX と同じ値だが役割が違う。
// 旧実装では「中心1点しか見ない再取得判定」を埋めるための余白だったのに対し、ここでは
// 「覆うべき視野そのもの」の見積り(実際の地図領域は端末サイズによるが、セル1枚(2次で約10km・
// 1次で約80km)が視野より十分大きいので、余白は構造的に確保される)。
const VIEW_HALF_PX: f64 = 900.0;
// 1回の判定で必要なセルがこれを超えたら取得しない。セルに分割すると「1回のbbox取得」が
// 「N回のセル取得」に化けるため、広域では今より通信が増えてしまう。その安全弁。
const MAX_CELLS_PER_JOB: usize = 9;
// メモリ上に保持するセル数の既定の上限。視野外のセルもしばらく残して往復(行って戻る)を
// 速くするが、長距離を走り続けたときに無制限へ増えないよう、視野に入っていないものから
// 古い順に捨てる。1セルが小さいレイヤ(交通量・規制・カメラ・道路・過去災害)はこの値。
const MAX_CELLS_IN_MEMORY: usize = 32;
// 人口メッシュだけは1セル(=1都道府県)が北海道で3.6MBあるため、32セルだと最悪100MBを超える。
// レイヤごとの値にして4に下げる(4件で最悪14MB。設計 §6.4-2)。
const POPULATION_CELLS_IN_MEMORY: usize = 4;
// 取得に失敗した(または取得しても値が更新されなかった)セルを、次に試すまでの間隔。
// これが無いと圏外で毎フレーム新しいジョブを起こし続ける(旧実装は REFRESH=90秒がこの役をしていた)。
const RETRY_BACKOFF_SECS: u64 = 60;

/// 視野の中心(グローバル画素)とズームから、覆うべき緯度経度bboxを作る。
pub fn view_bbox(cx: f64, cy: f64, z: u32) -> Bbox {
    let (lat_max, lon_min) = crate::geo::pixel_to_deg(cx - VIEW_HALF_PX, cy - VIEW_HALF_PX, z);
    let (lat_min, lon_max) = crate::geo::pixel_to_deg(cx + VIEW_HALF_PX, cy + VIEW_HALF_PX, z);
    (lat_min, lon_min, lat_max, lon_max)
}

fn intersects(a: Bbox, b: Bbox) -> bool {
    a.0 <= b.2 && a.2 >= b.0 && a.1 <= b.3 && a.3 >= b.1
}

/// 表示範囲で絞り込むために、各アイテムが自分の占める矩形を答える。
/// 点(交通量・カメラ)は面積ゼロの矩形、線(規制・道路)はその外接矩形になる。
pub trait PlotItem {
    fn bounds(&self) -> Bbox;
}

fn line_bounds(pts: &[(f64, f64)]) -> Bbox {
    let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(lat, lon) in pts {
        b.0 = b.0.min(lat);
        b.1 = b.1.min(lon);
        b.2 = b.2.max(lat);
        b.3 = b.3.max(lon);
    }
    if pts.is_empty() {
        // 空の線は「どこにも無い」= どの視野とも交差しない矩形にする。
        return (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    }
    b
}

impl PlotItem for traffic::TrafficPoint {
    fn bounds(&self) -> Bbox {
        (self.lat, self.lon, self.lat, self.lon)
    }
}
impl PlotItem for camera::RoadCamera {
    fn bounds(&self) -> Bbox {
        (self.lat, self.lon, self.lat, self.lon)
    }
}
impl PlotItem for regulation::ClosureEvent {
    fn bounds(&self) -> Bbox {
        line_bounds(&self.line)
    }
}
impl PlotItem for disaster::DisasterSite {
    fn bounds(&self) -> Bbox {
        (self.lat, self.lon, self.lat, self.lon)
    }
}
impl PlotItem for muni::MuniArea {
    fn bounds(&self) -> Bbox {
        self.bbox // パース時に全リングから計算済み
    }
}
// 500mメッシュは全件が軸平行の矩形で、9桁のコードから完全に復元できる(幾何は保存しない)。
impl PlotItem for population::PopMesh {
    fn bounds(&self) -> Bbox {
        mesh::half_mesh_bbox(self.mesh)
    }
}

/// 主要道路1本ぶんの線形。roadsearch::fetch_major_roads の戻り値 (点列, oneway) に対応する。
/// タプルのままだとJSONが位置配列だけになって読み手に意味が分からないので、名前付きにする。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoadShape {
    pub pts: Vec<(f64, f64)>,
    pub oneway: bool,
}

impl PlotItem for RoadShape {
    fn bounds(&self) -> Bbox {
        line_bounds(&self.pts)
    }
}

// ワーカー → UIスレッドへ流すセル1件ぶんの結果。
enum CellMsg<T> {
    // これから通信して取りに行くセル。他レイヤは1セルが1秒未満なので使わないが、人口メッシュは
    // 1セルに数十秒かかるため「いま何を待っているのか」を画面に出せるようにする(設計 §6.4-4)。
    // ディスクの fresh 値で済んだセルでは送らない(通信していないので待たせていない)。
    Started(String),
    Loaded(String, Cached<T>),
    // 取得も出来ずディスクにも無かったセル。手元の値は消さずに次の機会を待つ。
    Failed(String),
}

pub struct PlotLayer<T> {
    layer: Layer,
    // このズームより広域では取得しない(既に持っているセルは表示し続ける)。
    min_zoom: u32,
    // データ自身の時刻を fetched_at からどれだけ遡らせるか。交通量だけ 25分×60秒 で、
    // 他は0(取得時刻がそのままデータの時刻)。
    data_lag_secs: u64,
    cells_for: CellsFn,
    fetch: FetchFn<T>,
    // メモリ上に保持するセル数の上限。1セルの重さがレイヤによって2桁違うのでレイヤごとに持つ。
    max_in_memory: usize,
    // 1回のジョブで取りに行けるセル数の上限。過去災害/境界だけ usize::MAX(実質無制限)にする
    // (設計 docs/disaster-choropleth-unlimited-zoom-design.md §3.2)。他レイヤは全て既定値。
    max_cells_per_job: usize,
    cells: HashMap<String, Cached<T>>,
    // キー → この時刻(epoch秒)までは再取得を試みない。
    retry_after: HashMap<String, u64>,
    job: Option<Receiver<CellMsg<T>>>,
    // いま通信して取りに行っているセルのキー(ステータス表示用)。ジョブが終わったら None。
    fetching: Option<String>,
    // 直近の tick で視野を覆うと判定したセル(ステータス表示の経過時間とメモリ退避の判定に使う)。
    view_keys: Vec<String>,
    // ズーム下限/セル数上限で取得を止めているか。
    suppressed: bool,
    // セル表が変わるたび増える。ui.rs の再描画判定シグネチャに混ぜて、新しく届いたデータが
    // 次のフレームで必ず描き直されるようにする。
    generation: u64,
}

impl<T: Serialize + serde::de::DeserializeOwned + PlotItem + Send + 'static> PlotLayer<T> {
    fn new(layer: Layer, min_zoom: u32, data_lag_secs: u64, cells_for: CellsFn, fetch: FetchFn<T>) -> Self {
        Self::new_with_cap(layer, min_zoom, data_lag_secs, MAX_CELLS_IN_MEMORY, cells_for, fetch)
    }

    // メモリ保持セル数を明示する版。1セルが重いレイヤ(人口メッシュ)だけが使う。
    fn new_with_cap(
        layer: Layer,
        min_zoom: u32,
        data_lag_secs: u64,
        max_in_memory: usize,
        cells_for: CellsFn,
        fetch: FetchFn<T>,
    ) -> Self {
        Self::new_full(layer, min_zoom, data_lag_secs, max_in_memory, MAX_CELLS_PER_JOB, cells_for, fetch)
    }

    // 1回のジョブで取りに行けるセル数の上限を持たない版。過去災害/境界が使う
    // (設計 docs/disaster-choropleth-unlimited-zoom-design.md §3.2)。cells_for が返す順序で
    // 中心から近い順に1個ずつ取得が進む(disaster_cellsのソートに依存する)。
    fn new_uncapped(layer: Layer, min_zoom: u32, data_lag_secs: u64, cells_for: CellsFn, fetch: FetchFn<T>) -> Self {
        Self::new_full(layer, min_zoom, data_lag_secs, MAX_CELLS_IN_MEMORY, usize::MAX, cells_for, fetch)
    }

    fn new_full(
        layer: Layer,
        min_zoom: u32,
        data_lag_secs: u64,
        max_in_memory: usize,
        max_cells_per_job: usize,
        cells_for: CellsFn,
        fetch: FetchFn<T>,
    ) -> Self {
        PlotLayer {
            layer,
            min_zoom,
            data_lag_secs,
            cells_for,
            fetch,
            max_in_memory,
            max_cells_per_job,
            cells: HashMap::new(),
            retry_after: HashMap::new(),
            job: None,
            fetching: None,
            view_keys: Vec::new(),
            suppressed: false,
            generation: 0,
        }
    }

    /// 1フレーム分の進行: 進行中ジョブの受信 → 必要セルの算出 → 不足ぶんのジョブ起動。
    /// `enabled` が false でも受信だけは行う(設定でOFFにした瞬間に走っていたジョブを畳むため)。
    /// 戻り値は「セル表が変わったか」で、true なら呼び出し側は即座に描き直す。
    pub fn tick(&mut self, cx: f64, cy: f64, z: u32, enabled: bool) -> bool {
        let now = plotcache::now_secs();
        let mut changed = false;
        if let Some(rx) = &self.job {
            loop {
                match rx.try_recv() {
                    Ok(CellMsg::Started(key)) => {
                        self.fetching = Some(key);
                    }
                    Ok(CellMsg::Loaded(key, cached)) => {
                        // 受け取った値がまだ stale なら(取得に失敗して手元のディスク値が返った
                        // ケース)、すぐ next tick で同じジョブを起こさないよう間隔を空ける。
                        if cached.is_fresh(self.layer, now) {
                            self.retry_after.remove(&key);
                        } else {
                            self.retry_after.insert(key.clone(), now + RETRY_BACKOFF_SECS);
                        }
                        self.cells.insert(key, cached);
                        self.generation += 1;
                        changed = true;
                    }
                    Ok(CellMsg::Failed(key)) => {
                        // 失敗しても手元の値(self.cells)は触らない。空で上書きすると、
                        // トンネル1本で直前まで見えていた通行止めが消える。
                        self.retry_after.insert(key, now + RETRY_BACKOFF_SECS);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.job = None;
                        self.fetching = None;
                        break;
                    }
                }
            }
        }
        if !enabled || z < self.min_zoom {
            if enabled {
                self.suppressed = true;
            }
            return changed;
        }
        let keys = (self.cells_for)(view_bbox(cx, cy, z));
        if keys.len() > self.max_cells_per_job {
            self.suppressed = true;
            return changed;
        }
        self.suppressed = false;
        if keys.is_empty() {
            return changed; // 日本のメッシュ空間の外(取得元にデータが無い)
        }
        self.view_keys = keys.clone();
        self.evict(&keys);
        if self.job.is_some() {
            return changed;
        }
        let missing: Vec<String> = keys
            .into_iter()
            .filter(|k| {
                if self.retry_after.get(k).is_some_and(|t| now < *t) {
                    return false;
                }
                self.cells.get(k).is_none_or(|c| !c.is_fresh(self.layer, now))
            })
            .collect();
        if missing.is_empty() {
            return changed;
        }
        self.job = Some(spawn_job(self.layer, missing, self.fetch, self.data_lag_secs));
        changed
    }

    // 視野に入っていないセルを古い順に捨てて上限内に収める。
    fn evict(&mut self, keep: &[String]) {
        if self.cells.len() <= self.max_in_memory {
            return;
        }
        let excess = self.cells.len() - self.max_in_memory;
        let mut victims: Vec<(String, u64)> = self
            .cells
            .iter()
            .filter(|(k, _)| !keep.contains(k))
            .map(|(k, c)| (k.clone(), c.fetched_at))
            .collect();
        victims.sort_by_key(|(_, t)| *t);
        for (k, _) in victims.into_iter().take(excess) {
            self.cells.remove(&k);
            self.retry_after.remove(&k);
        }
    }

    /// 表示範囲に掛かるアイテムだけを返す。カメラは整備局まるごと(数百台)をキャッシュするので、
    /// bboxでの絞り込みはここ(メモリ上)で行う。
    pub fn items(&self, view: Bbox) -> Vec<&T> {
        self.cells
            .values()
            .flat_map(|c| c.items.iter())
            .filter(|it| intersects(it.bounds(), view))
            .collect()
    }

    /// fresh を過ぎた値を表示しているときだけ、その経過秒(視野内で最も古いもの)を返す。
    /// 交通量は data_at 基準なので「観測からの経過」になる。
    pub fn stale_age_secs(&self, now: u64) -> Option<u64> {
        let mut worst: Option<u64> = None;
        for k in &self.view_keys {
            if let Some(c) = self.cells.get(k) {
                if !c.is_fresh(self.layer, now) {
                    let a = c.age_secs(now);
                    worst = Some(worst.map_or(a, |w: u64| w.max(a)));
                }
            }
        }
        worst
    }

    pub fn job_active(&self) -> bool {
        self.job.is_some()
    }

    /// いま通信して取りに行っているセルのキー。1セルに数十秒かかる人口メッシュで
    /// 「北海道を取得中…」と出すために使う(設計 §6.4-4/§7.6)。
    pub fn fetching_key(&self) -> Option<&str> {
        if self.job.is_some() {
            self.fetching.as_deref()
        } else {
            None
        }
    }

    /// ズーム下限/セル数上限で取得を止めているか(ステータスの「広域では非表示」用)。
    pub fn suppressed(&self) -> bool {
        self.suppressed
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

// 1本のジョブ。不足セルを直列に回し、1セル取れるたびに送る(部分的に描画が進む)。
// Overpass 等へ並列には投げない。
fn spawn_job<T: Serialize + serde::de::DeserializeOwned + Send + 'static>(
    layer: Layer,
    keys: Vec<String>,
    fetch: FetchFn<T>,
    data_lag_secs: u64,
) -> Receiver<CellMsg<T>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let root = plotcache::cache_root();
        let mut scratch: Option<String> = None;
        for key in keys {
            let now = plotcache::now_secs();
            let disk = root.as_ref().and_then(|r| plotcache::load::<T>(r, layer, &key, now));
            if disk.as_ref().is_some_and(|c| c.is_fresh(layer, now)) {
                // fresh なディスク値がある = 通信しない。
                if tx.send(CellMsg::Loaded(key, disk.expect("checked above"))).is_err() {
                    return;
                }
                continue;
            }
            // ここから先は通信する。何を待たせているのかを先に知らせる。
            if tx.send(CellMsg::Started(key.clone())).is_err() {
                return;
            }
            let msg = match fetch(&key, &mut scratch) {
                Ok(items) => {
                    let fetched_at = plotcache::now_secs();
                    let data_at = fetched_at.saturating_sub(data_lag_secs);
                    if let Some(r) = &root {
                        // 保存はベストエフォート(失敗しても次回また取りに行くだけ)。
                        let _ = plotcache::store(r, layer, &key, &items, fetched_at, data_at);
                    }
                    CellMsg::Loaded(key, Cached { items, fetched_at, data_at })
                }
                // 取得できなくても、stale上限内のディスク値があればそれを出す(オフライン継続表示)。
                Err(_) => match disk {
                    Some(cached) => CellMsg::Loaded(key, cached),
                    None => CellMsg::Failed(key),
                },
            };
            if tx.send(msg).is_err() {
                return; // 受信側が消えた(レイヤOFF・終了)
            }
        }
    });
    rx
}

// ---- セル被覆(視野bbox → セルキー) ----

fn primary_cells(b: Bbox) -> Vec<String> {
    mesh::primary_codes(b.0, b.1, b.2, b.3).iter().map(u32::to_string).collect()
}

fn secondary_cells(b: Bbox) -> Vec<String> {
    mesh::secondary_codes(b.0, b.1, b.2, b.3).iter().map(u32::to_string).collect()
}

// カメラは取得元が地方整備局ごとにページを持つので、視野中心の最寄り局1つだけを見る。
// 管轄境界のポリゴンを持っていないため、境界付近のカメラを取りこぼす制約は従来と同じ。
fn bureau_cells(b: Bbox) -> Vec<String> {
    vec![camera::nearest_bureau((b.0 + b.2) / 2.0, (b.1 + b.3) / 2.0).to_string()]
}

// 過去災害の年代しきい値(この年以降の事例だけを数える)。取得元が年をグループ化キーに
// 入れさせてくれず where で絞るしかないため、しきい値が変わるとセルの中身も変わる。
// CellsFn/FetchFn は fn ポインタ(環境を捕まえられない)なので、被覆側と取得側の両方から
// 同じ値を読むにはプロセス全体の値として置くしかない。
static DISASTER_SINCE: AtomicI32 = AtomicI32::new(disaster::DEFAULT_SINCE_YEAR);

pub fn disaster_since() -> i32 {
    DISASTER_SINCE.load(Ordering::Relaxed)
}

/// しきい値年を変える。**変えたらレイヤを作り直すこと**(古いキーのセルがセル表に残り、
/// items() が全セルを舐めるため、作り直さないと別の年代のデータが混ざる)。
/// 現状は設定に出していないので呼び出し元は無い(#75 Stage2 で設定行を足すときに使う)。
#[allow(dead_code)]
pub fn set_disaster_since(year: i32) {
    DISASTER_SINCE.store(year.max(0), Ordering::Relaxed);
}

// 市区町村境界のセルキーの接頭辞。plotcache::valid_key(英数字と - _ の16文字以内)に収まる短さにする
// ("c20s0"〜"c20s9" で5文字)。
const BOUNDARY_KEY_PREFIX: &str = "c20s";

// 市区町村境界は取得元(気象庁 class20s)が全国を10ファイルに分けているので、そのファイル単位を
// セルにする。視野bboxと各ファイルの外接矩形(muni::RELM)が交差するものを全部返す。
fn boundary_cells(b: Bbox) -> Vec<String> {
    muni::relm_indices(b).iter().map(|i| format!("{BOUNDARY_KEY_PREFIX}{i}")).collect()
}

// 過去災害は交通量・規制と同じ1次メッシュだが、キーに年代しきい値を足した複合キーにする
// (例 "5339_1926"、全期間は "5339_0")。しきい値を切り替えても別ファイルになって混ざらない。
// disaster/boundaryはMAX_CELLS_PER_JOBの上限を持たない(new_uncapped、設計
// docs/disaster-choropleth-unlimited-zoom-design.md §3.2)ため、広域で1次メッシュが
// 何十枚要っても取得を止めない。その代わり中心から近い順に並べて返し、spawn_jobが
// 1個ずつ順に取っていく間、常に「今見ている場所の近く」から埋まるようにする(同 §3.3)。
fn disaster_cells(b: Bbox) -> Vec<String> {
    let since = disaster_since().max(0);
    let mut codes = mesh::primary_codes(b.0, b.1, b.2, b.3);
    sort_codes_by_distance_from_center(&mut codes, mesh::primary_bbox, b);
    codes.iter().map(|c| format!("{c}_{since}")).collect()
}

// codesを、bの中心に近いセルの中心が先頭になるよう並べ替える(度単位の平面近似で十分。
// 優先度付けが目的で、厳密な距離は要らない)。
fn sort_codes_by_distance_from_center(codes: &mut [u32], bbox_of: fn(u32) -> Bbox, view: Bbox) {
    let center = ((view.0 + view.2) / 2.0, (view.1 + view.3) / 2.0);
    codes.sort_by(|&a, &b| {
        let d = |code: u32| {
            let (s, w, n, e) = bbox_of(code);
            let c = ((s + n) / 2.0, (w + e) / 2.0);
            (c.0 - center.0).powi(2) + (c.1 - center.1).powi(2)
        };
        d(a).partial_cmp(&d(b)).unwrap_or(std::cmp::Ordering::Equal)
    });
}

// 人口メッシュのセルは都道府県まるごと("01"〜"47")。取得元が都道府県単位でしかファイルを
// 分けていないので、それに合わせる(取得元が持つ自然な単位に揃えるという設計方針どおり)。
// 都道府県の判定に外接矩形は使えない(東京都は小笠原まで含み沖縄と同じ緯度帯を覆う)ため、
// 2次メッシュ→都道府県の索引を引く(population::prefectures_for)。
fn population_cells(b: Bbox) -> Vec<String> {
    population::prefectures_for(b).iter().map(|p| format!("{p:02}")).collect()
}

// "5339_1926" → (5339, 1926)。
fn split_disaster_key(key: &str) -> Result<(u32, i32), String> {
    let bad = || format!("不正なセルキー: {key}");
    let (code, since) = key.split_once('_').ok_or_else(bad)?;
    let code = code.parse::<u32>().map_err(|_| bad())?;
    let since = since.parse::<i32>().map_err(|_| bad())?;
    Ok((code, since))
}

// ---- セル取得(セルキー → データ) ----

fn parse_code(key: &str) -> Result<u32, String> {
    key.parse::<u32>().map_err(|_| format!("不正なセルキー: {key}"))
}

fn fetch_traffic_cell(key: &str, _scratch: &mut Option<String>) -> Result<Vec<traffic::TrafficPoint>, String> {
    let (s, w, n, e) = mesh::shrink(mesh::primary_bbox(parse_code(key)?));
    traffic::fetch_traffic(s, w, n, e)
}

fn fetch_roads_cell(key: &str, _scratch: &mut Option<String>) -> Result<Vec<RoadShape>, String> {
    let (s, w, n, e) = mesh::shrink(mesh::secondary_bbox(parse_code(key)?));
    let frags = roadsearch::fetch_major_roads(s, w, n, e)?;
    Ok(frags
        .into_iter()
        .map(|(pts, oneway)| RoadShape {
            // 座標は小数6桁(約0.1m)へ丸める。z16の1pxが約2.4mなので描画には影響せず、
            // 保存サイズだけが小さくなる。
            pts: pts.into_iter().map(|(lat, lon)| (round6(lat), round6(lon))).collect(),
            oneway,
        })
        .collect())
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

fn fetch_camera_cell(key: &str, _scratch: &mut Option<String>) -> Result<Vec<camera::RoadCamera>, String> {
    camera::fetch_bureau(parse_code(key)?)
}

fn fetch_regulation_cell(key: &str, scratch: &mut Option<String>) -> Result<Vec<regulation::ClosureEvent>, String> {
    let mesh_code = parse_code(key)?;
    if scratch.is_none() {
        // 配信元パスは更新のたびに変わるので永続化しない。1ジョブに1回だけ発見して使い回す。
        *scratch = Some(regulation::discover_json_base()?);
    }
    let base = scratch.as_deref().unwrap_or_default();
    regulation::fetch_mesh(base, mesh_code)
}

fn fetch_population_cell(key: &str, _scratch: &mut Option<String>) -> Result<Vec<population::PopMesh>, String> {
    let pref = key.parse::<u8>().map_err(|_| format!("不正なセルキー: {key}"))?;
    population::fetch_prefecture(pref)
}

// shrink は必須。外すと同じ代表点が隣り合う広域セルの両方に入り、choropleth::tally が
// コード単位で件数を足して2倍の市区町村ができる(設計 §2.7)。
fn fetch_disaster_cell(key: &str, _scratch: &mut Option<String>) -> Result<Vec<disaster::DisasterSite>, String> {
    let (code, since) = split_disaster_key(key)?;
    let (s, w, n, e) = mesh::shrink(mesh::primary_bbox(code));
    disaster::fetch_sites(s, w, n, e, since)
}

fn fetch_boundary_cell(key: &str, _scratch: &mut Option<String>) -> Result<Vec<muni::MuniArea>, String> {
    let index = key
        .strip_prefix(BOUNDARY_KEY_PREFIX)
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(|| format!("不正なセルキー: {key}"))?;
    muni::fetch_relm(index)
}

// ---- 7レイヤの組み立て ----

/// 道路交通量(JARTIC)。1次メッシュ単位・z11未満では取得しない。
pub fn traffic() -> PlotLayer<traffic::TrafficPoint> {
    PlotLayer::new(
        Layer::Traffic,
        11,
        traffic::OBSERVE_LAG_MIN as u64 * 60,
        primary_cells,
        fetch_traffic_cell,
    )
}

/// 主要道路(OSM trunk/primary)。2次メッシュ単位・z14未満では取得しない。
/// 1次メッシュだと都心の幾何が重すぎて Overpass の [timeout:25] に収まらない恐れがあるため。
pub fn roads() -> PlotLayer<RoadShape> {
    PlotLayer::new(Layer::Roads, 14, 0, secondary_cells, fetch_roads_cell)
}

/// 道路ライブカメラ。地方整備局単位。局は全国で10しかなく1回のレスポンスで足りるので
/// ズーム下限は設けない。
pub fn camera() -> PlotLayer<camera::RoadCamera> {
    PlotLayer::new(Layer::Camera, 0, 0, bureau_cells, fetch_camera_cell)
}

/// 通行規制。取得元が1次メッシュ単位でファイルを分けているのでそれに合わせる。z11未満では取得しない。
pub fn regulation() -> PlotLayer<regulation::ClosureEvent> {
    PlotLayer::new(Layer::Regulation, 11, 0, primary_cells, fetch_regulation_cell)
}

/// 過去災害の発生履歴(NIED 災害事例データベース)。広域セル(1次メッシュ4×4束)+年代しきい値の
/// 複合キー単位。取得元が広いbboxの集計を1リクエストで返すので、他レイヤより粗い刻みにしてある
/// (設計 §2.2)。z11以上でも最大4枚・典型1枚で済み、パンによる再取得も減る。
pub fn disaster() -> PlotLayer<disaster::DisasterSite> {
    PlotLayer::new_uncapped(Layer::Disaster, 9, 0, disaster_cells, fetch_disaster_cell)
}

/// 市区町村境界(気象庁 class20s)。過去災害の塗り(コロプレス)にだけ使うので、ズーム下限は
/// 過去災害と揃える(片方だけ取れていて塗れない、という状態を作らない)。
/// 領域は全国で10しかなく、視野に掛かるのは z11 で1〜2枚・z9 でも最大5枚(設計 §0.2)。
pub fn boundary() -> PlotLayer<muni::MuniArea> {
    PlotLayer::new_uncapped(Layer::Boundary, 9, 0, boundary_cells, fetch_boundary_cell)
}

/// 500mメッシュ別推計人口(国土数値情報)。都道府県単位・z11未満では取得しない。
/// z11 は交通量・通行規制・過去災害と同じ値で、複数レイヤを同時にONにしたときに
/// 「このレイヤだけ端が欠ける」状態にならないようにしてある。z10ではメッシュ1枚が
/// braille の4×4ドットまで縮み、ディザで間引いた時点で隣の階級と区別できない(設計 §6.3)。
/// メモリ保持は4県まで(1県が最大3.6MBあるため他レイヤの32とは別枠)。
pub fn population() -> PlotLayer<population::PopMesh> {
    PlotLayer::new_with_cap(
        Layer::Population,
        11,
        0,
        POPULATION_CELLS_IN_MEMORY,
        population_cells,
        fetch_population_cell,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::deg_to_pixel;
    use std::sync::Mutex;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct TestItem {
        lat: f64,
        lon: f64,
    }
    impl PlotItem for TestItem {
        fn bounds(&self) -> Bbox {
            (self.lat, self.lon, self.lat, self.lon)
        }
    }

    // fn ポインタは環境を捕まえられないので、テスト用の取得関数の振る舞いはこの静的変数で操る。
    // 同時に走ると混ざるため、この変数を使うテストは TEST_LOCK で直列化する。
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static FAIL_KEYS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static FETCHED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn test_fetch(key: &str, _scratch: &mut Option<String>) -> Result<Vec<TestItem>, String> {
        FETCHED.lock().unwrap().push(key.to_string());
        if FAIL_KEYS.lock().unwrap().iter().any(|k| k == key) {
            return Err(format!("テストの取得失敗: {key}"));
        }
        // キーごとに1件、識別できる座標を返す(テストの視野中心=東京駅付近のすぐ近く)。
        let n: f64 = key.parse().unwrap_or(0.0);
        Ok(vec![test_item(n)])
    }

    // テストの視野中心(35.68,139.77)のすぐ近くに置く。どのズームでも視野に入る距離。
    fn test_item(n: f64) -> TestItem {
        TestItem { lat: 35.68 + n / 10000.0, lon: 139.77 + n / 10000.0 }
    }

    fn three_cells(_b: Bbox) -> Vec<String> {
        vec!["1".to_string(), "2".to_string(), "3".to_string()]
    }
    fn ten_cells(_b: Bbox) -> Vec<String> {
        (1..=10).map(|i| i.to_string()).collect()
    }

    // テスト中はディスクキャッシュを一時ディレクトリへ逃がして実 $HOME を汚さない。
    struct TestEnv {
        _guard: std::sync::MutexGuard<'static, ()>,
        root: std::path::PathBuf,
    }
    impl TestEnv {
        fn new(tag: &str) -> Self {
            let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = std::env::temp_dir().join(format!("termmap_plotlayer_{}_{}", std::process::id(), tag));
            let _ = std::fs::remove_dir_all(&root);
            std::env::set_var("TERMMAP_PLOT_CACHE_DIR", &root);
            FAIL_KEYS.lock().unwrap().clear();
            FETCHED.lock().unwrap().clear();
            TestEnv { _guard: guard, root }
        }
    }
    impl Drop for TestEnv {
        fn drop(&mut self) {
            // 環境変数は**戻さない**。ワーカースレッドは自分が起きたタイミングで cache_root() を
            // 読むため、ここで消すと「テストは終わったがまだ生きているワーカー」が実 $HOME 側へ
            // 書いてしまう(実際に ~/.config/termmap/plot-cache へテストデータが漏れた)。
            // 次の TestEnv が上書きするので、残しておいても他のテストには影響しない。
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn tokyo_center(z: u32) -> (f64, f64) {
        deg_to_pixel(35.68, 139.77, z)
    }

    // ジョブが終わる(Disconnected を受け取る)まで tick を回す。
    fn run_to_idle<T: Serialize + serde::de::DeserializeOwned + PlotItem + Send + 'static>(
        layer: &mut PlotLayer<T>,
        z: u32,
    ) {
        let (cx, cy) = tokyo_center(z);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            layer.tick(cx, cy, z, true);
            if !layer.job_active() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("ジョブが終わらない");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn test_layer(cells_for: CellsFn, min_zoom: u32) -> PlotLayer<TestItem> {
        PlotLayer::new(Layer::Traffic, min_zoom, 0, cells_for, test_fetch)
    }

    fn test_layer_capped(cells_for: CellsFn, cap: usize) -> PlotLayer<TestItem> {
        PlotLayer::new_with_cap(Layer::Traffic, 0, 0, cap, cells_for, test_fetch)
    }

    fn test_layer_uncapped(cells_for: CellsFn, min_zoom: u32) -> PlotLayer<TestItem> {
        PlotLayer::new_uncapped(Layer::Traffic, min_zoom, 0, cells_for, test_fetch)
    }

    #[test]
    fn view_bbox_is_ordered_south_west_north_east() {
        let (cx, cy) = tokyo_center(14);
        let (s, w, n, e) = view_bbox(cx, cy, 14);
        assert!(s < n && w < e, "({s},{w},{n},{e})");
        assert!(s < 35.68 && 35.68 < n);
        assert!(w < 139.77 && 139.77 < e);
    }

    #[test]
    fn view_bbox_shrinks_as_the_zoom_gets_deeper() {
        let (cx14, cy14) = tokyo_center(14);
        let b14 = view_bbox(cx14, cy14, 14);
        let (cx16, cy16) = tokyo_center(16);
        let b16 = view_bbox(cx16, cy16, 16);
        assert!((b16.2 - b16.0) < (b14.2 - b14.0));
    }

    #[test]
    fn traffic_and_regulation_share_the_primary_mesh_cells() {
        // 交通量と規制は同じ被覆関数を使うので、両方ONのときにセル境界が揃う。
        let (cx, cy) = tokyo_center(12);
        let cells = primary_cells(view_bbox(cx, cy, 12));
        assert!(cells.contains(&"5339".to_string()), "{cells:?}");
        assert!(cells.len() <= 4, "z12(約56km)は1〜4枚のはず: {cells:?}");
    }

    #[test]
    fn primary_cells_stay_within_the_job_cap_at_the_zoom_floor() {
        // 交通量/規制の下限 z11 で、1次メッシュが MAX_CELLS_PER_JOB を超えないこと。
        for (lat, lon) in [(35.68, 139.77), (43.06, 141.35), (26.21, 127.68)] {
            let (cx, cy) = deg_to_pixel(lat, lon, 11);
            let n = primary_cells(view_bbox(cx, cy, 11)).len();
            assert!(n <= MAX_CELLS_PER_JOB, "z11 {lat},{lon} で {n} セル");
        }
    }

    #[test]
    fn secondary_cells_stay_within_the_job_cap_at_the_roads_zoom_floor() {
        for (lat, lon) in [(35.68, 139.77), (43.06, 141.35), (34.69, 135.52)] {
            let (cx, cy) = deg_to_pixel(lat, lon, 14);
            let n = secondary_cells(view_bbox(cx, cy, 14)).len();
            assert!(n <= MAX_CELLS_PER_JOB, "z14 {lat},{lon} で {n} セル");
        }
    }

    #[test]
    fn bureau_cells_pick_exactly_one_office_for_the_view_centre() {
        let (cx, cy) = tokyo_center(14);
        assert_eq!(bureau_cells(view_bbox(cx, cy, 14)), vec!["83".to_string()]); // 関東地方整備局
        let (cx, cy) = deg_to_pixel(43.06, 141.35, 14);
        assert_eq!(bureau_cells(view_bbox(cx, cy, 14)), vec!["81".to_string()]); // 北海道開発局
    }

    #[test]
    fn cells_outside_japan_produce_no_keys() {
        let (cx, cy) = deg_to_pixel(48.85, 2.35, 12); // パリ
        assert!(primary_cells(view_bbox(cx, cy, 12)).is_empty());
    }

    #[test]
    fn a_fetched_cell_lands_in_the_view_and_hits_the_disk() {
        let env = TestEnv::new("fetch");
        let mut l = test_layer(three_cells, 0);
        run_to_idle(&mut l, 14);
        let (cx, cy) = tokyo_center(14);
        assert_eq!(l.items(view_bbox(cx, cy, 14)).len(), 3);
        assert_eq!(FETCHED.lock().unwrap().len(), 3, "3セルとも取得した");
        for k in ["1", "2", "3"] {
            assert!(env.root.join("v1/traffic").join(format!("{k}.json")).is_file(), "{k} が保存されていない");
        }
    }

    #[test]
    fn a_second_pass_reads_the_disk_instead_of_fetching_again() {
        let _env = TestEnv::new("fresh");
        let mut l = test_layer(three_cells, 0);
        run_to_idle(&mut l, 14);
        assert_eq!(FETCHED.lock().unwrap().len(), 3);
        // 別インスタンス(=再起動相当)。fresh TTL 内なのでディスクから読むだけで通信しない。
        let mut l2 = test_layer(three_cells, 0);
        run_to_idle(&mut l2, 14);
        assert_eq!(FETCHED.lock().unwrap().len(), 3, "2周目で取得関数が呼ばれてはいけない");
        let (cx, cy) = tokyo_center(14);
        assert_eq!(l2.items(view_bbox(cx, cy, 14)).len(), 3, "ディスクから復元されている");
    }

    #[test]
    fn a_failing_cell_never_wipes_the_value_already_on_screen() {
        let _env = TestEnv::new("keepold");
        let mut l = test_layer(three_cells, 0);
        run_to_idle(&mut l, 14);
        let (cx, cy) = tokyo_center(14);
        assert_eq!(l.items(view_bbox(cx, cy, 14)).len(), 3);

        // 手元の値を期限切れにし、ディスクの控えも消したうえで全セルの取得を失敗させる。
        // (圏外に入った直後の状態)
        for c in l.cells.values_mut() {
            c.fetched_at = 0;
        }
        l.retry_after.clear();
        let _ = std::fs::remove_dir_all(_env.root.join("v1"));
        *FAIL_KEYS.lock().unwrap() = vec!["1".into(), "2".into(), "3".into()];
        run_to_idle(&mut l, 14);

        assert_eq!(l.items(view_bbox(cx, cy, 14)).len(), 3, "失敗で消えてはいけない");
    }

    #[test]
    fn a_partial_failure_applies_only_the_cells_that_succeeded() {
        let _env = TestEnv::new("partial");
        *FAIL_KEYS.lock().unwrap() = vec!["2".into()];
        let mut l = test_layer(three_cells, 0);
        run_to_idle(&mut l, 14);
        let (cx, cy) = tokyo_center(14);
        assert_eq!(l.items(view_bbox(cx, cy, 14)).len(), 2, "3セル中2セルだけ入る");
        assert!(l.cells.contains_key("1") && l.cells.contains_key("3"));
        assert!(!l.cells.contains_key("2"));
    }

    #[test]
    fn a_stale_disk_entry_is_shown_when_the_network_is_down() {
        let env = TestEnv::new("staleshow");
        // 55分前に取得した交通量(fresh 5分は過ぎているが stale上限 60分の内側)。
        let now = plotcache::now_secs();
        plotcache::store(&env.root, Layer::Traffic, "1", &[test_item(1.0)], now - 55 * 60, now - 55 * 60).unwrap();
        *FAIL_KEYS.lock().unwrap() = vec!["1".into(), "2".into(), "3".into()];
        let mut l = test_layer(three_cells, 0);
        run_to_idle(&mut l, 14);
        let (cx, cy) = tokyo_center(14);
        assert_eq!(l.items(view_bbox(cx, cy, 14)).len(), 1, "staleでも出す");
        let age = l.stale_age_secs(plotcache::now_secs()).expect("stale なら経過時間が出る");
        assert!((3200..3400).contains(&age), "経過時間が55分前後でない: {age}");
    }

    #[test]
    fn a_fresh_cell_reports_no_age() {
        let _env = TestEnv::new("noage");
        let mut l = test_layer(three_cells, 0);
        run_to_idle(&mut l, 14);
        assert_eq!(l.stale_age_secs(plotcache::now_secs()), None);
    }

    #[test]
    fn nothing_is_fetched_below_the_zoom_floor() {
        let _env = TestEnv::new("minzoom");
        let mut l = test_layer(three_cells, 11);
        let (cx, cy) = deg_to_pixel(35.68, 139.77, 10);
        l.tick(cx, cy, 10, true);
        assert!(!l.job_active(), "z10ではジョブを起こさない");
        assert!(l.suppressed());
        assert!(FETCHED.lock().unwrap().is_empty());
        // 下限以上なら取りに行く。
        run_to_idle(&mut l, 11);
        assert!(!l.suppressed());
        assert_eq!(FETCHED.lock().unwrap().len(), 3);
    }

    #[test]
    fn nothing_is_fetched_when_the_view_needs_too_many_cells() {
        let _env = TestEnv::new("maxcells");
        let mut l = test_layer(ten_cells, 0);
        let (cx, cy) = tokyo_center(14);
        l.tick(cx, cy, 14, true);
        assert!(!l.job_active(), "10セル必要なら取得しない");
        assert!(l.suppressed());
        assert!(FETCHED.lock().unwrap().is_empty());
    }

    #[test]
    fn a_disabled_layer_does_not_fetch_but_still_drains_its_job() {
        let _env = TestEnv::new("disabled");
        let mut l = test_layer(three_cells, 0);
        let (cx, cy) = tokyo_center(14);
        l.tick(cx, cy, 14, false);
        assert!(!l.job_active());
        assert!(FETCHED.lock().unwrap().is_empty());
        // ONにして走らせたジョブは、その後OFFにしても畳まれる(受信は続く)。
        l.tick(cx, cy, 14, true);
        assert!(l.job_active());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while l.job_active() {
            l.tick(cx, cy, 14, false);
            assert!(std::time::Instant::now() < deadline, "OFFのままジョブが畳まれない");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn a_failed_cell_is_not_retried_immediately() {
        let _env = TestEnv::new("backoff");
        *FAIL_KEYS.lock().unwrap() = vec!["1".into(), "2".into(), "3".into()];
        let mut l = test_layer(three_cells, 0);
        run_to_idle(&mut l, 14);
        assert_eq!(FETCHED.lock().unwrap().len(), 3);
        // 直後に何度 tick しても新しいジョブは起きない(圏外で叩き続けない)。
        let (cx, cy) = tokyo_center(14);
        for _ in 0..50 {
            l.tick(cx, cy, 14, true);
        }
        assert!(!l.job_active());
        assert_eq!(FETCHED.lock().unwrap().len(), 3, "バックオフ中に再取得してはいけない");
    }

    #[test]
    fn only_one_job_runs_per_layer_at_a_time() {
        let _env = TestEnv::new("onejob");
        let mut l = test_layer(three_cells, 0);
        let (cx, cy) = tokyo_center(14);
        l.tick(cx, cy, 14, true);
        assert!(l.job_active());
        for _ in 0..20 {
            l.tick(cx, cy, 14, true);
        }
        // 追加のジョブが立っていなければ、取得は3セルぶんで止まる。
        run_to_idle(&mut l, 14);
        assert_eq!(FETCHED.lock().unwrap().len(), 3);
    }

    #[test]
    fn items_are_filtered_by_the_view_rectangle() {
        let _env = TestEnv::new("filter");
        let mut l = test_layer(three_cells, 0);
        run_to_idle(&mut l, 14);
        // アイテムは 35.6801/35.6802/35.6803 にある。1件だけ入る細い矩形で絞る。
        let narrow = (35.68005, 139.77005, 35.68015, 139.77015);
        assert_eq!(l.items(narrow).len(), 1);
        // どれも入らない矩形。
        assert!(l.items((10.0, 100.0, 11.0, 101.0)).is_empty());
    }

    #[test]
    fn line_items_are_kept_when_only_a_part_of_the_line_is_visible() {
        let shape = RoadShape { pts: vec![(35.0, 139.0), (36.0, 140.0)], oneway: false };
        // 線の中ほどだけを含む小さな矩形でも、外接矩形が交差するので残る。
        assert!(intersects(shape.bounds(), (35.4, 139.4, 35.6, 139.6)));
        assert!(!intersects(shape.bounds(), (37.0, 141.0, 38.0, 142.0)));
    }

    #[test]
    fn generation_advances_only_when_cells_change() {
        let _env = TestEnv::new("gen");
        let mut l = test_layer(three_cells, 0);
        assert_eq!(l.generation(), 0);
        run_to_idle(&mut l, 14);
        assert_eq!(l.generation(), 3, "3セル受信で3回進む");
        let (cx, cy) = tokyo_center(14);
        for _ in 0..10 {
            l.tick(cx, cy, 14, true);
        }
        assert_eq!(l.generation(), 3, "変化が無ければ進まない");
    }

    #[test]
    fn cells_outside_the_view_are_dropped_once_memory_fills_up() {
        let _env = TestEnv::new("evict");
        let mut l = test_layer(three_cells, 0);
        let now = plotcache::now_secs();
        // 視野内の3セルは fresh にしておく(この tick でジョブを起こさせないため)。
        for k in ["1", "2", "3"] {
            l.cells.insert(
                k.to_string(),
                Cached { items: vec![test_item(1.0)], fetched_at: now, data_at: now },
            );
        }
        // 視野外のセルを上限ぶん詰めてから tick すると、古い順に落ちて上限へ収まる。
        for i in 0..MAX_CELLS_IN_MEMORY + 5 {
            l.cells.insert(
                format!("z{i}"),
                Cached { items: Vec::<TestItem>::new(), fetched_at: i as u64, data_at: i as u64 },
            );
        }
        let (cx, cy) = tokyo_center(14);
        l.tick(cx, cy, 14, true);
        assert!(!l.job_active(), "視野内が fresh ならジョブは起きない");
        assert_eq!(l.cells.len(), MAX_CELLS_IN_MEMORY, "退避後: {}", l.cells.len());
        assert!(!l.cells.contains_key("z0"), "最も古いセルから捨てる");
        assert!(l.cells.contains_key(&format!("z{}", MAX_CELLS_IN_MEMORY + 4)));
        for k in ["1", "2", "3"] {
            assert!(l.cells.contains_key(k), "視野内のセルは捨てない({k})");
        }
    }

    #[test]
    fn road_shapes_round_trip_through_json_with_the_designed_field_names() {
        let s = RoadShape { pts: vec![(35.123456, 139.654321)], oneway: true };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"pts":[[35.123456,139.654321]],"oneway":true}"#);
        assert_eq!(serde_json::from_str::<RoadShape>(&json).unwrap(), s);
    }

    #[test]
    fn round6_trims_to_about_ten_centimetres() {
        assert_eq!(round6(35.123456789), 35.123457);
        assert_eq!(round6(-139.0000004), -139.0);
    }

    #[test]
    fn parse_code_rejects_a_non_numeric_key() {
        assert!(parse_code("5339").is_ok());
        assert!(parse_code("abc").is_err());
        assert!(parse_code("").is_err());
    }

    #[test]
    fn disaster_cells_tag_the_mesh_code_with_the_year_threshold() {
        let (cx, cy) = tokyo_center(12);
        let b = view_bbox(cx, cy, 12);
        let cells = disaster_cells(b);
        assert!(cells.contains(&format!("5339_{}", disaster::DEFAULT_SINCE_YEAR)), "{cells:?}");
        // 交通量/規制と同じ1次メッシュの格子(接尾辞を外すと primary_cells と同じ集合になる)。
        let mut got: Vec<u32> = cells.iter().map(|k| k.split('_').next().unwrap().parse().unwrap()).collect();
        let mut want = mesh::primary_codes(b.0, b.1, b.2, b.3);
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    #[test]
    fn disaster_cell_keys_are_accepted_by_the_disk_cache() {
        // plotcache::valid_key は英数字と - _ の16文字以内しか許さない。
        let _env = TestEnv::new("diskey");
        for z in [9u32, 11] {
            let (cx, cy) = tokyo_center(z);
            for k in disaster_cells(view_bbox(cx, cy, z)) {
                assert!(k.len() <= 16, "キー {k} が16文字を超える");
                assert!(
                    plotcache::store(&_env.root, Layer::Disaster, &k, &[test_item(1.0)], 0, 0).is_ok(),
                    "キー {k} がディスク層に弾かれる"
                );
            }
        }
    }

    #[test]
    fn split_disaster_key_round_trips_with_disaster_cells() {
        assert_eq!(split_disaster_key("5339_1926").unwrap(), (5339, 1926));
        assert_eq!(split_disaster_key("906_0").unwrap(), (906, 0), "全期間");
        for bad in ["5339", "", "_1926", "5339_", "abc_1926", "5339_x", "5339-1926"] {
            assert!(split_disaster_key(bad).is_err(), "key={bad:?}");
        }
        // disaster_cells が作るキーは必ず読み戻せる。
        let (cx, cy) = tokyo_center(11);
        for k in disaster_cells(view_bbox(cx, cy, 11)) {
            let (code, since) = split_disaster_key(&k).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(since, disaster::DEFAULT_SINCE_YEAR);
            let _ = mesh::primary_bbox(code); // コードから矩形が引ける(パニックしない)ことの確認
        }
    }

    // 過去災害/境界は MAX_CELLS_PER_JOB の上限を持たない(new_uncapped)。z9で1次メッシュ
    // 48枚が必要な東京でも、1件も取りに行かず諦める(suppressed)ことなくmissingが積まれる。
    // 設計 docs/disaster-choropleth-unlimited-zoom-design.md §3.2。
    // 実際のdisaster()は実ネットワークを叩くfetch_disaster_cellを使うため、ここでは
    // ten_cells(10件、通常上限9を超える)+test_fetch(モック)の組み合わせで安全に確認する。
    #[test]
    fn uncapped_layer_does_not_suppress_when_more_than_the_normal_cap_is_needed() {
        let _env = TestEnv::new("uncapped");
        let mut l = test_layer_uncapped(ten_cells, 0);
        let (cx, cy) = tokyo_center(14);
        l.tick(cx, cy, 14, true);
        assert!(!l.suppressed(), "上限無しレイヤは10セル要求でもsuppressedにならない");
        assert_eq!(l.view_keys.len(), 10, "上限を気にせず全セルをview_keysへ積む");
    }

    // 実際にz9の東京(48セル)でも同様にsuppressedにならないこと(disaster_cellsの実出力件数で確認)。
    #[test]
    fn disaster_cells_exceed_the_normal_job_cap_at_wide_zoom_but_layer_is_uncapped() {
        let (cx, cy) = tokyo_center(9);
        let n = disaster_cells(view_bbox(cx, cy, 9)).len();
        assert!(n > MAX_CELLS_PER_JOB, "z9 東京で {n} セル(通常上限 {MAX_CELLS_PER_JOB} を超えているはず)");
        let _env = TestEnv::new("uncapped_real_cells");
        let mut l = test_layer_uncapped(disaster_cells, 0);
        l.tick(cx, cy, 9, true);
        assert!(!l.suppressed(), "実際のdisaster_cells件数({n})でもsuppressedにならない");
        assert_eq!(l.view_keys.len(), n);
    }

    // z9だけでなくz10〜z13(塗りが出る全ズーム帯)でも同じ仕組みが効くこと。修正自体が
    // 「上限を外す」というズーム非依存の変更なので、特定のズームだけ効くような分岐は無い
    // はずだが、それを実際に確認しておく(設計 §3.2は全ズーム帯が対象)。
    #[test]
    fn disaster_layer_stays_unsuppressed_across_every_fill_zoom() {
        let _env = TestEnv::new("uncapped_all_zooms");
        for z in [9u32, 10, 11, 12, 13] {
            let (cx, cy) = tokyo_center(z);
            let n = disaster_cells(view_bbox(cx, cy, z)).len();
            let mut l = test_layer_uncapped(disaster_cells, 0);
            l.tick(cx, cy, z, true);
            assert!(!l.suppressed(), "z{z} 東京で{n}セル要求でもsuppressedになってはいけない");
            assert_eq!(l.view_keys.len(), n, "z{z}");
        }
    }

    // 中心から近いセルほど先に来る(取得の優先度)。既知の並びで固定する:
    // 東京中心の視野では、東京駅を含むメッシュ(5339)が最初に来るはず。
    #[test]
    fn disaster_cells_are_ordered_nearest_to_center_first() {
        let (cx, cy) = tokyo_center(9);
        let cells = disaster_cells(view_bbox(cx, cy, 9));
        assert!(cells.len() > 1, "並び順を確認するには2件以上要る: {cells:?}");
        assert_eq!(cells[0], format!("5339_{}", disaster::DEFAULT_SINCE_YEAR), "先頭は中心を含むメッシュのはず: {cells:?}");
    }

    #[test]
    fn sort_codes_by_distance_from_center_orders_nearest_first() {
        // primary_bboxの実コードで、東京(5339)・大阪(5235)・札幌(6441)相当のものを使う。
        let mut codes = vec![6441u32, 5235, 5339];
        let tokyo_view = mesh::primary_bbox(5339);
        sort_codes_by_distance_from_center(&mut codes, mesh::primary_bbox, tokyo_view);
        assert_eq!(codes[0], 5339, "視野の中心自身のコードが最初に来るはず: {codes:?}");
    }

    #[test]
    fn sort_codes_by_distance_from_center_handles_empty_and_single() {
        let mut empty: Vec<u32> = Vec::new();
        sort_codes_by_distance_from_center(&mut empty, mesh::primary_bbox, (35.0, 139.0, 36.0, 140.0));
        assert!(empty.is_empty());
        let mut single = vec![5339u32];
        sort_codes_by_distance_from_center(&mut single, mesh::primary_bbox, (35.0, 139.0, 36.0, 140.0));
        assert_eq!(single, vec![5339]);
    }

    // 6種の実データ型が、実際にディスクへ書いて読み戻せること
    // (型ごとの serde 単体テストとは別に、plotcache を通した往復を1本で押さえる)。
    #[test]
    fn all_six_real_item_types_survive_a_trip_through_the_disk_cache() {
        let _env = TestEnv::new("realtypes");
        let root = _env.root.clone();
        let now = plotcache::now_secs();

        let t = vec![traffic::TrafficPoint { lat: 35.6, lon: 139.7, volume: 135 }];
        plotcache::store(&root, Layer::Traffic, "5339", &t, now, now - 25 * 60).unwrap();
        let got = plotcache::load::<traffic::TrafficPoint>(&root, Layer::Traffic, "5339", now).unwrap();
        assert_eq!(got.items, t);
        assert_eq!(got.age_secs(now), 25 * 60, "交通量は観測時刻からの経過になる");

        let r = vec![RoadShape { pts: vec![(35.6, 139.7), (35.61, 139.71)], oneway: true }];
        plotcache::store(&root, Layer::Roads, "533946", &r, now, now).unwrap();
        assert_eq!(plotcache::load::<RoadShape>(&root, Layer::Roads, "533946", now).unwrap().items, r);

        let c = vec![camera::RoadCamera {
            id: "811C200101".into(),
            lat: 42.5,
            lon: 140.36,
            name: "長万部町大浜情報板".into(),
            thumb_url: Some("https://example.invalid/s_x.jpeg".into()),
            full_url: Some("https://example.invalid/x.jpeg".into()),
            taken_at: "2026-08-16 16:00:36".into(),
        }];
        plotcache::store(&root, Layer::Camera, "81", &c, now, now).unwrap();
        let back = plotcache::load::<camera::RoadCamera>(&root, Layer::Camera, "81", now).unwrap();
        assert_eq!(back.items[0].id, c[0].id);
        assert_eq!(back.items[0].name, c[0].name);
        assert_eq!(back.items[0].full_url, None, "期限切れになるURLは持ち越さない");

        let g = vec![regulation::ClosureEvent {
            line: vec![(35.64, 139.73), (35.641, 139.732)],
            kind: regulation::RegulationKind::Closed,
            detail_id: "2431834e238b1115".to_string(),
            active: true,
        }];
        plotcache::store(&root, Layer::Regulation, "5339", &g, now, now).unwrap();
        assert_eq!(
            plotcache::load::<regulation::ClosureEvent>(&root, Layer::Regulation, "5339", now).unwrap().items,
            g
        );

        let d = vec![disaster::DisasterSite {
            lat: 35.955106,
            lon: 139.874828,
            muni_code: "12208".to_string(),
            kinds: vec![disaster::KindCount {
                kind: disaster::DisasterKind::Storm,
                count: 60,
                year_min: 1926,
                year_max: 2019,
            }],
        }];
        plotcache::store(&root, Layer::Disaster, "w1309_1926", &d, now, now).unwrap();
        assert_eq!(
            plotcache::load::<disaster::DisasterSite>(&root, Layer::Disaster, "w1309_1926", now).unwrap().items,
            d
        );

        let m = vec![muni::MuniArea {
            code: "1220800".to_string(),
            name: "野田市".to_string(),
            rings: vec![vec![(35.9, 139.8), (35.9, 139.9), (36.0, 139.9)]],
            bbox: (35.9, 139.8, 36.0, 139.9),
        }];
        plotcache::store(&root, Layer::Boundary, "c20s3", &m, now, now).unwrap();
        let back = plotcache::load::<muni::MuniArea>(&root, Layer::Boundary, "c20s3", now).unwrap();
        assert_eq!(back.items, m);
        assert_eq!(back.items[0].muni_code(), "12208", "読み戻した区域から結合キーが引ける");
    }

    #[test]
    fn the_six_real_layers_use_the_ttls_and_zoom_floors_from_the_design() {
        assert_eq!(traffic().min_zoom, 11);
        assert_eq!(traffic().data_lag_secs, 25 * 60);
        assert_eq!(roads().min_zoom, 14);
        assert_eq!(camera().min_zoom, 0);
        assert_eq!(regulation().min_zoom, 11);
        // 過去災害と境界だけは z9(コロプレスを広域で出すため。設計 §1-1/§2.5)。
        // z8 は広域セルが20枚要り、かつ取得元の集計が2,000行で打ち切られるので下限にしない。
        assert_eq!(disaster().min_zoom, 9);
        assert_eq!(traffic().layer.fresh_ttl().as_secs(), 300);
        assert_eq!(regulation().layer.fresh_ttl().as_secs(), 600);
        assert_eq!(disaster().layer.fresh_ttl().as_secs(), 30 * 24 * 3600, "過去災害は30日");
        assert_eq!(disaster().layer.stale_limit(), None, "古い集計が誤りになることはない");
        assert_eq!(disaster().data_lag_secs, 0);
        // 市区町村境界は過去災害の塗りにだけ使うので、ズーム下限を過去災害と揃える。
        assert_eq!(boundary().min_zoom, disaster().min_zoom, "塗りだけ端が欠ける状態を作らない");
        assert_eq!(boundary().min_zoom, 9);
        assert_eq!(boundary().layer.fresh_ttl().as_secs(), 180 * 24 * 3600, "境界は180日");
        assert_eq!(boundary().layer.stale_limit(), None, "古くても境界が誤りになることはない");
        assert_eq!(boundary().data_lag_secs, 0);
    }

    #[test]
    fn boundary_cells_name_the_class20s_file_that_covers_the_view() {
        let (cx, cy) = tokyo_center(14);
        assert_eq!(boundary_cells(view_bbox(cx, cy, 14)), vec!["c20s3".to_string()], "関東");
        // z11 の視野(±約56km)は中部の矩形にも掛かるので複数返るが、関東は必ず入っている。
        let (cx11, cy11) = tokyo_center(11);
        let wide = boundary_cells(view_bbox(cx11, cy11, 11));
        assert!(wide.contains(&"c20s3".to_string()), "{wide:?}");
        let (cx, cy) = deg_to_pixel(43.06, 141.35, 11);
        assert_eq!(boundary_cells(view_bbox(cx, cy, 11)), vec!["c20s0".to_string()], "北海道");
        let (cx, cy) = deg_to_pixel(26.21, 127.68, 11);
        assert_eq!(boundary_cells(view_bbox(cx, cy, 11)), vec!["c20s9".to_string()], "沖縄");
        // 日本の外では取りに行かない。
        let (cx, cy) = deg_to_pixel(48.85, 2.35, 11);
        assert!(boundary_cells(view_bbox(cx, cy, 11)).is_empty(), "パリ");
    }

    #[test]
    fn boundary_cell_keys_are_accepted_by_the_disk_cache() {
        // plotcache::valid_key は英数字と - _ の16文字以内しか許さない("c20s3" は5文字)。
        let _env = TestEnv::new("bndkey");
        for i in 0..muni::RELM_COUNT {
            let k = format!("{BOUNDARY_KEY_PREFIX}{i}");
            assert!(
                plotcache::store(&_env.root, Layer::Boundary, &k, &[test_item(1.0)], 0, 0).is_ok(),
                "キー {k} がディスク層に弾かれる"
            );
        }
    }

    // 境界データ(気象庁 class20s)は全国で10ファイルしかないので広域でも増えようがない
    // (設計 §0.2 の実測で z9 は最大5枚)。ズーム下限を過去災害と揃えて 9 まで下げても上限内。
    #[test]
    fn boundary_cells_stay_within_the_job_cap_everywhere_in_japan() {
        for (lat, lon) in [(35.68, 139.77), (43.06, 141.35), (26.21, 127.68), (34.69, 135.52), (36.5, 138.7)] {
            for z in [9u32, 10, 11, 12, 14] {
                let (cx, cy) = deg_to_pixel(lat, lon, z);
                let n = boundary_cells(view_bbox(cx, cy, z)).len();
                assert!(n <= MAX_CELLS_PER_JOB, "z{z} {lat},{lon} で {n} セル");
            }
        }
    }

    // 旧形式のキー(ディスクに残っている "5339_1926")を渡されても、通信に出る前に弾く。
    // 数字として読めてしまうため、素通しすると別地域の矩形を取りに行く(設計 §2.6)。
    #[test]
    fn fetch_disaster_cell_rejects_a_key_it_did_not_produce() {
        for bad in ["", "5339", "_1926", "5339_", "abc_1926", "5339_x", "5339-1926"] {
            assert!(fetch_disaster_cell(bad, &mut None).is_err(), "key={bad:?}");
        }
    }

    #[test]
    fn fetch_boundary_cell_rejects_a_key_it_did_not_produce() {
        // ネットワークに出る前に弾かれること(範囲内の番号だけは実通信になるので試さない)。
        for bad in ["", "3", "c20s", "c20sx", "5339", "c20s-1"] {
            assert!(fetch_boundary_cell(bad, &mut None).is_err(), "key={bad:?}");
        }
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。`cargo test --release -- --ignored`で実行。
    // 最も混む広域セル(1309 = 緯度34.67〜37.33度・経度136〜140度。東京・横浜・名古屋・静岡・
    // 長野を含む)の集計が、取得元の打ち切り(maxRecordCount=2,000行)に当たらないこと。
    // 集計クエリでは resultOffset が黙って無視されるので、**打ち切られたら回復手段が無い**。
    // 打ち切られると市区町村がまるごと塗られなくなり、画面上は「記録が無い」と区別がつかない
    // (設計 §2.2/§7)。データが増えて近づいたら気づけるよう、地点数を出力する。
    #[test]
    #[ignore]
    fn live_the_busiest_wide_cell_is_not_truncated() {
        let key = format!("w1309_{}", disaster::DEFAULT_SINCE_YEAR);
        let sites = fetch_disaster_cell(&key, &mut None).expect("live fetch should succeed");
        println!("広域セル {key}: 地点 {} 件", sites.len());
        assert!(!sites.is_empty(), "最も混むセルが空はおかしい");
        assert!(!disaster::truncation_seen(), "2,000行の打ち切りに当たっている(件数が黙って減る)");
        // 全期間(since=0)でも余裕があること(Stage2 で年代しきい値を設定に出すときの前提)。
        let all = fetch_disaster_cell("w1309_0", &mut None).expect("live fetch should succeed");
        println!("広域セル w1309_0(全期間): 地点 {} 件", all.len());
        assert!(!disaster::truncation_seen(), "全期間で打ち切りに当たっている");
    }

    // 人口メッシュは1セルが最大3.6MBあるため、保持上限を4に下げてある(設計 §6.4-2)。
    // 他レイヤの32は変わらない。
    #[test]
    fn the_memory_cap_is_per_layer_not_global() {
        assert_eq!(traffic().max_in_memory, MAX_CELLS_IN_MEMORY);
        assert_eq!(roads().max_in_memory, MAX_CELLS_IN_MEMORY);
        assert_eq!(camera().max_in_memory, MAX_CELLS_IN_MEMORY);
        assert_eq!(regulation().max_in_memory, MAX_CELLS_IN_MEMORY);
        assert_eq!(disaster().max_in_memory, MAX_CELLS_IN_MEMORY);
        assert_eq!(population().max_in_memory, POPULATION_CELLS_IN_MEMORY);
        assert_eq!(POPULATION_CELLS_IN_MEMORY, 4);
    }

    #[test]
    fn a_layer_with_a_small_cap_drops_down_to_that_cap() {
        let _env = TestEnv::new("cap");
        let mut l = test_layer_capped(three_cells, 4);
        let now = plotcache::now_secs();
        for k in ["1", "2", "3"] {
            l.cells.insert(
                k.to_string(),
                Cached { items: vec![test_item(1.0)], fetched_at: now, data_at: now },
            );
        }
        for i in 0..10 {
            l.cells.insert(
                format!("z{i}"),
                Cached { items: Vec::<TestItem>::new(), fetched_at: i as u64, data_at: i as u64 },
            );
        }
        let (cx, cy) = tokyo_center(14);
        l.tick(cx, cy, 14, true);
        assert_eq!(l.cells.len(), 4, "上限4まで削る: {}", l.cells.len());
        for k in ["1", "2", "3"] {
            assert!(l.cells.contains_key(k), "視野内のセルは捨てない({k})");
        }
        assert!(l.cells.contains_key("z9"), "視野外は最も新しい1件だけ残る");
    }

    // 通信を始めたセルのキーが取れる(人口メッシュのステータス「北海道を取得中…」用)。
    #[test]
    fn the_key_being_fetched_is_visible_while_the_job_runs() {
        let _env = TestEnv::new("fetchkey");
        let mut l = test_layer(three_cells, 0);
        assert_eq!(l.fetching_key(), None, "ジョブが無ければ何も待っていない");
        let (cx, cy) = tokyo_center(14);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen: Vec<String> = Vec::new();
        loop {
            l.tick(cx, cy, 14, true);
            if let Some(k) = l.fetching_key() {
                if seen.last().map(String::as_str) != Some(k) {
                    seen.push(k.to_string());
                }
            }
            if !l.job_active() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "ジョブが終わらない");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!seen.is_empty(), "取得中のキーが1度も見えていない");
        assert!(seen.iter().all(|k| ["1", "2", "3"].contains(&k.as_str())), "{seen:?}");
        assert_eq!(l.fetching_key(), None, "ジョブが終わったら消える");
    }

    // ディスクの fresh 値で済んだセルでは Started を送らない(通信していないので待たせていない)。
    #[test]
    fn a_cell_served_from_a_fresh_disk_entry_never_reports_as_fetching() {
        let _env = TestEnv::new("nofetchkey");
        let mut l = test_layer(three_cells, 0);
        run_to_idle(&mut l, 14);
        let mut l2 = test_layer(three_cells, 0);
        let (cx, cy) = tokyo_center(14);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            l2.tick(cx, cy, 14, true);
            assert_eq!(l2.fetching_key(), None, "ディスクから読むだけなのに取得中と出ている");
            if !l2.job_active() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "ジョブが終わらない");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    // ---- 人口メッシュ(セル=都道府県) ----

    #[test]
    fn population_cell_keys_are_zero_padded_and_accepted_by_the_disk_cache() {
        let _env = TestEnv::new("popkeys");
        // 索引の中身に依存しないよう、キーの形だけを直接確かめる。
        for pref in [1u8, 13, 47] {
            let key = format!("{pref:02}");
            assert_eq!(key.len(), 2);
            assert!(
                plotcache::store(&_env.root, Layer::Population, &key, &[test_item(1.0)], 0, 0).is_ok(),
                "キー {key} がディスク層に弾かれる"
            );
        }
    }

    #[test]
    fn population_cells_never_exceed_the_job_cap() {
        // 索引が全国ぶん揃っている前提のテストにしないため、上限を超えないことだけを見る
        // (z11の視野が跨る都道府県は関東・近畿の県境付近でも3〜4件)。
        for (lat, lon) in [(35.68, 139.77), (43.06, 141.35), (34.69, 135.52), (26.21, 127.68)] {
            let (cx, cy) = deg_to_pixel(lat, lon, 11);
            let n = population_cells(view_bbox(cx, cy, 11)).len();
            assert!(n <= MAX_CELLS_PER_JOB, "z11 {lat},{lon} で {n} セル");
        }
    }

    #[test]
    fn population_cells_are_empty_outside_japan() {
        let (cx, cy) = deg_to_pixel(48.85, 2.35, 12); // パリ
        assert!(population_cells(view_bbox(cx, cy, 12)).is_empty());
    }

    #[test]
    fn fetch_population_cell_rejects_a_non_numeric_key_without_touching_the_network() {
        let mut scratch = None;
        assert!(fetch_population_cell("abc", &mut scratch).is_err());
        assert!(fetch_population_cell("", &mut scratch).is_err());
        // 範囲外の都道府県コードも通信前に断る(population 側の検査)。
        assert!(fetch_population_cell("99", &mut scratch).is_err());
    }

    // 500mメッシュの矩形は MESH_ID から作られる(幾何を保存していない)。
    #[test]
    fn a_population_mesh_reports_the_rectangle_of_its_code() {
        let m = population::PopMesh {
            mesh: 523351132,
            pop: [0.0; population::YEARS.len()],
            aged: [f32::NAN; population::AGED_YEARS],
        };
        let b = m.bounds();
        assert_eq!(b, mesh::half_mesh_bbox(523351132));
        // 視野との交差判定がそのまま効く(items() のフィルタ)。
        assert!(intersects(b, (35.09, 133.16, 35.10, 133.18)));
        assert!(!intersects(b, (35.60, 139.70, 35.70, 139.80)));
    }

    #[test]
    fn the_population_layer_uses_the_ttl_and_zoom_floor_from_the_design() {
        assert_eq!(population().min_zoom, 11);
        assert_eq!(population().data_lag_secs, 0);
        assert_eq!(population().layer.fresh_ttl().as_secs(), 365 * 24 * 3600, "推計の改定は数年に1度");
        assert_eq!(population().layer.stale_limit(), None, "古い推計が誤りになることはない");
        assert_eq!(population().layer.max_entries(), 47, "都道府県は47しかない");
    }

    // 人口メッシュがディスクキャッシュを往復できること(NaN を含む年齢構成比も含めて)。
    #[test]
    fn a_population_mesh_survives_a_trip_through_the_disk_cache() {
        let _env = TestEnv::new("poptype");
        let now = plotcache::now_secs();
        let mut aged = [12.5f32; population::AGED_YEARS];
        aged[2] = f32::NAN; // 秘匿対象の年
        let p = vec![population::PopMesh {
            mesh: 523351132,
            pop: [1.0, 28.5645, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0],
            aged,
        }];
        plotcache::store(&_env.root, Layer::Population, "31", &p, now, now).unwrap();
        let back = plotcache::load::<population::PopMesh>(&_env.root, Layer::Population, "31", now).unwrap();
        assert_eq!(back.items[0].mesh, 523351132);
        assert_eq!(back.items[0].pop[1], 28.5645);
        assert_eq!(back.items[0].aged[0], 12.5);
        assert!(back.items[0].aged[2].is_nan(), "NaN(データなし)が保たれていない");
    }
}
