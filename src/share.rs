// スマホ共有: GoogleマップURL生成 と URLパース(QR は呼び出し側で描画)
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // 16進2桁(ASCII)を確かめてから切り出す。確かめずに切ると、%の直後の多バイト文字の途中で
        // 文字境界を割ってパニックする。
        if bytes[i] == b'%' && i + 2 < bytes.len() && bytes[i + 1].is_ascii_hexdigit() && bytes[i + 2].is_ascii_hexdigit() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) { out.push(b); i += 3; continue; }
        }
        if bytes[i] == b'+' { out.push(b' '); } else { out.push(bytes[i]); }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}
fn num_after(url: &str, tag: &str) -> Option<f64> {
    let i = url.find(tag)? + tag.len();
    let rest = &url[i..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}
// GoogleマップのURLから (lat,lon,店名) を取り出す。地点ピン(!3d/!4d)を優先、無ければ表示中心(@lat,lon)。
pub fn parse_gmaps_place(url: &str) -> Option<(f64, f64, String)> {
    let (lat, lon) = match (num_after(url, "!3d"), num_after(url, "!4d")) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            let i = url.find('@')? + 1;
            let mut it = url[i..].split(',');
            (it.next()?.parse().ok()?, it.next()?.parse().ok()?)
        }
    };
    let name = url.find("/place/").map(|i| {
        let rest = &url[i + 7..];
        let end = rest.find('/').unwrap_or(rest.len());
        urldecode(&rest[..end])
    }).unwrap_or_default();
    Some((lat, lon, name))
}
// waypoints → Googleマップ経路URL(origin/destination/waypoints)。経由点はURL上限で切る。
pub fn gmaps_url(wps: &[(f64, f64)]) -> (String, usize) {
    // QRを小さく保つためパス形式(/maps/dir/lat,lon/...)+座標4桁(約11m)。クエリより短い。
    const MAX_PT: usize = 10;
    let dropped = wps.len().saturating_sub(MAX_PT);
    let used: Vec<(f64, f64)> = if wps.len() <= MAX_PT {
        wps.to_vec()
    } else {
        wps.iter().take(MAX_PT - 1).chain(std::iter::once(&wps[wps.len() - 1])).copied().collect()
    };
    let path = used.iter().map(|(la, lo)| format!("{la:.4},{lo:.4}")).collect::<Vec<_>>().join("/");
    (format!("https://www.google.com/maps/dir/{path}"), dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmaps_url_pathform() {
        let (u, dropped) = gmaps_url(&[(35.6812, 139.7671), (35.7141, 139.7774), (35.6595, 139.7967)]);
        assert!(u.starts_with("https://www.google.com/maps/dir/35.6812,139.7671/"));
        assert!(u.contains("/35.7141,139.7774/") && u.ends_with("/35.6595,139.7967"));
        assert_eq!(dropped, 0);
        let many: Vec<(f64, f64)> = (0..11).map(|i| (35.0 + i as f64 * 0.01, 139.0)).collect();
        assert_eq!(gmaps_url(&many).1, 1); // 11点→上限10で1点省略
    }

    #[test]
    fn gmaps_place_and_urldecode() {
        let url = "https://www.google.co.jp/maps/place/%E6%BA%80%E5%B7%9E%E8%BB%92/@35.3835299,139.6160282,19.44z/data=!3m1!5s0x60184366ecf5c297!4m6!3m5!8m2!3d35.3836675!4d139.6162832!16s%2Fg%2F1tmkkyjd";
        let (la, lo, name) = parse_gmaps_place(url).unwrap();
        assert!((la - 35.3836675).abs() < 1e-6 && (lo - 139.6162832).abs() < 1e-6); // !3d/!4d を優先
        assert_eq!(name, "満州軒");
        // /place 無し・@座標のみ
        let (la2, _, n2) = parse_gmaps_place("https://www.google.com/maps/@35.68,139.76,15z").unwrap();
        assert!((la2 - 35.68).abs() < 1e-9 && n2.is_empty());
        // 非URL/座標無しは None
        assert!(parse_gmaps_place("ただの文字列").is_none());
        assert_eq!(urldecode("%E7%8E%8B%E5%AD%90"), "王子");
    }

    #[test]
    fn urldecode_turns_plus_into_space_and_keeps_broken_escapes_as_is() {
        assert_eq!(urldecode("New+York"), "New York");
        assert_eq!(urldecode("abc-123"), "abc-123", "ふつうの文字はそのまま");
        assert_eq!(urldecode("100%zz"), "100%zz", "16進でない%はそのまま残す");
        assert_eq!(urldecode("末尾%"), "末尾%", "途中で切れた%で落ちない");
    }

    // %の直後が16進2桁でなければデコードしない。多バイト文字が続いても文字境界で落ちない。
    #[test]
    fn urldecode_keeps_a_percent_followed_by_a_multibyte_character() {
        assert_eq!(urldecode("100%チョコ"), "100%チョコ");
        assert_eq!(urldecode("%aあ"), "%aあ");
        assert_eq!(urldecode("%+1"), "% 1", "符号付きの数は16進として読まない");
        assert_eq!(urldecode("%e3%81%82"), "あ", "小文字の16進も読む");
    }

    #[test]
    fn gmaps_place_with_a_percent_in_the_shop_name_does_not_panic() {
        let (_, _, name) = parse_gmaps_place("https://www.google.com/maps/place/100%チョコ/@35.1,139.1,15z/data=!3d35.2!4d139.3").unwrap();
        assert_eq!(name, "100%チョコ");
    }

    // 地点ピンは !3d と !4d の両方があるときだけ使い、片方だけなら表示中心(@)へ戻る。
    #[test]
    fn gmaps_place_needs_both_pin_coordinates_or_falls_back_to_the_view_center() {
        let (la, lo, _) = parse_gmaps_place("https://www.google.com/maps/place/x/@35.1,139.1,15z/data=!3d35.9").unwrap();
        assert_eq!((la, lo), (35.1, 139.1));
        assert!(parse_gmaps_place("https://www.google.com/maps/@abc,def,15z").is_none(), "座標が数値でなければ None");
    }
}
