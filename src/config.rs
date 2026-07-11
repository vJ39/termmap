//! Self-contained configuration module for termmap.
//!
//! Standard library only, no external crates, no `crate::` references —
//! this file is designed to be compiled and tested on its own with:
//!
//!     rustc --edition 2021 --test src/config.rs -o /tmp/tm_config_test && /tmp/tm_config_test
//!
//! Config file format is a minimal, hand-rolled TOML subset (no `toml`
//! crate dependency):
//!
//!     [llm]
//!     recommend_enabled = true
//!     model = "claude-sonnet-5"
//!     command = "claude"
//!
//!     [route]
//!     profile = "car-fast"
//!     sample_interval_m = 800.0
//!
//!     [display]
//!     style = "osm"
//!     show_spots = true
//!
//! Values are plain `true`/`false`, bare numbers, or `"quoted strings"`.
//! Unknown lines/sections/keys are ignored. A missing or unreadable file
//! yields `Config::default()`; a partially malformed file keeps whichever
//! keys parsed successfully and leaves the rest at their default values.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub llm_recommend_enabled: bool,
    pub llm_model: String,
    pub llm_command: String,
    pub route_profile: String,
    pub sample_interval_m: f64,
    pub style: String,
    pub show_spots: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            llm_recommend_enabled: true,
            llm_model: "claude-sonnet-5".to_string(),
            llm_command: "claude".to_string(),
            route_profile: "car-fast".to_string(),
            sample_interval_m: 800.0,
            style: "osm".to_string(),
            show_spots: true,
        }
    }
}

/// Returns `$HOME/.config/termmap/config.toml`, or `None` if `HOME` is unset.
pub fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("termmap");
    p.push("config.toml");
    Some(p)
}

/// Loads a `Config` from `path`. Missing or unreadable files yield
/// `Config::default()`. Recognized keys found in the file override the
/// corresponding default field; everything else (unknown keys/sections,
/// malformed values) is skipped and leaves that field at its default.
pub fn load_config_from(path: &Path) -> Config {
    let mut cfg = Config::default();

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return cfg,
    };

    let mut section = String::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }

        let mut parts = line.splitn(2, '=');
        let key = match parts.next() {
            Some(k) => k.trim(),
            None => continue,
        };
        let value = match parts.next() {
            Some(v) => v.trim(),
            None => continue,
        };

        match (section.as_str(), key) {
            ("llm", "recommend_enabled") => {
                if let Some(b) = parse_bool(value) {
                    cfg.llm_recommend_enabled = b;
                }
            }
            ("llm", "model") => {
                if let Some(s) = parse_string(value) {
                    cfg.llm_model = s;
                }
            }
            ("llm", "command") => {
                if let Some(s) = parse_string(value) {
                    cfg.llm_command = s;
                }
            }
            ("route", "profile") => {
                if let Some(s) = parse_string(value) {
                    cfg.route_profile = s;
                }
            }
            ("route", "sample_interval_m") => {
                if let Some(f) = parse_number(value) {
                    cfg.sample_interval_m = f;
                }
            }
            ("display", "style") => {
                if let Some(s) = parse_string(value) {
                    cfg.style = s;
                }
            }
            ("display", "show_spots") => {
                if let Some(b) = parse_bool(value) {
                    cfg.show_spots = b;
                }
            }
            _ => {}
        }
    }

    cfg
}

/// Serializes `c` to `path` in the minimal TOML subset described above,
/// creating any missing parent directories first.
pub fn save_config_to(path: &Path, c: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create dir {}: {}", parent.display(), e))?;
        }
    }

    let contents = format!(
        "[llm]\n\
         recommend_enabled = {}\n\
         model = \"{}\"\n\
         command = \"{}\"\n\
         \n\
         [route]\n\
         profile = \"{}\"\n\
         sample_interval_m = {}\n\
         \n\
         [display]\n\
         style = \"{}\"\n\
         show_spots = {}\n",
        c.llm_recommend_enabled,
        c.llm_model,
        c.llm_command,
        c.route_profile,
        c.sample_interval_m,
        c.style,
        c.show_spots,
    );

    std::fs::write(path, contents).map_err(|e| format!("failed to write {}: {}", path.display(), e))
}

/// Loads the config from the standard location (`config_path()`), or
/// `Config::default()` if the location cannot be determined.
pub fn load_config() -> Config {
    match config_path() {
        Some(p) => load_config_from(&p),
        None => Config::default(),
    }
}

