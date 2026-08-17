// 500mメッシュ別の推計人口(国土数値情報「500mメッシュ別将来推計人口データ」国土交通省)。
// 走る前・走っている最中に「この先に人がいるか」を読むためのレイヤ。
// 調査は docs/population-opendata-investigation.md、設計は docs/population-mesh-overlay-design.md。
//
// 実測で確認済みの構造(2026/08/17):
//   - 配布は都道府県単位のzip1本。認証・Referer・Cookie 不要。北海道が最悪ケースで
//     zip 30MB・展開264MB・40,544件(§2.2)。全国1本(511MB)は端末アプリの取得単位にしない。
//   - 幾何は全件が軸平行の矩形で、MESH_ID(9桁)から完全に復元できる(50,074件検査して不一致0)。
//     そのため頂点は保存も解析もせず、mesh::half_mesh_bbox で毎回計算する(§2.4)。
//   - 塗りに使うのは PTN_*(合算前)。PT00_* は秘匿処理で0にされたメッシュが出るため、
//     「人がいない帯」が実際とは違う場所に出る(§2.5)。
//   - 年齢構成比 RTC_* は PT00_* 基準なので、秘匿対象(PT00=0)では意味を持たない=データなし扱い。
//   - zipは3エントリともデータディスクリプタ無し(flag=0x0000)で、ローカルヘッダだけで
//     csize/usize が確定する。中央ディレクトリを読まずに先頭から辿れる(§3)。
//
// traffic.rs/regulation.rs/disaster.rs と同じく std + ureq + serde_json(+ flate2)だけに依存し、
// crate:: 参照は mesh のみにしてある。これは索引生成器(src/bin/gen-mesh2pref.rs)が
// #[path] でこのファイルと mesh.rs だけを取り込んで動くようにするため。

use serde::de::{Deserializer, IgnoredAny};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

use crate::mesh;

/// (lat_min, lon_min, lat_max, lon_max)。plotlayer::Bbox と同じ形(この型を別に持つのは、
/// 索引生成器が plotlayer を取り込まずにこのファイルを使えるようにするため)。
pub type Bbox = (f64, f64, f64, f64);

/// 推計の版を表すディレクトリ名。「R6推計・2024年公開」。次の推計が出たら変わるので定数1つに
/// 切り出してある。版を上げたら plotcache::FORMAT_VERSION も上げて旧データを一掃する(§6.2)。
pub const DATASET_DIR: &str = "m500r6-24";
const BASE_URL: &str = "https://nlftp.mlit.go.jp/ksj/gml/data/m500r6";
const USER_AGENT: &str = "termmap/0.1 (personal experiment)";
// 1都道府県まるごとの取得。北海道の30MBを細い回線で引く前提なので、他レイヤ(20秒)より長く取る。
const HTTP_TIMEOUT_SECS: u64 = 180;
// zipの受け入れ上限。実測の最大は北海道の30.1MB。桁が変わるほど大きいものは配信側の異常とみなす。
const MAX_ZIP_BYTES: u64 = 64 * 1024 * 1024;
// 展開後の受け入れ上限(zip爆弾よけ)。実測の最大は北海道の264MB。
const MAX_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

/// 収録されている年次。2020年は令和2年国勢調査に基づく実績で、それ以降は推計値。
pub const YEARS: [u16; 11] = [2020, 2025, 2030, 2035, 2040, 2045, 2050, 2055, 2060, 2065, 2070];
/// 年齢構成比(`aged`)が存在する年次の数。2020年に年齢別の値は無いので YEARS より1つ少ない。
pub const AGED_YEARS: usize = YEARS.len() - 1;

/// 出典表記(利用規約が明示を求めている)。ヘルプ・MANUAL・ONにした直後のメッセージで出す。
pub const ATTRIBUTION: &str = "出典: 国土数値情報(500mメッシュ別将来推計人口データ) 国土交通省";

/// 500mメッシュ1枚ぶんの推計人口。幾何は持たない(MESH_ID から復元できる・§2.4)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PopMesh {
    /// 2分の1地域メッシュコード(9桁)。u32 に収まる(最大 999,999,999 < u32::MAX)。
    pub mesh: u32,
    /// 総数人口(PTN_*、合算前)。索引は YEARS と同じ並び(2020,2025,…,2070 の11要素)。
    pub pop: [f32; YEARS.len()],
    /// 65歳以上の構成比(RTC_*、%)。2025〜2070 の10要素。2020年に年齢別の値は存在しない。
    /// 秘匿対象メッシュ(PT00=0)では意味を持たないので f32::NAN が入る。
    #[serde(with = "nan_array")]
    pub aged: [f32; AGED_YEARS],
}

// NaN(データなし)を JSON の null として往復させる。serde_json は f32 の非有限値を null として
// 書くが、読み戻す側は null を f32 として受け付けないため、ディスクキャッシュ(plotcache)を
// 通すと壊れる。設計 §5.1 の「値が無いときは NaN」を保ったまま保存できるようにここで橋渡しする。
mod nan_array {
    use super::*;

    pub fn serialize<S: Serializer>(v: &[f32; AGED_YEARS], s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(AGED_YEARS))?;
        for x in v {
            if x.is_finite() {
                seq.serialize_element(x)?;
            } else {
                seq.serialize_element(&Option::<f32>::None)?;
            }
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[f32; AGED_YEARS], D::Error> {
        let v = <[Option<f32>; AGED_YEARS]>::deserialize(d)?;
        Ok(v.map(|x| x.unwrap_or(f32::NAN)))
    }
}

/// 解析結果1件。`shicode`(行政区域コード5桁)は索引生成でしか使わないので PopMesh には持たせない。
#[derive(Debug)]
pub struct PopRecord {
    /// 本体(termmap)では読まない。索引生成器(src/bin/gen-mesh2pref.rs)だけが使う。
    #[allow(dead_code)]
    pub shicode: u32,
    pub mesh: PopMesh,
}

/// 表示指標。Stage1 は Density のみ実装する(§7.5。配色は実際に画面で見てから決める)。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Metric {
    Density,
    #[allow(dead_code)] // Stage2 で実装(高齢化率)
    Aging,
    #[allow(dead_code)] // Stage2 で実装(2020年比の増減率)
    Change,
}

