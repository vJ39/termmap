// CoreLocationCLI をポーリングして現在地をライブ取得する背景スレッド。
// std のみ・外部crate禁止・crate::参照禁止（単体コンパイル可能）。

use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

// CoreLocationCLI の stdout をパースし、最初に現れる妥当な (lat, lon) を返す。
// 想定フォーマット例:
//   "35.681 139.767"
//   "latitude: 35.681 longitude: 139.767"
// トークンを前から順に走査し、lat候補(-90..=90)の直後に続くlon候補(-180..=180)を採用する。
pub fn parse_location(out: &str) -> Option<(f64, f64)> {
    // 数値らしきトークンだけを抜き出す（ラベル文字列やカンマ等は無視）。
    let nums: Vec<f64> = out
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .filter(|s| !s.is_empty() && *s != "-" && *s != "+" && *s != ".")
        .filter_map(|s| s.parse::<f64>().ok())
        .collect();

    for i in 0..nums.len() {
        let lat = nums[i];
        if !(-90.0..=90.0).contains(&lat) {
            continue;
        }
        if let Some(&lon) = nums.get(i + 1) {
            if (-180.0..=180.0).contains(&lon) {
                return Some((lat, lon));
            }
        }
    }
    None
}

// interval_secs ごとに command を実行し、パースに成功した位置だけ channel へ送る背景スレッドを起動する。
// 受信側が Receiver を drop したら次の send で失敗しスレッドは終了する。
pub fn start_poller(command: String, interval_secs: u64) -> Receiver<(f64, f64)> {
    let (tx, rx) = mpsc::channel::<(f64, f64)>();
    thread::spawn(move || loop {
        // gps_here と同じフォーマット指定("lat lon"の1行)で叩き、parse_location が確実に拾えるようにする。
        let output = Command::new(&command).args(["--format", "%latitude %longitude"]).output();
        if let Ok(out) = output {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                if let Some(loc) = parse_location(&text) {
                    if tx.send(loc).is_err() {
                        return; // 受信側drop→終了
                    }
                }
            }
        }
        thread::sleep(Duration::from_secs(interval_secs));
    });
    rx
}

// command が実行可能かどうかを --version の起動可否で判定する。
pub fn available(command: &str) -> bool {
    Command::new(command).arg("--version").output().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_labeled_format() {
        let got = parse_location("latitude: 35.681 longitude: 139.767");
        assert_eq!(got, Some((35.681, 139.767)));
    }

    #[test]
    fn parse_bare_format() {
        let got = parse_location("35.681 139.767");
        assert_eq!(got, Some((35.681, 139.767)));
    }

    #[test]
    fn parse_negative_coords() {
        let got = parse_location("latitude: -33.868 longitude: 151.209");
        assert_eq!(got, Some((-33.868, 151.209)));
    }

    #[test]
    fn parse_out_of_range_returns_none() {
        // lat候補が範囲外(999)→次のペアも作れない
        assert_eq!(parse_location("999.0 139.767"), None);
    }

    #[test]
    fn parse_garbage_returns_none() {
        assert_eq!(parse_location("\u{0}\u{1}garbled???"), None);
        assert_eq!(parse_location(""), None);
        assert_eq!(parse_location("no numbers here"), None);
    }

    #[test]
    fn parse_lon_out_of_range_skips_and_finds_next_pair() {
        // 最初の(lat候補, lon候補)が範囲外でも、後続に妥当なペアがあれば拾う。
        // "50.0 300.0 35.681 139.767" -> i=0: lat=50 ok, lon=300 NG(範囲外) -> continue
        //                                  i=2: lat=35.681 ok, lon=139.767 ok -> Some
        let got = parse_location("50.0 300.0 35.681 139.767");
        assert_eq!(got, Some((35.681, 139.767)));
    }
}