/// Saves `c` to the standard location (`config_path()`).
pub fn save_config(c: &Config) -> Result<(), String> {
    match config_path() {
        Some(p) => save_config_to(&p, c),
        None => Err("could not determine config path (HOME not set)".to_string()),
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_string(v: &str) -> Option<String> {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        Some(v[1..v.len() - 1].to_string())
    } else {
        None
    }
}

fn parse_number(v: &str) -> Option<f64> {
    v.trim().parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Generates a unique path under the OS temp dir so tests never touch
    /// the real `$HOME/.config/termmap/config.toml` and never collide with
    /// each other when run in parallel.
    fn unique_temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let mut p = std::env::temp_dir();
        p.push(format!("termmap_config_test_{}_{}_{}.toml", tag, pid, n));
        p
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        if let Some(parent) = path.parent() {
            // best-effort: only removes if empty and it's one of our test dirs
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[test]
    fn default_values_match_spec() {
        let c = Config::default();
        assert_eq!(c.llm_recommend_enabled, true);
        assert_eq!(c.llm_model, "claude-sonnet-5");
        assert_eq!(c.llm_command, "claude");
        assert_eq!(c.route_profile, "car-fast");
        assert_eq!(c.sample_interval_m, 800.0);
        assert_eq!(c.style, "osm");
        assert_eq!(c.show_spots, true);
    }

    #[test]
    fn config_path_uses_home_and_expected_suffix() {
        match config_path() {
            Some(p) => {
                assert!(p.ends_with(".config/termmap/config.toml"));
            }
            None => {
                // Only acceptable if HOME really is unset in this environment.
                assert!(std::env::var_os("HOME").is_none());
            }
        }
    }

    #[test]
    fn missing_file_returns_default() {
        let path = unique_temp_path("missing");
        // Ensure it really doesn't exist.
        let _ = std::fs::remove_file(&path);
        let cfg = load_config_from(&path);
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn round_trip_default_via_save_and_load() {
        let path = unique_temp_path("roundtrip_default");
        let original = Config::default();
        save_config_to(&path, &original).expect("save should succeed");
        let loaded = load_config_from(&path);
        assert_eq!(loaded, original);
        cleanup(&path);
    }

    #[test]
    fn round_trip_custom_values() {
        let path = unique_temp_path("roundtrip_custom");
        let original = Config {
            llm_recommend_enabled: false,
            llm_model: "some-other-model".to_string(),
            llm_command: "my-cli".to_string(),
            route_profile: "bike-scenic".to_string(),
            sample_interval_m: 12.5,
            style: "satellite".to_string(),
            show_spots: false,
        };
        save_config_to(&path, &original).expect("save should succeed");
        let loaded = load_config_from(&path);
        assert_eq!(loaded, original);
        cleanup(&path);
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let mut dir = std::env::temp_dir();
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        dir.push(format!(
            "termmap_config_test_nested_{}_{}",
            std::process::id(),
            n
        ));
        // dir itself does not exist yet.
        let path = dir.join("nested").join("config.toml");
        assert!(!path.exists());

        let cfg = Config::default();
        save_config_to(&path, &cfg).expect("save should create parent dirs");
        assert!(path.exists());

        let loaded = load_config_from(&path);
        assert_eq!(loaded, cfg);

        // cleanup
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(dir.join("nested"));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn malformed_lines_keep_default_for_that_key_only() {
        let path = unique_temp_path("malformed_partial");
        let contents = r#"
[llm]
recommend_enabled = false
model = "good-model"
command = not_a_quoted_string

[route]
profile = "good-profile"
sample_interval_m = not_a_number

[display]
style = "good-style"
show_spots = maybe
"#;
        std::fs::write(&path, contents).unwrap();

        let cfg = load_config_from(&path);
        // Recognized, well-formed keys reflected:
        assert_eq!(cfg.llm_recommend_enabled, false);
        assert_eq!(cfg.llm_model, "good-model");
        assert_eq!(cfg.route_profile, "good-profile");
        assert_eq!(cfg.style, "good-style");
        // Malformed values fall back to default for that key only:
        assert_eq!(cfg.llm_command, Config::default().llm_command);
        assert_eq!(cfg.sample_interval_m, Config::default().sample_interval_m);
        assert_eq!(cfg.show_spots, Config::default().show_spots);

        cleanup(&path);
    }

    #[test]
    fn totally_garbage_file_yields_all_defaults() {
        let path = unique_temp_path("garbage");
        std::fs::write(&path, "this is not toml at all\n@@@ ### !!!\n").unwrap();
        let cfg = load_config_from(&path);
        assert_eq!(cfg, Config::default());
        cleanup(&path);
    }

    #[test]
    fn unknown_keys_and_sections_are_ignored() {
        let path = unique_temp_path("unknown_keys");
        let contents = r#"
[llm]
recommend_enabled = false
unknown_key = "ignored"

[totally_unknown_section]
whatever = true

[route]
profile = "custom-profile"
"#;
        std::fs::write(&path, contents).unwrap();

        let cfg = load_config_from(&path);
        assert_eq!(cfg.llm_recommend_enabled, false);
        assert_eq!(cfg.route_profile, "custom-profile");
        // Everything else stays default.
        assert_eq!(cfg.llm_model, Config::default().llm_model);
        assert_eq!(cfg.llm_command, Config::default().llm_command);
        assert_eq!(cfg.sample_interval_m, Config::default().sample_interval_m);
        assert_eq!(cfg.style, Config::default().style);
        assert_eq!(cfg.show_spots, Config::default().show_spots);

        cleanup(&path);
    }

    #[test]
    fn parse_bool_accepts_only_true_or_false() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("false"), Some(false));
        assert_eq!(parse_bool("TRUE"), None);
        assert_eq!(parse_bool("1"), None);
        assert_eq!(parse_bool(""), None);
    }

    #[test]
    fn parse_string_requires_matching_quotes() {
        assert_eq!(parse_string("\"hello\""), Some("hello".to_string()));
        assert_eq!(parse_string("\"\""), Some("".to_string()));
        assert_eq!(parse_string("hello"), None);
        assert_eq!(parse_string("\"unterminated"), None);
        assert_eq!(parse_string("\""), None);
    }

    #[test]
    fn parse_number_accepts_ints_and_floats() {
        assert_eq!(parse_number("800"), Some(800.0));
        assert_eq!(parse_number("800.0"), Some(800.0));
        assert_eq!(parse_number("12.5"), Some(12.5));
        assert_eq!(parse_number("-3.25"), Some(-3.25));
        assert_eq!(parse_number("not_a_number"), None);
        assert_eq!(parse_number("true"), None);
    }

    #[test]
    fn whitespace_and_comments_and_blank_lines_are_tolerated() {
        let path = unique_temp_path("whitespace_comments");
        let contents = "\n\
            # a leading comment\n\
            \n\
            [llm]\n\
            # comment inside a section\n\
            recommend_enabled   =   false   \n\
            \n\
            [route]\n\
            sample_interval_m = 42.0\n\
            \n";
        std::fs::write(&path, contents).unwrap();

        let cfg = load_config_from(&path);
        assert_eq!(cfg.llm_recommend_enabled, false);
        assert_eq!(cfg.sample_interval_m, 42.0);

        cleanup(&path);
    }

    #[test]
    fn load_config_falls_back_to_default_when_home_unset() {
        // load_config() itself is exercised indirectly via config_path();
        // here we just confirm the None-path of load_config mirrors
        // Config::default() by construction (config_path -> None => default).
        // We can't safely unset $HOME for the whole process in a shared
        // test binary, so this test documents the contract via direct call.
        let cfg = match config_path() {
            Some(p) => load_config_from(&p.with_file_name("this_file_almost_certainly_does_not_exist_termmap.toml")),
            None => Config::default(),
        };
        // Whatever branch, a nonexistent file must yield defaults.
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn save_config_and_load_config_use_same_path_contract() {
        // We can't touch the real $HOME config in this test, but we can
        // verify save_config_to/load_config_from agree on round-tripping
        // the exact same struct the public save_config/load_config would
        // operate on, using a temp path substituted for config_path().
        let path = unique_temp_path("public_api_contract");
        let cfg = Config {
            llm_recommend_enabled: true,
            llm_model: "m".to_string(),
            llm_command: "c".to_string(),
            route_profile: "p".to_string(),
            sample_interval_m: 1.0,
            style: "s".to_string(),
            show_spots: false,
        };
        save_config_to(&path, &cfg).unwrap();
        let loaded = load_config_from(&path);
        assert_eq!(loaded, cfg);
        cleanup(&path);
    }
}