/// 年次(西暦) → `pop` / YEARS の索引。5年刻みの一覧に無い年は None。
pub fn year_index(year: u16) -> Option<usize> {
    YEARS.iter().position(|y| *y == year)
}

/// 人口密度(人/km²)。500mメッシュは0.25km²なのでメッシュ人口×4。
/// 値が無い(年次が範囲外・NaN)場合は None。
pub fn density(m: &PopMesh, year_idx: usize) -> Option<f64> {
    let v = *m.pop.get(year_idx)? as f64;
    if v.is_finite() {
        Some(v * 4.0)
    } else {
        None
    }
}

/// メッシュ1枚と年・指標から塗る階級(1..=6)を決める。値が無い/人口0なら None(塗らない)。
/// 階級は視野に依存しない固定値にする(視野内で正規化するとパンするたび同じ土地の色が変わる)。
/// 境界は下側を含む: 〜100 / 100〜1,000 / 1,000〜4,000 / 4,000〜10,000 / 10,000〜20,000 / 20,000〜
pub fn class_of(m: &PopMesh, year_idx: usize, metric: Metric) -> Option<u8> {
    if metric != Metric::Density {
        return None; // Stage1 は人口密度だけ(§7.5)
    }
    let d = density(m, year_idx)?;
    if d <= 0.0 {
        return None; // 無人・海・山は塗らない
    }
    Some(match d {
        d if d < 100.0 => 1,
        d if d < 1_000.0 => 2,
        d if d < 4_000.0 => 3,
        d if d < 10_000.0 => 4,
        d if d < 20_000.0 => 5,
        _ => 6,
    })
}

/// 階級 → 塗る色(RGBA)。色相は紫〜マゼンタの単一色相で明度を変える(§7.4)。
/// 地図の分類色(水=青/緑地=緑/幹線=黄)とも雨雲の気象庁配色(青→緑→黄→赤)とも衝突させない。
/// 薄い階級はアルファも低いので、人の少ない土地は地図が透けたまま残る。
pub fn class_color(class: u8) -> [u8; 4] {
    match class {
        1 => [250, 232, 245, 40],
        2 => [232, 190, 230, 80],
        3 => [208, 140, 210, 120],
        4 => [176, 92, 186, 160],
        5 => [138, 52, 150, 200],
        _ => [96, 20, 110, 230],
    }
}

/// 都道府県コード(1..=47) → zipのURL。
pub fn zip_url(pref: u8) -> String {
    format!("{BASE_URL}/{DATASET_DIR}/500m_mesh_2024_{pref:02}_GEOJSON.zip")
}

/// 都道府県コード(1..=47) → 名前。ステータス行の「北海道を取得中…」に使う。
pub fn pref_name(pref: u8) -> &'static str {
    const NAMES: [&str; 47] = [
        "北海道", "青森県", "岩手県", "宮城県", "秋田県", "山形県", "福島県", "茨城県", "栃木県",
        "群馬県", "埼玉県", "千葉県", "東京都", "神奈川県", "新潟県", "富山県", "石川県", "福井県",
        "山梨県", "長野県", "岐阜県", "静岡県", "愛知県", "三重県", "滋賀県", "京都府", "大阪府",
        "兵庫県", "奈良県", "和歌山県", "鳥取県", "島根県", "岡山県", "広島県", "山口県", "徳島県",
        "香川県", "愛媛県", "高知県", "福岡県", "佐賀県", "長崎県", "熊本県", "大分県", "宮崎県",
        "鹿児島県", "沖縄県",
    ];
    NAMES.get(pref.wrapping_sub(1) as usize).copied().unwrap_or("")
}

// ---- 2次メッシュ → 都道府県 の索引(§4.2) ----
//
// 都道府県の外接矩形では判定できない。東京都は小笠原・南鳥島まで含むため緯度24.3〜35.9度・
// 経度139.0〜154.0度に及び、沖縄本島を見ているだけで東京(8.19MB)を取りに行ってしまう。
// 判定は矩形ではなくメッシュコードで行う。索引はデータ自身から作れる(各 feature が
// MESH_ID の上6桁=2次メッシュと SHICODE の上2桁=都道府県を両方持っている)。

const MESH2PREF_CSV: &str = include_str!("../data/mesh2pref.csv");

fn index() -> &'static HashMap<u32, Vec<u8>> {
    static INDEX: OnceLock<HashMap<u32, Vec<u8>>> = OnceLock::new();
    INDEX.get_or_init(|| parse_index(MESH2PREF_CSV))
}

/// `533946,13,14` 形式の索引を読む。空行と `#` 始まりは読み飛ばす。
/// 壊れた行は黙って捨てる(索引に無い2次メッシュは「何も取得せず何も描かない」で正しく畳める)。
pub fn parse_index(csv: &str) -> HashMap<u32, Vec<u8>> {
    let mut out: HashMap<u32, Vec<u8>> = HashMap::new();
    for line in csv.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split(',');
        let Some(Ok(code)) = it.next().map(str::parse::<u32>) else { continue };
        let prefs: Vec<u8> = it.filter_map(|p| p.trim().parse::<u8>().ok()).filter(|p| (1..=47).contains(p)).collect();
        if !prefs.is_empty() {
            out.entry(code).or_default().extend(prefs);
        }
    }
    for v in out.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    out
}

/// 視野bboxに掛かる都道府県コード。重複は除き、昇順で返す。
/// 索引に無い2次メッシュ(人口メッシュが1枚も無い海上・無人の山域)は原典にもデータが無いので
/// 何も返さない = 何も取得せず何も描かない。
pub fn prefectures_for(b: Bbox) -> Vec<u8> {
    prefectures_in(index(), b)
}

