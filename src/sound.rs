//! 操作UIの効果音。macOS の Core Audio(AudioServicesPlaySystemSound)で鳴らす。
//! crate依存追加なし(build.rsでAudioToolbox/CoreFoundationフレームワークをリンクするのみ)。
//!
//! 起動時に各WAVを一時ファイルへ書き出し、SystemSoundIDとして事前登録しておく(＝メモリに保持)。
//! play()は登録済みIDを渡してAudioServicesPlaySystemSoundを呼ぶだけで、都度プロセスを起動する
//! afplay方式と違って起動オーバーヘッドが無く低レイテンシ。呼び出しは即座に返り重複再生も自然に
//! 許容される(前の音の再生完了を待たない)。

// 埋め込む効果音。名前 = play() で指定する識別子。tmp へ <name>.wav で書き出す。
const SFX: &[(&str, &[u8])] = &[
    ("pop", include_bytes!("../assets/sfx/sfx_pop.wav")),
    ("blip", include_bytes!("../assets/sfx/sfx_blip.wav")),
    ("error", include_bytes!("../assets/sfx/sfx_error.wav")),
    ("confirm", include_bytes!("../assets/sfx/sfx_confirm.wav")),
    ("back", include_bytes!("../assets/sfx/sfx_back.wav")),
    ("click", include_bytes!("../assets/sfx/sfx_click.wav")),
];

#[cfg(target_os = "macos")]
mod coreaudio {
    use std::os::raw::c_void;

    pub type CFAllocatorRef = *const c_void;
    pub type CFUrlRef = *const c_void;
    pub type CfIndex = isize;
    pub type Boolean = u8;
    pub type OsStatus = i32;
    pub type SystemSoundId = u32;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub static kCFAllocatorDefault: CFAllocatorRef;
        pub fn CFURLCreateFromFileSystemRepresentation(
            allocator: CFAllocatorRef,
            buffer: *const u8,
            buf_len: CfIndex,
            is_directory: Boolean,
        ) -> CFUrlRef;
        pub fn CFRelease(cf: *const c_void);
    }

    #[link(name = "AudioToolbox", kind = "framework")]
    extern "C" {
        pub fn AudioServicesCreateSystemSoundID(
            in_file_url: CFUrlRef,
            out_system_sound_id: *mut SystemSoundId,
        ) -> OsStatus;
        pub fn AudioServicesPlaySystemSound(in_system_sound_id: SystemSoundId);
    }

    /// WAVファイルをSystemSoundIDとして登録する。失敗時はNone(呼び出し側で無視する)。
    pub fn register(path: &std::path::Path) -> Option<SystemSoundId> {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        unsafe {
            let url = CFURLCreateFromFileSystemRepresentation(
                kCFAllocatorDefault,
                bytes.as_ptr(),
                bytes.len() as CfIndex,
                0,
            );
            if url.is_null() {
                return None;
            }
            let mut id: SystemSoundId = 0;
            let status = AudioServicesCreateSystemSoundID(url, &mut id);
            CFRelease(url);
            if status == 0 { Some(id) } else { None }
        }
    }
}

pub struct Sound {
    #[cfg(target_os = "macos")]
    ids: Vec<(&'static str, coreaudio::SystemSoundId)>,
    enabled: bool,
}

impl Sound {
    /// 効果音プレイヤーを作る。enabled=false / 非macOS / 登録失敗時はno-opになる。
    pub fn new(enabled: bool) -> Sound {
        #[cfg(target_os = "macos")]
        {
            if !enabled {
                return Sound { ids: Vec::new(), enabled: false };
            }
            let dir = std::env::temp_dir().join("termmap-sfx");
            if std::fs::create_dir_all(&dir).is_err() {
                return Sound { ids: Vec::new(), enabled: false };
            }
            let mut ids = Vec::new();
            for &(name, bytes) in SFX {
                let path = dir.join(format!("{name}.wav"));
                if std::fs::write(&path, bytes).is_err() {
                    continue;
                }
                if let Some(id) = coreaudio::register(&path) {
                    ids.push((name, id));
                }
            }
            let ok = !ids.is_empty();
            return Sound { ids, enabled: ok };
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = enabled;
            Sound { enabled: false }
        }
    }

    /// sfx名を鳴らす。無効時や未登録時は何もしない(UIをブロックしない・呼び出しは即座に返る)。
    pub fn play(&self, name: &'static str) {
        if !self.enabled {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(&(_, id)) = self.ids.iter().find(|(n, _)| *n == name) {
                unsafe { coreaudio::AudioServicesPlaySystemSound(id) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_registers_all_sfx_and_play_does_not_panic() {
        let snd = Sound::new(true);
        // CI/ヘッドレス環境でも音声デバイス有無に関わらずクラッシュしないことを確認する。
        #[cfg(target_os = "macos")]
        assert_eq!(snd.ids.len(), SFX.len(), "全SFXがSystemSoundIDとして登録できること");
        for &(name, _) in SFX {
            snd.play(name); // 音は聞こえなくてよい。panicしないことだけ確認
        }
        snd.play("存在しない名前"); // 未登録名はno-op
    }

    #[test]
    fn disabled_is_noop() {
        let snd = Sound::new(false);
        snd.play("pop"); // no-opでpanicしない
    }
}
