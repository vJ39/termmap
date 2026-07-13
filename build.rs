// sound.rs が Core Audio(AudioServicesPlaySystemSound)をFFIで使うためのフレームワークリンク。
// crate依存の追加ではなく、macOS標準のシステムフレームワークをリンクするだけ。
fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" {
        println!("cargo:rustc-link-lib=framework=AudioToolbox");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
    }
}