// 索引を差し替えられる形の本体(テストが埋め込み索引の中身に依存しないように分けてある)。
fn prefectures_in(idx: &HashMap<u32, Vec<u8>>, b: Bbox) -> Vec<u8> {
    let mut out: Vec<u8> = mesh::secondary_codes(b.0, b.1, b.2, b.3)
        .iter()
        .filter_map(|c| idx.get(c))
        .flatten()
        .copied()
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

// ---- zip(deflate)の展開 ----

/// zipのローカルファイルエントリ1件。中央ディレクトリは読まない(§3)。
#[derive(Debug, PartialEq)]
pub struct ZipEntry {
    pub name: String,
    pub method: u16,
    /// 圧縮データの開始位置(zip先頭からのバイト数)。
    pub offset: usize,
    pub csize: usize,
    pub usize_: usize,
}

const LOCAL_HEADER_SIG: u32 = 0x0403_4b50;
const LOCAL_HEADER_LEN: usize = 30;
const FLAG_DATA_DESCRIPTOR: u16 = 0x0008;
const METHOD_DEFLATE: u16 = 8;

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// 先頭から順にローカルヘッダを辿り、名前が `.geojson` で終わる最初のエントリを返す。
/// 中央ディレクトリに達したら(署名が変わったら)打ち切る。
pub fn find_geojson_entry(zip: &[u8]) -> Result<ZipEntry, String> {
    let mut at = 0usize;
    while at + LOCAL_HEADER_LEN <= zip.len() {
        if le32(zip, at) != LOCAL_HEADER_SIG {
            break; // 中央ディレクトリ(0x02014b50)等。ローカルエントリは終わり
        }
        let flags = le16(zip, at + 6);
        let method = le16(zip, at + 8);
        let csize = le32(zip, at + 18) as usize;
        let usize_ = le32(zip, at + 22) as usize;
        let nlen = le16(zip, at + 26) as usize;
        let elen = le16(zip, at + 28) as usize;
        let name_at = at + LOCAL_HEADER_LEN;
        if name_at + nlen > zip.len() {
            return Err("人口メッシュ: zipのファイル名が途中で切れている".to_string());
        }
        let name = String::from_utf8_lossy(&zip[name_at..name_at + nlen]).into_owned();
        let data_at = name_at + nlen + elen;
        if name.to_ascii_lowercase().ends_with(".geojson") {
            if flags & FLAG_DATA_DESCRIPTOR != 0 {
                // ヘッダの csize/usize が 0 になる形式。実データはこの形式では配られていない
                // (3エントリとも flag=0x0000 を実測)ので、対応せず明示的に断る。
                return Err("人口メッシュ: zipがデータディスクリプタ形式で読めない".to_string());
            }
            if method != METHOD_DEFLATE {
                return Err(format!("人口メッシュ: 未対応のzip圧縮方式 {method}"));
            }
            if usize_ as u64 > MAX_UNCOMPRESSED_BYTES {
                return Err(format!("人口メッシュ: 展開後が大きすぎる({usize_}バイト)"));
            }
            if data_at + csize > zip.len() {
                return Err("人口メッシュ: zipの圧縮データが途中で切れている".to_string());
            }
            return Ok(ZipEntry { name, method, offset: data_at, csize, usize_ });
        }
        // ディレクトリエントリ等は読み飛ばす。データディスクリプタ付きだと csize=0 で
        // 位置が進まないため、そこで無限に回らないよう打ち切る。
        if flags & FLAG_DATA_DESCRIPTOR != 0 {
            return Err("人口メッシュ: zipがデータディスクリプタ形式で読めない".to_string());
        }
        let next = data_at + csize;
        if next <= at {
            break;
        }
        at = next;
    }
    Err("人口メッシュ: zipに .geojson が見つからない".to_string())
}

/// zip(生バイト列)→ メッシュ一覧。展開とJSON解析をリーダーで直結し、展開後の264MBは
/// メモリへ載せない。343属性のうち拾うのは33個だけで、残りは serde が読み飛ばす(§5.3)。
pub fn read_records(zip: &[u8]) -> Result<Vec<PopRecord>, String> {
    let e = find_geojson_entry(zip)?;
    let raw = &zip[e.offset..e.offset + e.csize];
    let dec = flate2::read::DeflateDecoder::new(raw);
    // 宣言された展開後サイズが嘘でも読み進めないよう、リーダー側でも上限を掛ける。
    let rdr = std::io::BufReader::new(dec.take(MAX_UNCOMPRESSED_BYTES));
    let fc: FeatureCollection =
        serde_json::from_reader(rdr).map_err(|err| format!("人口メッシュの解析: {err}"))?;
    Ok(fc.features.into_iter().map(Feature::into_record).collect())
}

/// 都道府県コード(1..=47)の500mメッシュ人口を取得する。zip取得 → 展開 → 解析まで。
/// 数秒〜数十秒かかる。呼び出しはワーカースレッドから(plotlayer が面倒を見る)。
pub fn fetch_prefecture(pref: u8) -> Result<Vec<PopMesh>, String> {
    if !(1..=47).contains(&pref) {
        return Err(format!("人口メッシュ: 都道府県コードが範囲外 {pref}"));
    }
    let zip = download_zip(pref)?;
    Ok(read_records(&zip)?.into_iter().map(|r| r.mesh).collect())
}

/// zipをまるごとメモリへ読む。zipの解析にはヘッダの先読みが要るのでここは載せる(最大31MB)。
pub fn download_zip(pref: u8) -> Result<Vec<u8>, String> {
    let url = zip_url(pref);
    let resp = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("人口メッシュ({}): {e}", pref_name(pref)))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_ZIP_BYTES)
        .read_to_end(&mut buf)
        .map_err(|e| format!("人口メッシュ({})の読み取り: {e}", pref_name(pref)))?;
    if buf.len() as u64 >= MAX_ZIP_BYTES {
        return Err(format!("人口メッシュ({}): zipが大きすぎる", pref_name(pref)));
    }
    Ok(buf)
}

