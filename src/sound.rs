//! 操作UIの効果音。macOS の afplay に委譲して短い WAV を鳴らす。
//! 依存追加なし。非macOS / afplay不在 / 無効時は完全に no-op。
//!
//! 再生はワーカースレッド1本に channel で sfx名(&'static str)を送るだけ。
//! afplay の子プロセスは status() で待って reap する(＝ゾンビ化させない)。
//! play() は送信するだけなので UI をブロックしない。

use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

// 埋め込む効果音。名前 = play() で指定する識別子。tmp へ <name>.wav で書き出す。
const SFX: &[(&str, &[u8])] = &[
    ("pop", include_bytes!("../assets/sfx/sfx_pop.wav")),
    ("blip", include_bytes!("../assets/sfx/sfx_blip.wav")),
    ("error", include_bytes!("../assets/sfx/sfx_error.wav")),
    ("confirm", include_bytes!("../assets/sfx/sfx_confirm.wav")),
    ("back", include_bytes!("../assets/sfx/sfx_back.wav")),
    ("click", include_bytes!("../assets/sfx/sfx_click.wav")),
];

pub struct Sound {
    tx: Option<Sender<&'static str>>,
}

impl Sound {
    /// 効果音プレイヤーを作る。
    /// enabled=false / 非macOS / afplay不在 のいずれかなら no-op(tx=None)を返す。
    pub fn new(enabled: bool) -> Sound {
        if !enabled || !cfg!(target_os = "macos") {
            return Sound { tx: None };
        }
        if !afplay_available() {
            return Sound { tx: None };
        }
        // 埋め込みWAVを一時ディレクトリへ起動時に一度だけ書き出す。
        let dir = std::env::temp_dir().join("termmap-sfx");
        if std::fs::create_dir_all(&dir).is_err() {
            return Sound { tx: None };
        }
        for &(name, bytes) in SFX {
            let _ = std::fs::write(dir.join(format!("{name}.wav")), bytes);
        }
        // ワーカースレッド1本: channel から sfx名を受け、afplay で鳴らす。
        // 連打で送信済みキューが伸びると、操作をやめた後もSEが鳴り続ける("スタック")ので、
        // recv後にキューへ溜まった分を全部先読みして捨て、常に最新の1件だけ再生する。
        let (tx, rx) = std::sync::mpsc::channel::<&'static str>();
        std::thread::spawn(move || {
            // 全 Sender が drop されると recv が Err を返す → ループ終了(スレッド後始末)。
            while let Ok(mut name) = rx.recv() {
                while let Ok(next) = rx.try_recv() { name = next; }
                let path = dir.join(format!("{name}.wav"));
                // status() で子プロセスを待って回収する = ゾンビ化させない。失敗は無視。
                let _ = Command::new("afplay")
                    .arg(&path)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        });
        Sound { tx: Some(tx) }
    }

    /// sfx名を鳴らす。無効時(tx=None)や送信失敗時は何もしない(UIをブロックしない)。
    pub fn play(&self, name: &'static str) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(name);
        }
    }
}

// PATH 上の afplay が起動できるか判定する。標準パスの存在を先に見て、無ければ実起動を試す。
fn afplay_available() -> bool {
    if std::path::Path::new("/usr/bin/afplay").exists() {
        return true;
    }
    Command::new("afplay")
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}
