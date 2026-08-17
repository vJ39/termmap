// 2次メッシュ(6桁) → 都道府県コード の索引 data/mesh2pref.csv を作る開発用ツール。
// 設計は docs/population-mesh-overlay-design.md §4.2。
//
// 47都道府県の500mメッシュ人口(合計約480MB)を1件ずつ直列に取得し、各 feature が持つ
// MESH_ID の上6桁(2次メッシュ)と SHICODE の上2桁(都道府県)の直積を集めて重複を除く。
// 都道府県境を跨ぐ2次メッシュは複数の都道府県を持つので、索引は過剰側に倒す
// (取りすぎは通信の無駄で済むが、取りこぼすと県境の帯がまるごと欠ける)。
//
// 再生成が要るのは推計の版が上がったとき(数年に1度)だけ。配信元への負荷を考えて並列化はしない。
//
//   cargo run --release --bin gen-mesh2pref -- [出力先]        # 既定 data/mesh2pref.csv
//   cargo run --release --bin gen-mesh2pref -- out.csv 1 13 31 # 都道府県を絞る(動作確認用)
//
// 本体(termmap)とはモジュールを共有できない(バイナリクレートは別クレートになる)ので、
// 必要な2ファイルだけを #[path] で取り込む。population.rs が crate:: 参照を mesh だけに
// 留めてあるのはこのため。

#![allow(dead_code)] // 取り込んだモジュールのうち、索引生成に使わない関数は当然使われない

#[path = "../mesh.rs"]
mod mesh;
#[path = "../population.rs"]
mod population;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out_path = args.first().cloned().unwrap_or_else(|| "data/mesh2pref.csv".to_string());
    let prefs: Vec<u8> = if args.len() > 1 {
        args[1..].iter().filter_map(|a| a.parse::<u8>().ok()).collect()
    } else {
        (1..=47).collect()
    };

    let mut index: BTreeMap<u32, BTreeSet<u8>> = BTreeMap::new();
    let mut failed: Vec<u8> = Vec::new();
    for pref in &prefs {
        let pref = *pref;
        eprint!("{:02} {} … ", pref, population::pref_name(pref));
        let started = std::time::Instant::now();
        let zip = match population::download_zip(pref) {
            Ok(z) => z,
            Err(e) => {
                eprintln!("取得失敗: {e}");
                failed.push(pref);
                continue;
            }
        };
        let records = match population::read_records(&zip) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("解析失敗: {e}");
                failed.push(pref);
                continue;
            }
        };
        let mut cells = 0usize;
        for r in &records {
            // SHICODE(行政区域コード5桁)の上2桁が都道府県コード。データ側が空(0)の場合は、
            // ファイル1本=1都道府県なので取得に使った番号で補う。
            let from_data = (r.shicode / 1000) as u8;
            let p = if (1..=47).contains(&from_data) { from_data } else { pref };
            if index.entry(r.mesh.mesh / 1_000).or_default().insert(p) {
                cells += 1;
            }
        }
        eprintln!(
            "{}件 / 新規{}行 / {:.1}秒 / zip {:.1}MB",
            records.len(),
            cells,
            started.elapsed().as_secs_f64(),
            zip.len() as f64 / 1_048_576.0
        );
    }

    let mut body = String::new();
    body.push_str("# 2次メッシュ(6桁) → 都道府県コード(2桁・複数可)の索引。\n");
    body.push_str("# src/bin/gen-mesh2pref.rs が国土数値情報の500mメッシュデータから生成する(数年に1度の再生成)。\n");
    body.push_str(&format!("# 版: {} / {}行\n", population::DATASET_DIR, index.len()));
    for (cell, prefs) in &index {
        body.push_str(&cell.to_string());
        for p in prefs {
            body.push(',');
            body.push_str(&p.to_string());
        }
        body.push('\n');
    }
    if let Err(e) = write_out(&out_path, &body) {
        eprintln!("書き出し失敗 {out_path}: {e}");
        std::process::exit(1);
    }

    let multi = index.values().filter(|v| v.len() > 1).count();
    eprintln!(
        "{out_path}: {}行 / {}バイト / 都道府県境を跨ぐ2次メッシュ {}件",
        index.len(),
        body.len(),
        multi
    );
    if !failed.is_empty() {
        eprintln!("取得できなかった都道府県: {failed:?}(索引が欠けるので必ずやり直すこと)");
        std::process::exit(1);
    }
}

fn write_out(path: &str, body: &str) -> std::io::Result<()> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(body.as_bytes())
}