// ---- GeoJSON の型(拾う属性だけを並べる) ----

#[derive(Deserialize)]
struct FeatureCollection {
    features: Vec<Feature>,
}

#[derive(Deserialize)]
struct Feature {
    // 幾何は読み飛ばす。頂点5点の配列を4万件ぶん確保しない(矩形は MESH_ID から作る・§2.4)。
    #[serde(default)]
    #[allow(dead_code)]
    geometry: IgnoredAny,
    properties: Props,
}

impl Feature {
    fn into_record(self) -> PopRecord {
        let p = self.properties;
        let pop = [
            p.ptn_2020.unwrap_or(0.0),
            p.ptn_2025.unwrap_or(0.0),
            p.ptn_2030.unwrap_or(0.0),
            p.ptn_2035.unwrap_or(0.0),
            p.ptn_2040.unwrap_or(0.0),
            p.ptn_2045.unwrap_or(0.0),
            p.ptn_2050.unwrap_or(0.0),
            p.ptn_2055.unwrap_or(0.0),
            p.ptn_2060.unwrap_or(0.0),
            p.ptn_2065.unwrap_or(0.0),
            p.ptn_2070.unwrap_or(0.0),
        ];
        // 年齢構成比は PT00_*(合算後)基準なので、秘匿処理で PT00 が0にされたメッシュでは
        // 比率が意味を持たない。そこは「データなし」= NaN にして塗らない(§2.5)。
        let aged_of = |rtc: Option<f32>, pt00: Option<f32>| match (rtc, pt00) {
            (Some(r), Some(t)) if t > 0.0 && r.is_finite() => r,
            _ => f32::NAN,
        };
        let aged = [
            aged_of(p.rtc_2025, p.pt00_2025),
            aged_of(p.rtc_2030, p.pt00_2030),
            aged_of(p.rtc_2035, p.pt00_2035),
            aged_of(p.rtc_2040, p.pt00_2040),
            aged_of(p.rtc_2045, p.pt00_2045),
            aged_of(p.rtc_2050, p.pt00_2050),
            aged_of(p.rtc_2055, p.pt00_2055),
            aged_of(p.rtc_2060, p.pt00_2060),
            aged_of(p.rtc_2065, p.pt00_2065),
            aged_of(p.rtc_2070, p.pt00_2070),
        ];
        PopRecord { shicode: p.shicode, mesh: PopMesh { mesh: p.mesh_id, pop, aged } }
    }
}

// 拾う属性は33個(MESH_ID / SHICODE / PTN×11 / PT00×10 / RTC×10)。343属性のうち残り310個は
// derive が割り当てずに読み飛ばすので、文字列にすらならない。
// 設計 §5.3 は「PTN×11 + RTC×10 の21個」としていたが、RTC を NaN にするかの判定に PT00 が要る
// (§2.5)ため PT00×10 を足してある。PT00 は構造体には残さない(判定に使って捨てる)。
#[derive(Deserialize)]
struct Props {
    #[serde(rename = "MESH_ID", deserialize_with = "de_code")]
    mesh_id: u32,
    #[serde(rename = "SHICODE", default, deserialize_with = "de_code")]
    shicode: u32,
    #[serde(rename = "PTN_2020", default)] ptn_2020: Option<f32>,
    #[serde(rename = "PTN_2025", default)] ptn_2025: Option<f32>,
    #[serde(rename = "PTN_2030", default)] ptn_2030: Option<f32>,
    #[serde(rename = "PTN_2035", default)] ptn_2035: Option<f32>,
    #[serde(rename = "PTN_2040", default)] ptn_2040: Option<f32>,
    #[serde(rename = "PTN_2045", default)] ptn_2045: Option<f32>,
    #[serde(rename = "PTN_2050", default)] ptn_2050: Option<f32>,
    #[serde(rename = "PTN_2055", default)] ptn_2055: Option<f32>,
    #[serde(rename = "PTN_2060", default)] ptn_2060: Option<f32>,
    #[serde(rename = "PTN_2065", default)] ptn_2065: Option<f32>,
    #[serde(rename = "PTN_2070", default)] ptn_2070: Option<f32>,
    #[serde(rename = "PT00_2025", default)] pt00_2025: Option<f32>,
    #[serde(rename = "PT00_2030", default)] pt00_2030: Option<f32>,
    #[serde(rename = "PT00_2035", default)] pt00_2035: Option<f32>,
    #[serde(rename = "PT00_2040", default)] pt00_2040: Option<f32>,
    #[serde(rename = "PT00_2045", default)] pt00_2045: Option<f32>,
    #[serde(rename = "PT00_2050", default)] pt00_2050: Option<f32>,
    #[serde(rename = "PT00_2055", default)] pt00_2055: Option<f32>,
    #[serde(rename = "PT00_2060", default)] pt00_2060: Option<f32>,
    #[serde(rename = "PT00_2065", default)] pt00_2065: Option<f32>,
    #[serde(rename = "PT00_2070", default)] pt00_2070: Option<f32>,
    #[serde(rename = "RTC_2025", default)] rtc_2025: Option<f32>,
    #[serde(rename = "RTC_2030", default)] rtc_2030: Option<f32>,
    #[serde(rename = "RTC_2035", default)] rtc_2035: Option<f32>,
    #[serde(rename = "RTC_2040", default)] rtc_2040: Option<f32>,
    #[serde(rename = "RTC_2045", default)] rtc_2045: Option<f32>,
    #[serde(rename = "RTC_2050", default)] rtc_2050: Option<f32>,
    #[serde(rename = "RTC_2055", default)] rtc_2055: Option<f32>,
    #[serde(rename = "RTC_2060", default)] rtc_2060: Option<f32>,
    #[serde(rename = "RTC_2065", default)] rtc_2065: Option<f32>,
    #[serde(rename = "RTC_2070", default)] rtc_2070: Option<f32>,
}

