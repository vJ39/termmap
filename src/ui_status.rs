// 画面最下段のステータス行(反転表示の1行)の組み立て。ui.rs の描画ループから機械的に
// 切り出したもの(挙動は不変)。ループ状態は読むだけで書き換えない。
// 幅詰め(fit_cells)と端末への出力は呼び出し側(ui.rs)に残してある。

use crate::config::Config;
use crate::focus::Focus;
use crate::menu::MenuLevel;
use crate::radar;
use crate::settings;
use crate::spots::Spot;
use crate::tiles::{radar_progress, TileLoader};

// プロットレイヤ(道路交通量・道路ライブカメラ・通行規制・過去災害)1つぶんの表示状態。
// 件数しか見ないので、アイテムそのものではなく畳んだ値を受け取る。
pub(crate) struct PlotStatus {
    /// 表示範囲に出ている件数。
    pub count: usize,
    /// 背景取得が走っているか。
    pub job_active: bool,
    /// fresh を過ぎた値を出しているときだけ Some(経過秒)。交通量は観測からの経過。
    pub stale_age_secs: Option<u64>,
    /// ズーム下限より広域で、取得を止めているか。
    pub wide_area: bool,
    /// 中心十字がいる区画の名前と件数(過去災害だけが使う。他レイヤは常に None)。
    /// ステータス行に凡例を置く幅は無いので、代わりに「いまどの町にいて、その町にどれだけ
    /// 記録があるか」を直接出す。境界は簡略化された形なので、これは目安。
    pub area: Option<(String, u32)>,
}

// build_status_line が読むループ状態。Option のうち有無しか見ないもの(通信中ジョブ・GPS)は
// 呼び出し側で bool に畳んで渡す。
pub(crate) struct StatusCtx<'a> {
    pub focus: &'a Focus,
    pub save_confirm: &'a Option<String>,
    pub spot_move_confirm: Option<usize>,
    pub spots: &'a [Spot],
    pub cur_cat: &'a str,
    pub pending_spot: bool,
    pub set_sel: usize,
    pub poi_label: &'a str,
    pub route_note: &'a Option<String>,
    pub clear_route_confirm: bool,
    pub jobs_active: bool,
    pub spin: usize,
    pub gps_live: bool,
    pub web_gps_active: bool,
    pub play: Option<f64>,
    pub play_speed: f64,
    pub radar_on: bool,
    pub radar_tl: &'a radar::Timeline,
    pub radar_idx: usize,
    pub radar_follow: bool,
    pub loader: &'a TileLoader,
    pub rcx: f64,
    pub rcy: f64,
    pub rz: u32,
    pub rw: u32,
    pub rh: u32,
    pub cfg: &'a Config,
    pub traffic: PlotStatus,
    pub camera: PlotStatus,
    pub regulation: PlotStatus,
    pub disaster: PlotStatus,
    pub addr: &'a str,
    pub wps: &'a [(f64, f64)],
    pub z: u32,
    pub lat: f64,
    pub lon: f64,
    pub next_turn: &'a Option<String>,
}

// プロットレイヤ1つぶんの表記を組む。
//   fresh                → 🚗12地点
//   stale(表示継続中)   → 🚗12地点(32分前)     ← 今の状態とは限らないことを示す
//   stale + 再取得中     → 🚗12地点(32分前)…
//   0件 + 取得中         → 🚗取得中…
//   0件 + ズーム下限外   → 🚗広域では非表示     ← 取りに行っていないので「無し」とは言えない
//   0件                  → 🚗観測点無し
// fresh の間は経過時間を出さない(常時出すと情報量が増えるだけなので)。
// area が付いているとき(過去災害で中心が塗られた市区町村の中にいるとき)だけは、件数の代わりに
//   🌊野田市 89件(B)
// と出す。地点数より「いま見ている土地に何件の記録があるか」の方が読み手の知りたいことに近い。
fn plot_label(enabled: bool, icon: &str, unit: &str, suffix: &str, none_txt: &str, s: &PlotStatus) -> String {
    if !enabled {
        return String::new();
    }
    if s.count == 0 {
        if s.job_active {
            return format!("{icon}取得中… ");
        }
        if s.wide_area {
            return format!("{icon}広域では非表示 ");
        }
        return format!("{icon}{none_txt} ");
    }
    let age = match s.stale_age_secs {
        Some(secs) => format!("({})", format_age(secs)),
        None => String::new(),
    };
    // 古い値を出したまま裏で取り直しているときだけ、続きがあることを示す。
    let updating = if s.stale_age_secs.is_some() && s.job_active { "…" } else { "" };
    if let Some((name, n)) = &s.area {
        return format!("{icon}{name} {n}件{age}{suffix}{updating} ");
    }
    format!("{icon}{}{unit}{age}{suffix}{updating} ", s.count)
}

