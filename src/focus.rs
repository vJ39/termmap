// 対話UIの画面状態(どのパネル/フォームに入力が向いているか)。ui.rs の interactive() の
// ローカル定義だったものを、描画側(ui_gutter/ui_status/ui_overlay)からも参照できるよう切り出した。

use crate::menu::MenuLevel;

pub(crate) enum Focus {
    Map,
    RoutePanel,
    Menu(MenuLevel),
    Search(String),
    SaveName(String),
    NearSearch(String),
    PoiMenu,
    PoiList,
    RouteList,
    WaypointList,
    RoadList,
    NewCat(String),
    SpotForm { name: String, url: String, field: usize },
    SpotList,
    SpotCatList,
    SpotRename(String, usize),
    Settings,
    SettingsEdit(usize, String),
    SettingsPick(usize),
    RoadSearch(String),
    SpotEditName(String, usize),
    Recommend(String),
    ColorPick { cat: usize },
    ShapePick { cat: usize },
    PoiKindForm { label: String, tag: String, field: usize },
    WanderForm { dist_km: f64 },
    RouteFavMenu { sel: usize }, // お気に入りルート: 保存/呼び出しの小メニュー(Sキーで開く)
}
