// Space メニュー(2階層: カテゴリ→項目)の静的定義。ui.rs から機械的に切り出したもの(挙動は不変)。
// interactive() のローカル状態(cx/cy/wps/cache 等)には依存しない、参照専用の静的テーブル+関数のみ。

// Space メニュー。2階層(カテゴリ→項目)。項目は「操作として読める動詞ラベル」+ 単キー。
// 実処理は run_action! マクロ(ui.rs の interactive 内)に集約し、各キーの直接操作と共通化している。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    SearchPlace, SearchPoi, ShowAddress, Recommend,                                    // 検索・移動
    RouteForm, AddVia, RoadRoute, ManageRoads, Wander, CycleMode, AltRoute, ClearRoute, // ルート作成(RouteForm=並べ替えを開く / AddVia=中心に地点を置く / ManageRoads=道路の塊を管理)
    ManageSpots, ToggleSpots,                                                          // スポット
    ToggleElevation, StreetView, PlayRoute, ToggleGps, ToggleRadar, ViewCamera,        // ナビ・表示(ToggleRadar=雨雲レーダー/ViewCamera=道路ライブカメラ)
    TogglePopulation,                                                                  // ナビ・表示(500mメッシュ人口)
    SaveRoute, LoadRoute, SaveGpx, ShareQr,                                            // 保存・共有
    Settings, Help,                                                                    // 設定・ヘルプ
}
pub(crate) struct MenuItem { pub(crate) label: &'static str, pub(crate) key: char, pub(crate) action: MenuAction }
pub(crate) struct MenuCategory { pub(crate) label: &'static str, pub(crate) items: &'static [MenuItem] }

pub(crate) const MENU_CATEGORIES: &[MenuCategory] = &[
    MenuCategory { label: "検索・移動", items: &[
        MenuItem { label: "地名を検索",        key: '/', action: MenuAction::SearchPlace },
        MenuItem { label: "目的地を探す",      key: 'f', action: MenuAction::SearchPoi },
        MenuItem { label: "中心の住所を見る",  key: 'a', action: MenuAction::ShowAddress },
        MenuItem { label: "おすすめを出す",    key: '@', action: MenuAction::Recommend },
    ]},
    MenuCategory { label: "ルート作成", items: &[
        MenuItem { label: "地点を置く(中心)",  key: 'v', action: MenuAction::AddVia },
        MenuItem { label: "目的地を探して追加", key: 'f', action: MenuAction::SearchPoi }, // カテゴリ/キーワードで検索→結果一覧のvで追加
        MenuItem { label: "並べ替え・編集",    key: 'R', action: MenuAction::RouteForm },
        MenuItem { label: "道路名から追加",    key: 'r', action: MenuAction::RoadRoute },
        MenuItem { label: "道路の塊を管理",    key: 'D', action: MenuAction::ManageRoads },
        MenuItem { label: "おまかせ周回",      key: 'W', action: MenuAction::Wander },
        MenuItem { label: "移動モード切替",    key: 'm', action: MenuAction::CycleMode },
        MenuItem { label: "別ルートを検索",    key: 'n', action: MenuAction::AltRoute },
        MenuItem { label: "ルートを消去",      key: 'c', action: MenuAction::ClearRoute },
    ]},
    MenuCategory { label: "スポット", items: &[
        MenuItem { label: "マイスポットを開く", key: 'P', action: MenuAction::ManageSpots },
        MenuItem { label: "スポット表示を切替", key: 'V', action: MenuAction::ToggleSpots },
    ]},
    MenuCategory { label: "ナビ・表示", items: &[
        MenuItem { label: "標高プロファイル",  key: 'E', action: MenuAction::ToggleElevation },
        MenuItem { label: "実写を見る",        key: 'i', action: MenuAction::StreetView },
        MenuItem { label: "ルートを再生",      key: 'A', action: MenuAction::PlayRoute },
        MenuItem { label: "ライブ現在地",      key: 'G', action: MenuAction::ToggleGps },
        MenuItem { label: "雨雲レーダー",      key: 'C', action: MenuAction::ToggleRadar },
        MenuItem { label: "道路カメラを見る",  key: 'N', action: MenuAction::ViewCamera },
        MenuItem { label: "人口メッシュ",      key: 'U', action: MenuAction::TogglePopulation },
    ]},
    MenuCategory { label: "保存・共有", items: &[
        MenuItem { label: "ルートを保存",      key: 'S', action: MenuAction::SaveRoute },
        MenuItem { label: "保存ルートを開く",  key: 'L', action: MenuAction::LoadRoute },
        MenuItem { label: "GPXを書き出す",     key: 'g', action: MenuAction::SaveGpx },
        MenuItem { label: "QRで共有",          key: 'o', action: MenuAction::ShareQr },
    ]},
    MenuCategory { label: "設定・ヘルプ", items: &[
        MenuItem { label: "設定を開く",        key: ',', action: MenuAction::Settings },
        MenuItem { label: "ヘルプ",            key: '?', action: MenuAction::Help },
    ]},
];

// メニューの階層。Categories=トップ(カテゴリ選択) / Items(cat)=そのカテゴリの項目選択。
#[derive(Clone, Copy)]
pub(crate) enum MenuLevel { Categories, Items(usize) }

// トップメニューで押された文字キーを全カテゴリ横断で対応するアクションに引く(熟練者の直打ち用)。
pub(crate) fn menu_action_for_key(c: char) -> Option<MenuAction> {
    MENU_CATEGORIES.iter().flat_map(|cat| cat.items.iter()).find(|it| it.key == c).map(|it| it.action)
}

// 表示セル幅(fit_cells と同じ規則: ASCII=1 / 非ASCII=2)。
fn disp_width(s: &str) -> usize { unicode_width::UnicodeWidthStr::width(s) }

// メニュー項目1行。ラベルは左、キーは右端に揃える(幅 w セル内。行頭カーソル prefix の1セルは呼び出し側が足す)。
pub(crate) fn menu_row(label: &str, key: char, w: usize) -> String {
    let mut ks = [0u8; 4];
    let key_s = key.encode_utf8(&mut ks);
    let pad = w.saturating_sub(2 + disp_width(label) + disp_width(key_s));
    format!("  {label}{}{key_s}", " ".repeat(pad))
}

// Map左袖ルートパネルの操作行。Enterで既存のMenuActionを実行(ロジック再利用)。
pub(crate) const ROUTE_ACTS: [(&str, MenuAction); 7] = [
    ("▶ 保存", MenuAction::SaveRoute),
    ("▶ GPX書き出し", MenuAction::SaveGpx),
    ("▶ QRでスマホ共有", MenuAction::ShareQr),
    ("▶ プレビュー走行", MenuAction::PlayRoute),
    ("▶ 標高プロファイル", MenuAction::ToggleElevation),
    ("▶ 代替ルート", MenuAction::AltRoute),
    ("✕ ルート消去", MenuAction::ClearRoute),
];

#[cfg(test)]
mod tests {
    use super::*;