// 経過時間の粗い表記(ステータス行は幅が限られるので1単位だけ出す)。
fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}秒前")
    } else if secs < 3600 {
        format!("{}分前", secs / 60)
    } else if secs < 24 * 3600 {
        format!("{}時間前", secs / 3600)
    } else {
        format!("{}日前", secs / (24 * 3600))
    }
}

pub(crate) fn build_status_line(c: StatusCtx) -> String {
    let StatusCtx {
        focus, save_confirm, spot_move_confirm, spots, cur_cat, pending_spot, set_sel, poi_label,
        route_note, clear_route_confirm, jobs_active, spin, gps_live, web_gps_active, play,
        play_speed, radar_on, radar_tl, radar_idx, radar_follow, loader, rcx, rcy, rz, rw, rh,
        cfg, traffic, camera, regulation, disaster,
        addr, wps, z, lat, lon, next_turn,
    } = c;
    match focus {
        Focus::Search(_) => " 中央フォームに入力中 ".to_string(),
        Focus::SaveName(_) if save_confirm.is_some() => {
            let name = save_confirm.as_deref().unwrap_or("");
            format!(" 「{name}」は既に存在します。上書きしますか？ y=上書き / 他キー=名前を変更(新規登録) ")
        }
        Focus::SaveName(_) => " 中央フォームに入力中 ".to_string(),
        Focus::NearSearch(_) => " 中央フォームに入力中 ".to_string(),
        Focus::NewCat(_) => " 中央フォームに入力中 ".to_string(),
        Focus::SpotForm { .. } => " 新規スポット: ↑↓/Tab移動 入力/貼付 Enter=次/送信 Esc=取消 ".to_string(),
        Focus::PoiKindForm { .. } => " 新規カテゴリ: ↑↓/Tab移動 入力 Enter=次/追加 Esc=取消 ".to_string(),
        Focus::WanderForm { .. } => " おまかせ周回: ←→距離調整(Shiftで粗く) Enter=検索開始 Esc=取消 ".to_string(),
        Focus::SpotList if spot_move_confirm.is_some() => {
            let nm = spot_move_confirm.and_then(|gi| spots.get(gi)).map(|s| if s.name.is_empty() { "(無名)" } else { s.name.as_str() }).unwrap_or("");
            format!(" 「{nm}」をこの地図中心の位置へ移動する？ y=はい / 他キー=取消 ")
        }
        Focus::SpotList => format!(" [{cur_cat}] ↑↓ Enter移動 [ ]並替 n新規 r改名 m中心へ x削除 Esc戻る "),
        Focus::SpotEditName(_, _) => " 中央フォームに入力中 ".to_string(),
        Focus::SettingsEdit(..) => " 中央フォームに入力中 ".to_string(),
        Focus::SpotCatList if pending_spot => " 登録先カテゴリを選択: ↑↓ Enter=ここに登録 n新規 Esc取消 ".to_string(),
        Focus::SpotCatList => " カテゴリ: ↑↓選択 [ ]並替 Enter=中へ n新規 r改名 c色 M形 x削除(空のみ) Esc=閉 ".to_string(),
        Focus::Settings => {
            // 各行の説明文は settings.rs::setting_description へ切り出し済み(状態を必要としない純粋部分)。
            let desc = settings::setting_description(set_sel);
            format!(" ▶ {desc}   [↑↓選択 Enter切替/一覧選択/編集 Esc閉(自動保存)]")
        }
        Focus::RoadSearch(_) => " 中央フォームに入力中 ".to_string(),
        Focus::Recommend(_) => " 中央フォームに入力中 ".to_string(),
        Focus::SpotRename(_, _) => " 中央フォームに入力中 ".to_string(),
        Focus::PoiMenu => " 目的地カテゴリ: ↑↓選択 Enter=検索(キー直打ちも可) n新規 x削除 [ ]並替 / キーワードは最終行 Esc=取消 ".to_string(),
        Focus::PoiList => format!(" [{}] ↑↓選択(追従) ←→地図 +/-拡縮 v追加 Enter移動 P登録 f再検索 Esc閉 ", poi_label),
        Focus::RouteList => " お気に入り: ↑↓選択 Enter=読込 Esc=閉 ".to_string(),
        Focus::RouteFavMenu { .. } => " お気に入りルート: ↑↓/ws選択 Enter=決定 Esc=取消 ".to_string(),
        Focus::RoutePanel => {
            let base = " ルート一覧: ↑↓/ws選択 Enter実行 [ ]並替 x削除 v追加 +/-拡縮 Esc/Tabで地図へ ".to_string();
            match route_note { Some(rn) => format!("{base}| {rn} "), None => base }
        }
        Focus::RoadList => " 道路: ↑↓選択 x削除 Esc戻る ".to_string(),
        Focus::WaypointList => " 並べ替え: ↑↓/ws選択(地図追従)  Space掴む↔置く(掴み中↑↓/wsで移動)  x削除  +/-拡縮  Esc閉 ".to_string(),
        Focus::ColorPick { .. } => " 色を選択: ←→ Enter=決定 Esc=取消 ".to_string(),
        Focus::ShapePick { .. } => " 形を選択: ←→ Enter=決定 Esc=取消 ".to_string(),
        Focus::SettingsPick(27) => " 候補を選択: ↑↓/ws選択 Space=試聴 Enter=決定 Esc=取消(展開を閉じる) ".to_string(),
        Focus::SettingsPick(_) => " 候補を選択: ↑↓/ws選択 Enter=決定 Esc=取消(展開を閉じる) ".to_string(),
        Focus::Menu(MenuLevel::Categories) => " ↑↓カテゴリ Enter展開 / 文字キーで直接実行 Esc閉 ".to_string(),
        Focus::Menu(MenuLevel::Items(_)) => " ↑↓選択 Enter実行 / 右端キーでも実行 Esc戻る ".to_string(),
        Focus::Map if clear_route_confirm => " ルートを全消去しますか？ y=はい / 他キー=取消 ".to_string(),
        Focus::Map => {
            // 通信中(いずれかのジョブがSome)はスピナー1文字＋案内を先頭に出す
            let spinner = if jobs_active {
                const FR: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                format!("{} 通信中…(Escで中断) ", FR[spin % FR.len()])
            } else { String::new() };
            let live = if gps_live { "●LIVE(Gで解除) " }
                else if web_gps_active { "●LIVE(スマホGPS) " }
                else { "" };
            let playing = if play.is_some() { format!("▶再生{play_speed:.2}x([ ]変速/A停止) ") } else { String::new() };
            // 雨雲レーダー: 表示中の時刻・種別と、タイルの読込状況。ONのときだけ出す。
            // 出典表記は幅を食うので毎フレームは出さず、ONにした直後のメッセージ(addr)で1回出す。
            let radar_txt = if !radar_on {
                String::new()
            } else {
                match radar_tl.get(radar_idx) {
                    // targetTimes がまだ届いていない(ONにした直後・取得失敗中)
                    None => "☂時刻取得中… ".to_string(),
                    Some(f) => {
                        let (got, need) = radar_progress(loader, rcx, rcy, rz, rw, rh, f);
                        if need == 0 {
                            "☂範囲外 ".to_string() // 日本国外を表示中=1枚も取りに行っていない
                        } else if got < need {
                            format!("☂{} 読込{got}/{need} ", radar::frame_label(radar_tl, radar_idx))
                        } else {
                            let follow = if radar_follow { "(追従)" } else { "" };
                            format!("☂{}{follow} ", radar::frame_label(radar_tl, radar_idx))
                        }
                    }
                }
            };
            // 道路交通量: ONのときだけ、取得地点数(または取得中)を出す。0件は「圏外/観測点無し」
            // (このデータは直轄国道の観測点のみで、それ以外の道路には点が無い)と区別できるようにする。
            let traffic_txt = plot_label(cfg.traffic_enabled, "🚗", "地点", "", "観測点無し", &traffic);
            // 道路ライブカメラ: ONのときだけ件数を出す(考え方はtraffic_txtと同じ)。
            let camera_txt = plot_label(cfg.camera_enabled, "📷", "台", "(N)", "カメラ無し", &camera);
            // 通行規制: ONのときだけ件数を出す(考え方はtraffic_txtと同じ)。
            let regulation_txt = plot_label(cfg.regulation_enabled, "⚠", "件", "", "規制無し", &regulation);
            // 過去災害: 件数ではなく**地点数**を出す(事例数だと数千になり他レイヤと桁が合わない)。
            // (B)はカメラの(N)と同じで、押すと詳細が出るキーがあることを示す。
            let disaster_txt = plot_label(cfg.disaster_enabled, "🌊", "地点", "(B)", "記録無し", &disaster);
            // 一時メッセージが無い時は底面にロゴを常時表示。メッセージ発生時はそちらを優先。
            let msg = if addr.is_empty() { "◉╌╌╌► termmap · terminal touring map   ".to_string() } else { format!("» {addr} « ") };
            // 下部バーは細く。全操作は Space メニューから選べる
            let route_hint = if wps.is_empty() { "v=地点を置く".to_string() } else { format!("{}点 v足す w/s選択(操作行までEnterで実行) Tab=左の一覧へ(並替/操作)", wps.len()) };
            // 次の曲がり角(音声案内と同じデータソース。ONでルート走行中のみ出る)。
            let turn_txt = next_turn.as_deref().unwrap_or("");
            let base = format!(" {spinner}{msg}{live}{playing}{radar_txt}{traffic_txt}{camera_txt}{regulation_txt}{disaster_txt}{turn_txt}z{z} {lat:.4},{lon:.4} ｜ {route_hint} ｜ Space:メニュー ?ヘルプ q終了");
            match route_note { Some(rn) => format!("{base} | {rn} "), None => base }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::MenuLevel;
    use crate::tiles::Cache;

    // TileLoader はワーカースレッドを起こすのでテスト全体で1つだけ使い回す。
    // (radar_on=false もしくはコマ未取得のケースしか通さないため実際には参照されない)
    fn shared_loader() -> &'static TileLoader {
        static L: std::sync::OnceLock<TileLoader> = std::sync::OnceLock::new();
        L.get_or_init(|| TileLoader::start(std::sync::Arc::new(std::sync::Mutex::new(Cache::new()))))
    }

    // StatusCtx は借用の束なので、所有側をここに持って ctx() で組む。既定値は
    // 「Map・通信なし・レーダー/交通量OFF・メッセージなし・ルート未設定」。
    struct Fixture {
        focus: Focus,
        save_confirm: Option<String>,
        spot_move_confirm: Option<usize>,
        spots: Vec<Spot>,
        cur_cat: String,
        pending_spot: bool,
        set_sel: usize,
        poi_label: String,
        route_note: Option<String>,
        clear_route_confirm: bool,
        jobs_active: bool,
        spin: usize,
        gps_live: bool,
        web_gps_active: bool,
        play: Option<f64>,
        play_speed: f64,
        radar_on: bool,
        radar_tl: radar::Timeline,
        radar_idx: usize,
        radar_follow: bool,
        cfg: Config,
        traffic: PlotStatus,
        camera: PlotStatus,
        regulation: PlotStatus,
        disaster: PlotStatus,
        addr: String,
        wps: Vec<(f64, f64)>,
        z: u32,
        lat: f64,
        lon: f64,
        next_turn: Option<String>,
    }

    impl Fixture {
        fn new(focus: Focus) -> Self {
            Fixture {
                focus, save_confirm: None, spot_move_confirm: None, spots: Vec::new(),
                cur_cat: String::new(), pending_spot: false, set_sel: 0, poi_label: String::new(),
                route_note: None, clear_route_confirm: false, jobs_active: false, spin: 0,
                gps_live: false, web_gps_active: false, play: None, play_speed: 1.0,
                radar_on: false, radar_tl: radar::Timeline::default(), radar_idx: 0, radar_follow: true,
                cfg: Config::default(),
                traffic: idle_plot(), camera: idle_plot(), regulation: idle_plot(), disaster: idle_plot(),
                addr: String::new(), wps: Vec::new(), z: 14, lat: 35.0, lon: 139.0, next_turn: None,
            }
        }
        fn line(&self) -> String {
            build_status_line(StatusCtx {
                focus: &self.focus, save_confirm: &self.save_confirm,
                spot_move_confirm: self.spot_move_confirm, spots: &self.spots,
                cur_cat: &self.cur_cat, pending_spot: self.pending_spot, set_sel: self.set_sel,
                poi_label: &self.poi_label, route_note: &self.route_note,
                clear_route_confirm: self.clear_route_confirm, jobs_active: self.jobs_active,
                spin: self.spin, gps_live: self.gps_live, web_gps_active: self.web_gps_active,
                play: self.play, play_speed: self.play_speed,
                radar_on: self.radar_on, radar_tl: &self.radar_tl, radar_idx: self.radar_idx,
                radar_follow: self.radar_follow,
                loader: shared_loader(), rcx: 0.0, rcy: 0.0, rz: 10, rw: 300, rh: 200,
                cfg: &self.cfg,
                traffic: clone_plot(&self.traffic),
                camera: clone_plot(&self.camera),
                regulation: clone_plot(&self.regulation),
                disaster: clone_plot(&self.disaster),
                addr: &self.addr, wps: &self.wps, z: self.z, lat: self.lat, lon: self.lon,
                next_turn: &self.next_turn,
            })
        }
    }

    fn idle_plot() -> PlotStatus {
        PlotStatus { count: 0, job_active: false, stale_age_secs: None, wide_area: false, area: None }
    }
    fn clone_plot(p: &PlotStatus) -> PlotStatus {
        PlotStatus {
            count: p.count,
            job_active: p.job_active,
            stale_age_secs: p.stale_age_secs,
            wide_area: p.wide_area,
            area: p.area.clone(),
        }
    }

    fn spot(name: &str) -> Spot {
        Spot { lat: 35.0, lon: 139.0, cat: "温泉".to_string(), name: name.to_string() }
    }

    #[test]
    fn text_input_focuses_share_the_generic_message() {
        let focuses = [
            Focus::Search(String::new()),
            Focus::SaveName("箱根".to_string()), // save_confirm が無い間は入力中扱い
            Focus::NearSearch(String::new()),
            Focus::NewCat(String::new()),
            Focus::RoadSearch(String::new()),
            Focus::Recommend(String::new()),
            Focus::SpotRename(String::new(), 0),
            Focus::SpotEditName(String::new(), 0),
            Focus::SettingsEdit(6, String::new()),
        ];
        for f in focuses {
            assert_eq!(Fixture::new(f).line(), " 中央フォームに入力中 ");
        }
    }

    #[test]
    fn save_name_asks_before_overwriting_an_existing_name() {
        let mut f = Fixture::new(Focus::SaveName("箱根".to_string()));
        f.save_confirm = Some("箱根".to_string());
        let s = f.line();
        assert!(s.contains("「箱根」は既に存在します"), "{s}");
        assert!(s.contains("y=上書き"));
    }

    #[test]
    fn spot_move_confirmation_names_the_target_spot() {
        let mut f = Fixture::new(Focus::SpotList);
        f.spots = vec![spot("A湯"), spot("")];
        f.spot_move_confirm = Some(0);
        assert!(f.line().contains("「A湯」をこの地図中心の位置へ移動する？"));
        f.spot_move_confirm = Some(1);
        assert!(f.line().contains("「(無名)」を"), "name空は(無名)");
        // 添字が範囲外でも名前を空にするだけで落ちない
        f.spot_move_confirm = Some(9);
        assert!(f.line().contains("「」を"));
    }

    #[test]
    fn spot_list_without_confirmation_shows_the_category_and_keys() {
        let mut f = Fixture::new(Focus::SpotList);
        f.cur_cat = "温泉".to_string();
        assert_eq!(f.line(), " [温泉] ↑↓ Enter移動 [ ]並替 n新規 r改名 m中心へ x削除 Esc戻る ");
    }

    // 前回の切り出しで pending_spot を落としてこの分岐が消えた経緯があるので、両方の文面を固定する。
    #[test]
    fn spot_cat_list_switches_message_while_a_spot_is_pending() {
        let mut f = Fixture::new(Focus::SpotCatList);
        assert_eq!(f.line(), " カテゴリ: ↑↓選択 [ ]並替 Enter=中へ n新規 r改名 c色 M形 x削除(空のみ) Esc=閉 ");
        f.pending_spot = true;
        assert_eq!(f.line(), " 登録先カテゴリを選択: ↑↓ Enter=ここに登録 n新規 Esc取消 ");
    }

    #[test]
    fn settings_shows_the_description_of_the_selected_row() {
        let mut f = Fixture::new(Focus::Settings);
        f.set_sel = 2;
        let s = f.line();
        assert!(s.starts_with(&format!(" ▶ {}", settings::setting_description(2))), "{s}");
        assert!(s.contains("Esc閉(自動保存)"));
    }

    #[test]
    fn poi_list_shows_the_search_label() {
        let mut f = Fixture::new(Focus::PoiList);
        f.poi_label = "コンビニ".to_string();
        assert!(f.line().starts_with(" [コンビニ] ↑↓選択(追従)"));
    }

    #[test]
    fn menu_levels_have_their_own_hints() {
        assert!(Fixture::new(Focus::Menu(MenuLevel::Categories)).line().contains("Enter展開"));
        assert!(Fixture::new(Focus::Menu(MenuLevel::Items(0))).line().contains("右端キーでも実行"));
    }

    #[test]
    fn map_status_defaults_to_the_logo_line() {
        let f = Fixture::new(Focus::Map);
        assert_eq!(
            f.line(),
            " ◉╌╌╌► termmap · terminal touring map   z14 35.0000,139.0000 ｜ v=地点を置く ｜ Space:メニュー ?ヘルプ q終了"
        );
    }

    #[test]
    fn map_status_shows_the_message_instead_of_the_logo() {
        let mut f = Fixture::new(Focus::Map);
        f.addr = "東京都千代田区".to_string();
        let s = f.line();
        assert!(s.starts_with(" » 東京都千代田区 « z14"), "{s}");
        assert!(!s.contains("termmap · terminal touring map"));
    }

    #[test]
    fn map_status_counts_waypoints_in_the_route_hint() {
        let mut f = Fixture::new(Focus::Map);
        f.wps = vec![(35.0, 139.0), (36.0, 140.0)];
        assert!(f.line().contains("｜ 2点 v足す w/s選択"));
    }

    #[test]
    fn spinner_shows_only_while_a_job_runs_and_wraps_around() {
        let mut f = Fixture::new(Focus::Map);
        assert!(!f.line().contains("通信中"));
        f.jobs_active = true;
        assert!(f.line().starts_with(" ⠋ 通信中…(Escで中断) "));
        f.spin = 1;
        assert!(f.line().starts_with(" ⠙ 通信中…"));
        f.spin = 10; // フレーム数10で一周する
        assert!(f.line().starts_with(" ⠋ 通信中…"));
    }

    #[test]
    fn live_label_prefers_the_local_gps_over_the_phone() {
        let mut f = Fixture::new(Focus::Map);
        f.gps_live = true;
        f.web_gps_active = true;
        assert!(f.line().contains("●LIVE(Gで解除) "));
        f.gps_live = false;
        assert!(f.line().contains("●LIVE(スマホGPS) "));
        f.web_gps_active = false;
        assert!(!f.line().contains("●LIVE"));
    }

    #[test]
    fn playback_label_shows_the_speed_only_while_playing() {
        let mut f = Fixture::new(Focus::Map);
        assert!(!f.line().contains("▶再生"));
        f.play = Some(0.0);
        f.play_speed = 2.5;
        assert!(f.line().contains("▶再生2.50x([ ]変速/A停止) "));
    }

    #[test]
    fn radar_label_appears_only_when_on_and_waits_for_the_timeline() {
        let mut f = Fixture::new(Focus::Map);
        assert!(!f.line().contains('☂'));
        f.radar_on = true; // コマ一覧が未取得(Timeline空)の間は時刻取得中
        assert!(f.line().contains("☂時刻取得中… "));
    }

    #[test]
    fn traffic_label_distinguishes_loading_from_no_observation_points() {
        let mut f = Fixture::new(Focus::Map);
        assert!(!f.line().contains('🚗'), "OFFのときは出さない");
        f.cfg.traffic_enabled = true;
        f.traffic.job_active = true;
        assert!(f.line().contains("🚗取得中… "));
        f.traffic.job_active = false;
        assert!(f.line().contains("🚗観測点無し "));
        f.traffic.count = 2;
        assert!(f.line().contains("🚗2地点 "));
        f.traffic.job_active = true; // 取得済みなら取得中でも件数を優先する
        assert!(f.line().contains("🚗2地点 "));
    }

    #[test]
    fn camera_label_distinguishes_loading_from_no_cameras() {
        let mut f = Fixture::new(Focus::Map);
        assert!(!f.line().contains('📷'), "OFFのときは出さない");
        f.cfg.camera_enabled = true;
        f.camera.job_active = true;
        assert!(f.line().contains("📷取得中… "));
        f.camera.job_active = false;
        assert!(f.line().contains("📷カメラ無し "));
        f.camera.count = 1;
        assert!(f.line().contains("📷1台(N) "));
        f.camera.job_active = true; // 取得済みなら取得中でも件数を優先する
        assert!(f.line().contains("📷1台(N) "));
    }

    #[test]
    fn regulation_label_distinguishes_loading_from_no_regulations() {
        let mut f = Fixture::new(Focus::Map);
        assert!(!f.line().contains('⚠'), "OFFのときは出さない");
        f.cfg.regulation_enabled = true;
        f.regulation.job_active = true;
        assert!(f.line().contains("⚠取得中… "));
        f.regulation.job_active = false;
        assert!(f.line().contains("⚠規制無し "));
        f.regulation.count = 1;
        assert!(f.line().contains("⚠1件 "));
        f.regulation.job_active = true; // 取得済みなら取得中でも件数を優先する
        assert!(f.line().contains("⚠1件 "));
    }

    #[test]
    fn disaster_label_counts_sites_and_advertises_the_detail_key() {
        let mut f = Fixture::new(Focus::Map);
        assert!(!f.line().contains('🌊'), "OFFのときは出さない");
        f.cfg.disaster_enabled = true;
        f.disaster.job_active = true;
        assert!(f.line().contains("🌊取得中… "));
        f.disaster.job_active = false;
        assert!(f.line().contains("🌊記録無し "));
        f.disaster.count = 12;
        assert!(f.line().contains("🌊12地点(B) "), "事例数ではなく地点数");
        f.disaster.wide_area = true;
        f.disaster.count = 0;
        assert!(f.line().contains("🌊広域では非表示 "));
    }

    #[test]
    fn a_stale_disaster_layer_keeps_the_detail_key_hint_after_the_age() {
        let mut f = Fixture::new(Focus::Map);
        f.cfg.disaster_enabled = true;
        f.disaster.count = 7;
        f.disaster.stale_age_secs = Some(31 * 24 * 3600);
        assert!(f.line().contains("🌊7地点(31日前)(B) "));
    }

    // 中心十字が塗られた市区町村の中にいるときは、地点数ではなくその市区町村を出す
    // (ステータス行に凡例を置く幅が無いので、代わりに直接答える)。
    #[test]
    fn the_disaster_label_names_the_municipality_under_the_crosshair() {
        let mut f = Fixture::new(Focus::Map);
        f.cfg.disaster_enabled = true;
        f.disaster.count = 12;
        f.disaster.area = Some(("野田市".to_string(), 89));
        assert!(f.line().contains("🌊野田市 89件(B) "), "{}", f.line());
        assert!(!f.line().contains("12地点"), "地点数は出さない");
    }

    #[test]
    fn the_disaster_label_falls_back_to_the_site_count_outside_any_municipality() {
        let mut f = Fixture::new(Focus::Map);
        f.cfg.disaster_enabled = true;
        f.disaster.count = 12;
        f.disaster.area = None; // 海の上・記録の無い市区町村・境界が未取得
        assert!(f.line().contains("🌊12地点(B) "));
        // 0件のときは市区町村名より先に「取得中/広域では非表示/記録無し」を出す。
        f.disaster.count = 0;
        f.disaster.area = Some(("野田市".to_string(), 89));
        assert!(f.line().contains("🌊記録無し "), "{}", f.line());
    }

    #[test]
    fn a_stale_municipality_label_still_shows_the_age_and_the_detail_key() {
        let mut f = Fixture::new(Focus::Map);
        f.cfg.disaster_enabled = true;
        f.disaster.count = 7;
        f.disaster.area = Some(("広島市中区".to_string(), 278));
        f.disaster.stale_age_secs = Some(31 * 24 * 3600);
        f.disaster.job_active = true;
        assert!(f.line().contains("🌊広島市中区 278件(31日前)(B)… "), "{}", f.line());
    }

    // 市区町村名を出すのは過去災害だけ。他レイヤは area を持たない(常に件数表示)。
    #[test]
    fn the_other_plot_layers_never_show_an_area_name() {
        let mut f = Fixture::new(Focus::Map);
        f.cfg.traffic_enabled = true;
        f.traffic.count = 5;
        assert!(f.traffic.area.is_none());
        assert!(f.line().contains("🚗5地点 "));
    }

    #[test]
    fn the_five_plot_layers_do_not_collide_in_the_status_line() {
        // 5レイヤ全てONでも、それぞれのアイコンが1つずつ出る(記号の衝突が無い)。
        let mut f = Fixture::new(Focus::Map);
        f.cfg.traffic_enabled = true;
        f.cfg.camera_enabled = true;
        f.cfg.regulation_enabled = true;
        f.cfg.disaster_enabled = true;
        f.traffic.count = 1;
        f.camera.count = 2;
        f.regulation.count = 3;
        f.disaster.count = 4;
        let line = f.line();
        for icon in ['🚗', '📷', '⚠', '🌊'] {
            assert_eq!(line.matches(icon).count(), 1, "{icon} が {line}");
        }
        assert!(line.contains("🚗1地点 📷2台(N) ⚠3件 🌊4地点(B) "), "{line}");
    }

    // fresh を過ぎた値を出している間だけ経過時間を添える(今の状態とは限らないことを示す)。
    #[test]
    fn a_stale_layer_shows_how_old_the_data_is() {
        let mut f = Fixture::new(Focus::Map);
        f.cfg.traffic_enabled = true;
        f.traffic.count = 12;
        assert!(f.line().contains("🚗12地点 "), "freshなら経過時間は出さない");
        f.traffic.stale_age_secs = Some(32 * 60);
        assert!(f.line().contains("🚗12地点(32分前) "));
        f.traffic.job_active = true;
        assert!(f.line().contains("🚗12地点(32分前)… "), "裏で取り直し中は続きを示す");
    }

    #[test]
    fn a_stale_camera_keeps_the_photo_key_hint_after_the_age() {
        let mut f = Fixture::new(Focus::Map);
        f.cfg.camera_enabled = true;
        f.camera.count = 3;
        f.camera.stale_age_secs = Some(9 * 24 * 3600);
        assert!(f.line().contains("📷3台(9日前)(N) "));
    }

    // ズーム下限より広域では取りに行かないので「無し」とは言えない。
    #[test]
    fn a_wide_area_view_says_the_layer_is_not_shown_rather_than_empty() {
        let mut f = Fixture::new(Focus::Map);
        f.cfg.regulation_enabled = true;
        f.regulation.wide_area = true;
        assert!(f.line().contains("⚠広域では非表示 "));
        // 既に持っているぶんが表示されているなら、件数の方を出す。
        f.regulation.count = 4;
        assert!(f.line().contains("⚠4件 "));
    }

    #[test]
    fn a_wide_area_view_still_reports_an_in_flight_job_first() {
        let mut f = Fixture::new(Focus::Map);
        f.cfg.traffic_enabled = true;
        f.traffic.wide_area = true;
        f.traffic.job_active = true;
        assert!(f.line().contains("🚗取得中… "));
    }

    #[test]
    fn age_is_rendered_with_a_single_unit() {
        assert_eq!(format_age(0), "0秒前");
        assert_eq!(format_age(59), "59秒前");
        assert_eq!(format_age(60), "1分前");
        assert_eq!(format_age(3599), "59分前");
        assert_eq!(format_age(3600), "1時間前");
        assert_eq!(format_age(24 * 3600 - 1), "23時間前");
        assert_eq!(format_age(24 * 3600), "1日前");
        assert_eq!(format_age(30 * 24 * 3600), "30日前");
    }

    #[test]
    fn clear_route_confirmation_replaces_the_map_status() {
        let mut f = Fixture::new(Focus::Map);
        f.clear_route_confirm = true;
        assert_eq!(f.line(), " ルートを全消去しますか？ y=はい / 他キー=取消 ");
    }

    // 区切りが Map は " | "、ルートパネルは "| " と元から違うのでそのまま固定する。
    #[test]
    fn route_note_is_appended_with_the_original_separators() {
        let mut f = Fixture::new(Focus::Map);
        f.route_note = Some("経路探索に失敗".to_string());
        assert!(f.line().ends_with("q終了 | 経路探索に失敗 "));

        let mut f = Fixture::new(Focus::RoutePanel);
        f.route_note = Some("経路探索に失敗".to_string());
        assert_eq!(f.line(), " ルート一覧: ↑↓/ws選択 Enter実行 [ ]並替 x削除 v追加 +/-拡縮 Esc/Tabで地図へ | 経路探索に失敗 ");
        f.route_note = None;
        assert_eq!(f.line(), " ルート一覧: ↑↓/ws選択 Enter実行 [ ]並替 x削除 v追加 +/-拡縮 Esc/Tabで地図へ ");
    }
}