// MESH_ID / SHICODE は先頭ゼロを保つため文字列で入っているが、数値で来ても読めるようにする
// (SHICODE は "01234" のように先頭ゼロがあり、数値化されると桁が落ちる形式)。
fn de_code<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        S(String),
        N(u64),
        Null,
    }
    Ok(match Raw::deserialize(d)? {
        Raw::S(s) => s.trim().parse::<u32>().unwrap_or(0),
        Raw::N(n) => n as u32,
        Raw::Null => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 階級と密度 ----

    fn m(pop2025: f32) -> PopMesh {
        let mut pop = [0.0f32; YEARS.len()];
        pop[1] = pop2025; // 2025年
        PopMesh { mesh: 523351132, pop, aged: [f32::NAN; AGED_YEARS] }
    }

    #[test]
    fn density_is_four_times_the_mesh_population() {
        // 500mメッシュは0.25km²なので、メッシュ人口×4 が 人/km²。
        assert_eq!(density(&m(25.0), 1), Some(100.0));
        assert_eq!(density(&m(0.0), 1), Some(0.0));
        assert_eq!(density(&m(1.0), 99), None, "年次が範囲外");
    }

    #[test]
    fn class_boundaries_include_their_lower_edge() {
        // §7.4 の境界値: 100 / 1,000 / 4,000 / 10,000 / 20,000 人/km² の上下。
        let cases = [
            (24.9, 1),      // 99.6 人/km²
            (25.0, 2),      // 100 ちょうど → 2階級目
            (249.9, 2),     // 999.6
            (250.0, 3),     // 1,000 ちょうど
            (999.9, 3),     // 3,999.6
            (1000.0, 4),    // 4,000 ちょうど
            (2499.9, 4),    // 9,999.6
            (2500.0, 5),    // 10,000 ちょうど
            (4999.9, 5),    // 19,999.6
            (5000.0, 6),    // 20,000 ちょうど
            (15796.0, 6),   // 東京の最大値
        ];
        for (pop, want) in cases {
            assert_eq!(class_of(&m(pop), 1, Metric::Density), Some(want), "pop={pop}");
        }
    }

    #[test]
    fn zero_and_missing_population_are_not_painted() {
        assert_eq!(class_of(&m(0.0), 1, Metric::Density), None, "人口0は塗らない");
        assert_eq!(class_of(&m(-1.0), 1, Metric::Density), None, "負の値も塗らない");
        assert_eq!(class_of(&m(f32::NAN), 1, Metric::Density), None, "データなしは塗らない");
        assert_eq!(class_of(&m(100.0), 99, Metric::Density), None, "年次が無ければ塗らない");
    }

    #[test]
    fn stage_one_only_implements_the_density_metric() {
        // 高齢化率・増減率は型としては置くが Stage1 では描かない(§7.5)。
        assert!(class_of(&m(1000.0), 1, Metric::Aging).is_none());
        assert!(class_of(&m(1000.0), 1, Metric::Change).is_none());
    }

    #[test]
    fn class_colours_get_darker_and_more_opaque_as_the_density_rises() {
        let mut prev_alpha = 0u8;
        let mut prev_lum = 999i32;
        for c in 1..=6u8 {
            let col = class_color(c);
            assert!(col[3] > prev_alpha, "階級{c}のアルファが上がっていない");
            let lum = col[0] as i32 + col[1] as i32 + col[2] as i32;
            assert!(lum < prev_lum, "階級{c}が前より明るい");
            prev_alpha = col[3];
            prev_lum = lum;
        }
        // 設計 §7.4 の表そのまま。
        assert_eq!(class_color(1), [250, 232, 245, 40]);
        assert_eq!(class_color(4), [176, 92, 186, 160]);
        assert_eq!(class_color(6), [96, 20, 110, 230]);
        // 地図が読めなくならないよう、最も濃い階級でも完全不透明にはしない。
        assert!(class_color(6)[3] < 255);
    }

    #[test]
    fn year_index_covers_the_five_year_steps_only() {
        assert_eq!(year_index(2020), Some(0));
        assert_eq!(year_index(2025), Some(1));
        assert_eq!(year_index(2070), Some(10));
        assert_eq!(year_index(2021), None);
        assert_eq!(year_index(2075), None);
        assert_eq!(YEARS.len(), 11);
        assert_eq!(AGED_YEARS, 10, "2020年に年齢別の値は無い");
    }

    // ---- URL と都道府県名 ----

    #[test]
    fn zip_url_zero_pads_the_prefecture_code() {
        assert_eq!(
            zip_url(1),
            "https://nlftp.mlit.go.jp/ksj/gml/data/m500r6/m500r6-24/500m_mesh_2024_01_GEOJSON.zip"
        );
        assert!(zip_url(13).ends_with("500m_mesh_2024_13_GEOJSON.zip"));
        assert!(zip_url(47).ends_with("500m_mesh_2024_47_GEOJSON.zip"));
    }

    #[test]
    fn pref_name_maps_the_jis_codes() {
        assert_eq!(pref_name(1), "北海道");
        assert_eq!(pref_name(13), "東京都");
        assert_eq!(pref_name(31), "鳥取県");
        assert_eq!(pref_name(47), "沖縄県");
        assert_eq!(pref_name(0), "");
        assert_eq!(pref_name(48), "");
    }

    #[test]
    fn fetch_prefecture_refuses_a_code_outside_the_range_without_touching_the_network() {
        assert!(fetch_prefecture(0).is_err());
        assert!(fetch_prefecture(48).is_err());
    }

    // ---- 索引 ----

    fn test_index() -> HashMap<u32, Vec<u8>> {
        parse_index("533946,13,14\n523351,31\n404251,13\n# コメント\n\n643600,1\n")
    }

    #[test]
    fn parse_index_reads_multiple_prefectures_per_cell() {
        let idx = test_index();
        assert_eq!(idx.get(&533946), Some(&vec![13, 14]));
        assert_eq!(idx.get(&523351), Some(&vec![31]));
        assert_eq!(idx.len(), 4, "コメントと空行は行として数えない");
    }

    #[test]
    fn parse_index_drops_broken_lines_instead_of_failing() {
        let idx = parse_index("abc,13\n533946\n533947,\n533948,99\n533949,13\n,13\n");
        assert_eq!(idx.get(&533949), Some(&vec![13]));
        assert!(idx.get(&533948).is_none(), "都道府県コードの範囲外(99)は捨てる");
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn parse_index_merges_and_sorts_duplicate_cells() {
        let idx = parse_index("533946,14,13\n533946,13\n");
        assert_eq!(idx.get(&533946), Some(&vec![13, 14]));
    }

    #[test]
    fn prefectures_are_looked_up_by_mesh_not_by_bounding_box() {
        let idx = test_index();
        // 東京駅付近(2次メッシュ 533946)は 13 と 14 が出る。
        let tokyo = (35.68, 139.77, 35.68, 139.77);
        assert_eq!(prefectures_in(&idx, tokyo), vec![13, 14]);
        // 鳥取(523351)は 31 だけ。
        assert_eq!(prefectures_in(&idx, (35.09, 133.17, 35.09, 133.17)), vec![31]);
    }

    #[test]
    fn the_bounding_box_of_tokyo_does_not_drag_tokyo_into_okinawa() {
        // §4.1 の壊れるケース。東京都の外接矩形は小笠原・南鳥島を含むため
        // 緯度24.3〜35.9度・経度139.0〜154.0度に及び、沖縄本島(26.2,127.7)を覆ってしまう。
        // メッシュで引けばそうならない。
        let idx = test_index();
        let okinawa = (26.21, 127.68, 26.21, 127.68);
        assert!(!prefectures_in(&idx, okinawa).contains(&13), "沖縄を見て東京を取りに行っている");
        assert!(prefectures_in(&idx, okinawa).is_empty(), "索引に無いので何も取らない");
        // 逆に小笠原(父島・2次メッシュ404251)を見ているときは東京が出る。
        assert_eq!(prefectures_in(&idx, (27.09, 142.19, 27.09, 142.19)), vec![13]);
    }

    #[test]
    fn a_view_outside_japan_yields_no_prefecture() {
        let idx = test_index();
        assert!(prefectures_in(&idx, (48.85, 2.35, 48.86, 2.36)).is_empty()); // パリ
    }

    #[test]
    fn the_embedded_index_is_readable_and_only_holds_valid_codes() {
        // 生成物(data/mesh2pref.csv)が壊れていないことの最低限の確認。
        // 件数は版によって変わるので固定しない。
        for (cell, prefs) in index() {
            assert!((300_000..700_000).contains(cell), "2次メッシュコードとして不自然: {cell}");
            assert!(!prefs.is_empty());
            assert!(prefs.iter().all(|p| (1..=47).contains(p)), "cell={cell} prefs={prefs:?}");
            assert!(prefs.windows(2).all(|w| w[0] < w[1]), "昇順・重複なしのはず: {prefs:?}");
        }
    }

    // 生成物(data/mesh2pref.csv)が、実際に使える索引として成立していることの確認。
    // 件数そのものは推計の版が上がると変わるので固定しない。
    #[test]
    fn the_embedded_index_resolves_the_places_the_design_measured() {
        // 東京駅(2次メッシュ 533946)から東京が引ける。
        assert!(prefectures_for((35.68, 139.77, 35.68, 139.77)).contains(&13));
        // 鳥取市(523351)から鳥取が引ける。
        assert!(prefectures_for((35.09, 133.17, 35.09, 133.17)).contains(&31));
        // 札幌(北海道)。
        assert!(prefectures_for((43.06, 141.35, 43.06, 141.35)).contains(&1));
        // 那覇(沖縄)。ここで東京が出たら §4.1 の外接矩形の罠を踏んでいる。
        let okinawa = prefectures_for((26.21, 127.68, 26.21, 127.68));
        assert!(okinawa.contains(&47), "沖縄が引けない: {okinawa:?}");
        assert!(!okinawa.contains(&13), "沖縄を見て東京を取りに行っている: {okinawa:?}");
        // 小笠原・父島は東京都。離島も索引に入っている(外接矩形では区別できない側の確認)。
        assert!(prefectures_for((27.09, 142.19, 27.09, 142.19)).contains(&13));
        // 日本の外・海の上は何も引かない(原典にもデータが無い)。
        assert!(prefectures_for((48.85, 2.35, 48.86, 2.36)).is_empty(), "パリ");
        assert!(prefectures_for((30.0, 140.0, 30.01, 140.01)).is_empty(), "太平洋上");
    }

    // 県境の帯が欠けないよう、索引は過剰側に倒してある(跨るセルは全都道府県を載せる)。
    #[test]
    fn cells_on_a_prefecture_border_list_every_prefecture_they_touch() {
        let multi = index().values().filter(|v| v.len() > 1).count();
        assert!(multi > 100, "県境を跨ぐセルが少なすぎる(索引の作りが疑わしい): {multi}");
    }

    // ---- zip ----

    // 最小のzip(ローカルヘッダ1件ぶん)を組む。中央ディレクトリは付けない
    // (find_geojson_entry は署名が変わった時点で打ち切るので、無くても同じ経路を通る)。
    fn zip_entry_bytes(name: &str, method: u16, flags: u16, data: &[u8], usize_: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&LOCAL_HEADER_SIG.to_le_bytes());
        v.extend_from_slice(&20u16.to_le_bytes()); // version
        v.extend_from_slice(&flags.to_le_bytes());
        v.extend_from_slice(&method.to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes()); // time
        v.extend_from_slice(&0u16.to_le_bytes()); // date
        v.extend_from_slice(&0u32.to_le_bytes()); // crc32
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&usize_.to_le_bytes());
        v.extend_from_slice(&(name.len() as u16).to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes()); // extra len
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(data);
        v
    }

    // 生 deflate(zlibヘッダ無し)へ圧縮する。実データと同じ method=8 の中身になる。
    fn deflate(raw: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(raw).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn the_local_header_of_the_geojson_entry_is_read() {
        let body = deflate(b"{}");
        let zip = zip_entry_bytes("500m_mesh_2024_31.geojson", 8, 0, &body, 2);
        let e = find_geojson_entry(&zip).unwrap();
        assert_eq!(e.name, "500m_mesh_2024_31.geojson");
        assert_eq!(e.method, 8);
        assert_eq!(e.csize, body.len());
        assert_eq!(e.usize_, 2);
        assert_eq!(&zip[e.offset..e.offset + e.csize], &body[..]);
    }

    #[test]
    fn entries_before_the_geojson_are_skipped() {
        // 実ファイルは ディレクトリ / .geojson / .xml の3エントリ。先頭のディレクトリを飛ばす。
        let dir = zip_entry_bytes("500m_mesh_2024_31_GEOJSON/", 0, 0, b"", 0);
        let body = deflate(b"{}");
        let geo = zip_entry_bytes("500m_mesh_2024_31_GEOJSON/x.geojson", 8, 0, &body, 2);
        let mut zip = dir.clone();
        zip.extend_from_slice(&geo);
        let e = find_geojson_entry(&zip).unwrap();
        assert!(e.name.ends_with("x.geojson"));
        assert_eq!(e.offset, dir.len() + 30 + "500m_mesh_2024_31_GEOJSON/x.geojson".len());
    }

    #[test]
    fn a_non_deflate_geojson_entry_is_refused() {
        let zip = zip_entry_bytes("x.geojson", 0, 0, b"{}", 2); // method=0(無圧縮)
        let err = find_geojson_entry(&zip).unwrap_err();
        assert!(err.contains("未対応のzip圧縮方式"), "{err}");
    }

    #[test]
    fn an_oversized_uncompressed_entry_is_refused() {
        let body = deflate(b"{}");
        // 展開後 1GB を名乗るエントリ(zip爆弾)。上限512MBで断る。
        let zip = zip_entry_bytes("x.geojson", 8, 0, &body, 1024 * 1024 * 1024);
        let err = find_geojson_entry(&zip).unwrap_err();
        assert!(err.contains("大きすぎる"), "{err}");
    }

    #[test]
    fn a_data_descriptor_entry_is_refused_instead_of_looping() {
        let zip = zip_entry_bytes("x.geojson", 8, FLAG_DATA_DESCRIPTOR, b"", 0);
        assert!(find_geojson_entry(&zip).unwrap_err().contains("データディスクリプタ"));
        // .geojson でないエントリでも、csize=0 で位置が進まないので打ち切る(無限ループ防止)。
        let other = zip_entry_bytes("x.xml", 8, FLAG_DATA_DESCRIPTOR, b"", 0);
        assert!(find_geojson_entry(&other).is_err());
    }

    #[test]
    fn a_zip_without_any_geojson_is_an_error() {
        let zip = zip_entry_bytes("KS-META-x.xml", 8, 0, &deflate(b"<xml/>"), 6);
        assert!(find_geojson_entry(&zip).unwrap_err().contains(".geojson が見つからない"));
        assert!(find_geojson_entry(b"").is_err());
        assert!(find_geojson_entry(b"not a zip at all").is_err());
    }

    #[test]
    fn a_truncated_zip_is_an_error_not_a_panic() {
        let body = deflate(b"{}");
        let zip = zip_entry_bytes("x.geojson", 8, 0, &body, 2);
        for cut in [4usize, 20, 30, 35, zip.len() - 1] {
            assert!(find_geojson_entry(&zip[..cut]).is_err(), "cut={cut}");
        }
    }

    // ---- GeoJSON の解析 ----

    // 実ファイルから切り出した形の断片(属性は代表的なものだけ・未知の属性を混ぜてある)。
    // 1件目=通常のメッシュ / 2件目=秘匿対象(HITOKU='*' で PT00 が0にされている)。
    const SAMPLE: &str = r#"{
      "type": "FeatureCollection",
      "name": "500m_mesh_2024_31",
      "crs": { "type": "name", "properties": { "name": "urn:ogc:def:crs:EPSG::6668" } },
      "features": [
        { "type": "Feature",
          "properties": {
            "MESH_ID": "523351151", "SHICODE": "31201",
            "PTN_2020": 30.0, "PTN_2025": 28.5645, "PTN_2030": 27.0, "PTN_2035": 26.0,
            "PTN_2040": 25.0, "PTN_2045": 24.0, "PTN_2050": 23.0, "PTN_2055": 22.0,
            "PTN_2060": 21.0, "PTN_2065": 20.0, "PTN_2070": 19.0,
            "PT00_2025": 30.3039, "PT00_2030": 29.0, "PT00_2035": 28.0, "PT00_2040": 27.0,
            "PT00_2045": 26.0, "PT00_2050": 25.0, "PT00_2055": 24.0, "PT00_2060": 23.0,
            "PT00_2065": 22.0, "PT00_2070": 21.0,
            "RTC_2025": 33.5, "RTC_2030": 35.0, "RTC_2035": 36.0, "RTC_2040": 37.0,
            "RTC_2045": 38.0, "RTC_2050": 39.0, "RTC_2055": 40.0, "RTC_2060": 41.0,
            "RTC_2065": 42.0, "RTC_2070": 43.0,
            "HITOKU2025": "@", "GASSAN2025": null,
            "PT01_2025": 1.0, "PT20_2025": 0.5, "RTA_2025": 10.0, "PTA_2025": 3.0
          },
          "geometry": { "type": "Polygon", "coordinates": [[[133.1,35.0],[133.2,35.0],[133.2,35.1],[133.1,35.1],[133.1,35.0]]] }
        },
        { "type": "Feature",
          "properties": {
            "MESH_ID": "523351153", "SHICODE": "31201",
            "PTN_2020": 2.0, "PTN_2025": 1.7394,
            "PT00_2025": 0.0,
            "RTC_2025": 0.0,
            "HITOKU2025": "*", "GASSAN2025": "523351151"
          },
          "geometry": { "type": "Polygon", "coordinates": [[[133.1,35.0],[133.2,35.0],[133.2,35.1],[133.1,35.1],[133.1,35.0]]] }
        }
      ]
    }"#;

    fn sample_zip() -> Vec<u8> {
        let body = deflate(SAMPLE.as_bytes());
        zip_entry_bytes("500m_mesh_2024_31.geojson", 8, 0, &body, SAMPLE.len() as u32)
    }

    #[test]
    fn the_designed_attributes_are_read_in_year_order() {
        let recs = read_records(&sample_zip()).unwrap();
        assert_eq!(recs.len(), 2);
        let a = &recs[0];
        assert_eq!(a.mesh.mesh, 523351151);
        assert_eq!(a.shicode, 31201);
        assert_eq!(a.mesh.pop[0], 30.0, "PTN_2020");
        assert_eq!(a.mesh.pop[1], 28.5645, "PTN_2025(合算前を使う)");
        assert_eq!(a.mesh.pop[10], 19.0, "PTN_2070");
        assert_eq!(a.mesh.aged[0], 33.5, "RTC_2025");
        assert_eq!(a.mesh.aged[9], 43.0, "RTC_2070");
    }

    #[test]
    fn unknown_attributes_are_skipped_without_failing() {
        // PT01/PT20/RTA/PTA/HITOKU/GASSAN 等、拾わない属性が混ざっていても読める。
        assert!(read_records(&sample_zip()).is_ok());
    }

    #[test]
    fn a_suppressed_mesh_keeps_its_own_population_but_loses_its_age_ratio() {
        let recs = read_records(&sample_zip()).unwrap();
        let b = &recs[1];
        assert_eq!(b.mesh.mesh, 523351153);
        // PTN(合算前)はそのメッシュ自身の値なので残る。ここが PT00 だと 0 になり、
        // 「人がいない帯」が実際とは違う場所に出る(§2.5)。
        assert_eq!(b.mesh.pop[1], 1.7394);
        // PT00=0 の年は年齢構成比が意味を持たないので NaN。
        assert!(b.mesh.aged[0].is_nan(), "秘匿対象で NaN になっていない");
        // 値が無い年も NaN(0% と区別する)。
        assert!(b.mesh.aged[5].is_nan());
        // 値の無い年次の人口は 0(=塗らない)。
        assert_eq!(b.mesh.pop[10], 0.0);
    }

    #[test]
    fn a_mesh_id_given_as_a_number_is_also_accepted() {
        let json = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"MESH_ID":523351151,"SHICODE":31201,"PTN_2025":1.0},
             "geometry":null}]}"#;
        let zip = zip_entry_bytes("x.geojson", 8, 0, &deflate(json.as_bytes()), json.len() as u32);
        let recs = read_records(&zip).unwrap();
        assert_eq!(recs[0].mesh.mesh, 523351151);
        assert_eq!(recs[0].shicode, 31201);
    }

    #[test]
    fn broken_json_inside_the_zip_is_an_error_not_a_panic() {
        let bad = b"{ this is not json";
        let zip = zip_entry_bytes("x.geojson", 8, 0, &deflate(bad), bad.len() as u32);
        assert!(read_records(&zip).unwrap_err().contains("人口メッシュの解析"));
    }

    #[test]
    fn the_geometry_is_ignored_so_the_rectangle_comes_from_the_mesh_id() {
        // SAMPLE の geometry は実際の矩形とはまったく違う座標だが、描画に使うのは MESH_ID から
        // 作った矩形なので影響しない。
        let recs = read_records(&sample_zip()).unwrap();
        let (s, w, n, e) = mesh::half_mesh_bbox(recs[0].mesh.mesh);
        assert!((n - s - 1.0 / 240.0).abs() < 1e-12);
        assert!((e - w - 1.0 / 160.0).abs() < 1e-12);
        assert!(w > 133.1 && w < 133.2, "鳥取県内のはず: w={w}");
    }

    // ---- ディスクキャッシュ(plotcache)を通す往復 ----

    #[test]
    fn a_pop_mesh_round_trips_through_json_with_nan_as_null() {
        let mut aged = [1.5f32; AGED_YEARS];
        aged[3] = f32::NAN;
        let m = PopMesh { mesh: 523351132, pop: [1.0; YEARS.len()], aged };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("null"), "NaN は null として書く: {json}");
        let back: PopMesh = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mesh, m.mesh);
        assert_eq!(back.pop, m.pop);
        assert!(back.aged[3].is_nan(), "null は NaN として読み戻す");
        assert_eq!(back.aged[0], 1.5);
    }

    // 実データが要る確認(手順として残す・§12)。
    // 実行は `cargo test --release -- --ignored --nocapture population::tests::live_`。
    #[test]
    #[ignore = "ネットワークと数十MBの取得が要る"]
    fn live_tottori_has_the_measured_number_of_meshes() {
        let got = fetch_prefecture(31).expect("鳥取(31)の取得");
        assert_eq!(got.len(), 4083, "設計 §2.2 の実測値と件数が違う");
        assert!(got.iter().all(|m| (500_000_000..600_000_000).contains(&m.mesh)));
    }

    #[test]
    #[ignore = "ネットワークと30MBの取得が要る(最悪ケース)"]
    fn live_hokkaido_is_the_worst_case() {
        let got = fetch_prefecture(1).expect("北海道(01)の取得");
        assert_eq!(got.len(), 40544, "設計 §2.2 の実測値と件数が違う");
    }
}