    // 全カテゴリの全キーが menu_action_for_key で引ける(登録漏れが無いことの回帰確認)。
    // MenuAction は Debug 未導出(既存の最小フットプリントを維持)のため assert_eq! でなく matches! で比較する。
    #[test]
    fn menu_action_for_key_resolves_every_registered_key() {
        for cat in MENU_CATEGORIES {
            for it in cat.items {
                let resolved = menu_action_for_key(it.key);
                assert!(matches!(resolved, Some(a) if a == it.action), "key {:?} should resolve", it.key);
            }
        }
    }

    // 雨雲レーダーは「ナビ・表示」カテゴリに C で載っている(地図の C キーと同じアクション)。
    #[test]
    fn toggle_radar_is_registered_under_navigation_with_key_c() {
        let nav = MENU_CATEGORIES.iter().find(|c| c.label == "ナビ・表示").expect("ナビ・表示 カテゴリ");
        let it = nav.items.iter().find(|i| i.action == MenuAction::ToggleRadar).expect("雨雲レーダーの項目");
        assert_eq!(it.key, 'C');
        assert_eq!(it.label, "雨雲レーダー");
        assert!(matches!(menu_action_for_key('C'), Some(MenuAction::ToggleRadar)));
        // 小文字 c(ルート消去)とは別物であること
        assert!(matches!(menu_action_for_key('c'), Some(MenuAction::ClearRoute)));
    }

    // 人口メッシュは「ナビ・表示」カテゴリに U で載っている(地図の U キーと同じアクション)。
    // P はマイスポット・C は雨雲で埋まっているため、空いている U を割り当てている。
    #[test]
    fn toggle_population_is_registered_under_navigation_with_key_u() {
        let nav = MENU_CATEGORIES.iter().find(|c| c.label == "ナビ・表示").expect("ナビ・表示 カテゴリ");
        let it = nav.items.iter().find(|i| i.action == MenuAction::TogglePopulation).expect("人口メッシュの項目");
        assert_eq!(it.key, 'U');
        assert!(matches!(menu_action_for_key('U'), Some(MenuAction::TogglePopulation)));
    }

    // 同じキーが2つの別アクションに割り当てられていないこと(直打ちで意図しない機能が動く事故を防ぐ)。
    // 「目的地を探す」だけは検索・移動とルート作成の両方に同じ f/同じアクションで載っている。
    #[test]
    fn every_key_maps_to_exactly_one_action() {
        let mut seen: Vec<(char, MenuAction)> = Vec::new();
        for cat in MENU_CATEGORIES {
            for it in cat.items {
                if let Some((_, a)) = seen.iter().find(|(k, _)| *k == it.key) {
                    assert!(*a == it.action, "キー {:?} が2つのアクションに割り当てられている", it.key);
                } else {
                    seen.push((it.key, it.action));
                }
            }
        }
    }

    #[test]
    fn menu_action_for_key_unknown_char_is_none() {
        assert!(menu_action_for_key('~').is_none());
    }

    // menu_row: ラベル左・キー右端揃え(幅w内に収まる通常ケース)。
    #[test]
    fn menu_row_pads_key_to_right_edge() {
        let row = menu_row("abc", 'x', 10);
        // "  abc" + pad + "x" が w(=10)セルちょうどになる(先頭2 + ラベル3 + パディング + キー1)
        assert_eq!(disp_width(&row), 2 + 3 + 4 + 1);
        assert!(row.starts_with("  abc"));
        assert!(row.ends_with('x'));
    }

    #[test]
    fn route_acts_actions_match_labels_len() {
        assert_eq!(ROUTE_ACTS.len(), 7);
        assert!(ROUTE_ACTS[0].1 == MenuAction::SaveRoute);
        assert!(ROUTE_ACTS[6].1 == MenuAction::ClearRoute);
    }
}
