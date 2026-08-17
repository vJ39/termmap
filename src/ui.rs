// 対話UIループ。main.rs から機械的に切り出したもの(挙動は不変)。
// HELP / TermGuard / interactive を収める。fit_cells 等はクレートルート(main.rs)側に残す。

use crate::*;
use crate::geo::*;
use crate::tiles::*;
use crate::render::*;
use crate::route::*;
use crate::poi::*;
use crate::spots::*;
use crate::share::*;
use std::io::Write;
use image::{RgbImage, RgbaImage, imageops::FilterType};
// 単一行テキスト入力欄の共通編集ロジック(char_byte/insert_str_at/form_cur/edit_line/
// render_with_cursor/draw_input_panel)は textedit.rs へ切り出し済み。
use crate::textedit::{edit_line, form_cur, insert_str_at};
// 左袖リストのスクロール追従(ensure_visible)は listview.rs、行の組み立ては ui_gutter.rs へ切り出し済み。

// PALETTE_NAMES(中心十字の色名。SPOT_PALETTEと同じ並び)・その利用箇所は settings.rs に移設。
// 緑グラデのワードマーク(LOGO・ヘルプ画面で使用)は keymap.rs へ移設済み。

use crate::keymap::{HELP, LOGO};

// Space メニュー(MenuAction/MenuItem/MenuCategory/MENU_CATEGORIES/MenuLevel/menu_action_for_key/
// disp_width/menu_row/ROUTE_ACTS)は menu.rs へ切り出し済み。ここでは crate::menu を参照する。
use crate::menu::{MenuAction, MENU_CATEGORIES, MenuLevel, menu_action_for_key, ROUTE_ACTS};

// 道路名検索(r)で追加した道路の塊(RoadSeg)・その表示色選択(road_color_for)は roadseg.rs へ切り出し済み。
use crate::roadseg::{RoadSeg, road_color_for};

// interactive() の外にあった小さなヘルパー(onboarded_marker/QrView/build_qr_view/radar_opacity_value/
// radar_refresh_secs/maybe_speak_turn/persist_full_state/TermGuard)は ui_helpers.rs へ切り出し済み。
// 画面状態の Focus enum は focus.rs へ切り出し済み(描画側の ui_gutter/ui_status/ui_overlay からも参照する)。
use crate::ui_helpers::*;
use crate::focus::Focus;

// ---- 対話モード (crossterm) ----

pub(crate) fn interactive(mut cx: f64, mut cy: f64, mut z: u32, a: &Args) -> std::io::Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    let _guard = TermGuard::enter()?; // Drop で必ず端末復元
    // タイルキャッシュは常駐ローダーとメイン描画で共有する(Arc<Mutex>)。未取得タイルはメインが
    // グレーで即描画し、ローダーが現在viewに近い順で裏取得→cacheへ→次フレームで自動反映される。
    let cache = std::sync::Arc::new(std::sync::Mutex::new(Cache::new()));
    let loader = TileLoader::start(std::sync::Arc::clone(&cache));
    let mut out = std::io::stdout();
    let mut addr = String::new();          // 'a' 住所 / 一時メッセージ
    let mut focus = Focus::Map;
    let mut cfg = config::load_config();   // 設定(streetview key / 描画既定 等・設定画面で書き換え)
    let mut opts = a.clone();              // 実行中に変えられる描画設定(Argsのコピー)
    // config を既定として適用(CLIフラグは ON 方向で優先。style は CLI が既定osmなら config 採用)
    opts.braille = opts.braille || cfg.braille;
    opts.classify = opts.classify || cfg.classify;
    opts.edge = opts.edge || cfg.edge;
    opts.mono = opts.mono || cfg.mono;
    if opts.style == "osm" { opts.style = cfg.style.clone(); }
    // サブピクセル切り出しの上書き(設計 §5.1 のリスク項目)。起動時に1回だけ読む
    // (毎フレーム std::env::var を呼ぶ必要は無い)。未設定なら use_subpixel_window の既定。
    let subpixel_env: Option<String> = std::env::var("TERMMAP_SUBPIXEL").ok();
    let mut set_sel: usize = 0;            // 設定画面の選択行
    let mut input_cur: usize = 0;          // テキスト入力欄のカーソル位置(文字単位)。テキストFocus開始時に該当バッファ末尾へ
    let mut menu_cat_sel: usize = 0;       // Space メニュー: トップのカテゴリ選択
    let mut menu_item_sel: usize = 0;      // Space メニュー: 展開後の項目選択
    let mut poimenu_sel: usize = 0;        // 目的地カテゴリの選択行
    let mut street: Option<(RgbImage, i32, f64, f64)> = None; // 実写(画像, heading, lat, lon)
    let mut sv_fov: f64 = 90.0; // 実写のズーム(画角・度。小さいほどズームイン)。実写を開き直すたび既定値に戻す

    let (home_lat, home_lon) = pixel_to_deg(cx, cy, z);
    let mut spec = build_spec(a, home_lat, home_lon); // --range のリングは保持

    let mut wps: Vec<(f64, f64)> = a.route.clone().unwrap_or_default(); // 始点..終点
    let mut wp_sel: usize = 0;             // Tab で巡回する選択 waypoint
    let mut road_segs: Vec<RoadSeg> = Vec::new(); // 道路名検索(r)で追加した道路の塊(別色レイヤ・spec.roadsへ同期)
    let mut road_sel: usize = 0;           // 道路一覧(RoadList)の選択行
    let mut grab = false;                  // 並べ替えビューで地点を「掴んで」移動中か
    let mut route_sel: usize = 0;          // Map左袖ルートパネルの選択(0..n=点 / 以降=操作行)
    // ルートパネルの操作行(Enterで既存のMenuActionを実行・ロジック再利用)は menu.rs の ROUTE_ACTS。
    let mut mode = a.route_mode.clone();
    let mut pois: Vec<(f64, f64, String, PoiCat)> = Vec::new(); // 目的地検索結果
    let mut poi_sel: usize = 0;
    let mut poi_label = String::new();
    let mut route_names: Vec<String> = Vec::new(); // お気に入り一覧(L)
    let mut rn_sel: usize = 0;
    let mut help = false; // ? でヘルプ表示
    let mut help_page: usize = 0; // ヘルプが画面高に収まらない時のページ送り(0始まり)
    let mut qr_view: Option<QrView> = None; // o でGoogleマップQRをポップアップ表示
    let mut route_alt: u32 = 0; // n で BRouter の代替ルート(0..=3)を巡回
    let mut route_ele: Vec<f64> = Vec::new(); // 直近ルートの標高列(pts と同数)
    let mut route_ascend: f64 = 0.0;          // 直近ルートの累積登り(m)
    let mut show_elev = false;                // E で標高プロファイル表示
    let mut gps_rx: Option<gpslive::GpsPoller> = None; // G ライブ現在地(drop で停止)
    let mut gps_pos: Option<(f64, f64)> = None; // 最新の自位置
    let mut gps_trail: Vec<(f64, f64)> = Vec::new(); // 通過ブレッドクラム
    // web/touch-overlay.js からブラウザのGeolocation APIで送られてくるライブ現在地。
    // gps_rx(CoreLocationCLI・Mac本体の位置)とは別経路だが、描画(gps_pos/gps_trail)は共有する。
    let mut web_gps_active = false;
    // R でルート一覧(左袖)を隠す。ルート自体(wps)は保持したまま表示だけ消す
    // (画面が狭いWeb版で、ルートがある間ずっと出っぱなしなのが邪魔だという要望への対応)。
    // Tab等でRoutePanelへ実際にフォーカスした時は隠さない(操作したいのに何も見えないと困るため)。
    let mut route_panel_hidden = false;
    // 雨雲レーダー(気象庁ナウキャスト・C で ON/OFF、< > で表示時刻を前後)。
    // 起動時の状態は設定 [radar] enabled(既定OFF)に従う。ONにした人だけが外部サービスへ問い合わせる。
    let mut radar_on = cfg.radar_enabled;
    let mut radar_tl = radar::Timeline::default();  // フレーム時刻の一覧(RadarClock が5分ごとに更新)
    let mut radar_idx: usize = 0;                   // 表示中のコマ
    let mut radar_follow = true;                    // 最新の実況に追従するか(< > でスクラブすると外れる)
    // 時刻一覧の背景ポーラー(drop で停止)。起動時ONなら最初から立てておく(一覧が届くまでは「時刻取得中…」)。
    let mut radar_clock: Option<radar::RadarClock> =
        radar_on.then(|| radar::start_clock(radar_refresh_secs(&cfg)));
    let mut play: Option<f64> = None; // A ルート再生(先頭からの距離m。Noneで停止)
    let mut play_speed: f64 = 1.0;    // 再生速度倍率(再生中に [ ] で 0.25〜8x)
    let mut play_last_tick: Option<std::time::Instant> = None; // 再生の実時間ベース進行用(前回フレームの時刻)
    // 実画像モードでのルート再生ちらつき対策: 先読みスレッドがbuild_window(重い/ネットワーク)を
    // 事前に進めておき、メインは受け取った画像を使うだけにする(無ければ従来通り同期取得にfallback)。
    let mut play_prefetch_rx: Option<std::sync::mpsc::Receiver<(f64, RgbImage)>> = None;
    let mut play_prefetch_held: Option<(f64, RgbImage)> = None;
    let mut play_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut play_speed_bits = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1.0f64.to_bits()));
    let mut play_wants_prefetch = false; // 再生開始直後に一度だけ、rw/rh/rz確定後に先読みスレッドを起こすフラグ
    let mut scache = searchcache::load(); // 検索結果キャッシュ(キーワード+位置→結果。API節約)
    let mut popup: Option<String> = None; // 中央に出す一時ポップアップ(スポット名等・任意キーで閉じる)
    // ルート計算のバックグラウンド受信(マーカーは即時、ルート線は別スレッド)
    let (mut route_note, mut route_job) = {
        // ループ開始前(regulation_layerがまだ何も取得していない)なので通行止め回避は無し。
        let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, "");
        (n_, j_)
    };
    // ルート音声案内(cfg.voice_guide_enabled時のみ使う)。曲がり角一覧はルートが決まる
    // (route_jobが完了する)たびに背景取得し直す。voice_guideはturn_pointsと対で持ち、
    // ルートが変わったら作り直す(VoiceGuide::matches_lenで長さ不一致を検知)。
    let mut turn_points: Vec<route::TurnPoint> = Vec::new();
    let mut turn_job: Option<route::TurnRx> = None;
    let mut voice_guide: Option<voice::VoiceGuide> = None;
    // 気象警報(#79・ルートベース)。turn_jobと同じ「ルート確定時」フックで作り直す。
    let mut route_warnings: Vec<warning::ActiveWarning> = Vec::new();
    let mut route_warning_job: Option<std::sync::mpsc::Receiver<Vec<warning::ActiveWarning>>> = None;
    // 地図に重ねる7種のプロットデータ。取得単位(メッシュ/整備局/都道府県)・TTL・ズーム下限・
    // ディスク永続化はすべて plotlayer/plotcache 側が持つ。ここは毎フレーム tick して結果を読むだけ。
    // 道路交通量は cfg.traffic_enabled、カメラは camera_enabled、規制は regulation_enabled、
    // 過去災害は disaster_enabled、500mメッシュ人口は population_enabled で ON/OFFする。
    // 主要道路(#73)は交通量の観測点をラインへスナップする下地なので交通量と連動する。
    let mut traffic_layer = plotlayer::traffic();
    let mut roads_layer = plotlayer::roads();
    let mut camera_layer = plotlayer::camera();
    let mut regulation_layer = plotlayer::regulation();
    let mut disaster_layer = plotlayer::disaster();
    // 市区町村境界(気象庁 class20s)。過去災害を塗り分ける(コロプレス)ためだけの下地なので、
    // 過去災害がONでかつ塗りがONのときだけ取りに行く。
    let mut boundary_layer = plotlayer::boundary();
    let mut population_layer = plotlayer::population();
    // 期限切れ/上限超過のキャッシュ掃除は1セッション1回、最初のアイドル到達時に別スレッドで走らせる
    // (起動を遅くせず、無操作のたびにディレクトリを走査もしない)。
    let mut plot_gc_done = false;
    // Nキーで中心近くのカメラを選び、フル画像を取得して全画面表示する(実写Street Viewと同じ
    // 早期returnパターン)。パン/ズームは無い(道路カメラは固定視点の1枚画像のため)。
    let mut cam_view: Option<(RgbImage, camera::RoadCamera)> = None;
    let mut cam_job: Option<std::sync::mpsc::Receiver<(camera::RoadCamera, Result<RgbImage, String>)>> = None;
    // Bキーで中心近くの災害履歴の地点を選び、その地点の事例一覧(2段目)を取って中央パネルに出す。
    // 集計(1段目)には事例の名称も日付も入っていないので、押したときだけ引く。結果は保存しない。
    let mut disaster_view: Option<(String, Vec<String>)> = None; // (見出し, 本文行)
    let mut disaster_job: Option<std::sync::mpsc::Receiver<Result<(String, Vec<String>), String>>> = None;
    // 通行規制の詳細(Tキー。なぜ通れないかの規制原因等)。disaster_viewと同じ「見出し+本文行」形。
    let mut regulation_detail_view: Option<(String, Vec<String>)> = None;
    let mut regulation_detail_job: Option<std::sync::mpsc::Receiver<Result<regulation::ClosureDetail, String>>> = None;
    // 渋滞状況の色分け(#渋滞情報)。ルート成功のたびに、設定ONならGoogle Directionsへ別途確認する。
    let mut traffic_color_job: Option<route::TrafficColorRx> = None;
    // 規制原因アイコン(事故✕/工事)。表示中のClosedイベントについて1件ずつ規制原因を
    // バックグラウンドで取得し分類する(セッション内メモリのみ、無期限保持)。
    // 結果にdetail_idを添えて返す(ClosureDetail自体はidを持たないため紐付けに必要)。
    let mut cause_cache: std::collections::HashMap<String, regulation::CauseCategory> = std::collections::HashMap::new();
    let mut cause_job: Option<std::sync::mpsc::Receiver<(String, Result<regulation::ClosureDetail, String>)>> = None;
    // 読み上げの声(#78)の試聴。SettingsPick(27)でSpace=試聴/Enter確定後の1回再生の両方で使う。
    let mut voice_preview_job: Option<std::sync::mpsc::Receiver<Result<(), String>>> = None;
    // ルート計算と同じ非同期パターンで、検索/周辺/実写/おすすめの通信もバックグラウンド化する。
    // 新規spawn時に古いrxはdropされる=最新のみ採用(generation ID不要)。
    let mut search_job: Option<std::sync::mpsc::Receiver<(String, String, Result<Vec<(f64, f64, String)>, String>)>> = None; // (ckey, query, geocode結果)
    let mut near_job: Option<std::sync::mpsc::Receiver<(String, Result<Vec<(f64, f64, String)>, ApiError>)>> = None; // (query, search_nearbyのosm結果)
    let mut street_job: Option<std::sync::mpsc::Receiver<(f64, f64, i32, Result<image::RgbImage, String>)>> = None; // (lat, lon, heading, 実写画像)
    let mut recommend_job: Option<std::sync::mpsc::Receiver<Result<Vec<(f64, f64, String)>, String>>> = None; // 実在確認済みスポット列
    let mut road_job: Option<std::sync::mpsc::Receiver<(String, Result<Vec<(Vec<(f64, f64)>, bool)>, String>)>> = None; // (道路名, roadsearch::fetch結果)
    let mut wander_job: Option<std::sync::mpsc::Receiver<Result<Vec<(f64, f64)>, String>>> = None; // おまかせ周回(wander_route)結果
    let mut catpoi_job: Option<std::sync::mpsc::Receiver<(String, Result<Vec<(f64, f64, String, PoiCat)>, ApiError>)>> = None; // (カテゴリ名, poi_search結果)。ラベルは起動時に確定して送るので途中でpoi_kindsを編集されても安全
    let mut spin: usize = 0; // 通信中スピナーのフレーム(毎ループ+1)
    let mut poi_kinds: Vec<PoiKind> = load_poi_kinds(); // 目的地カテゴリ(並べ替え/追加/削除可・~/.config/termmap/poi-kinds.txt)
    let mut spots = load_spots();          // マイスポット
    let mut spot_cats = load_spot_cats();
    let mut show_spots = cfg.show_spots; // 前回終了時の表示/非表示を引き継ぐ
    let mut sp_sel: usize = 0;
    let mut cat_sel: usize = 0;
    let mut cur_cat = String::new(); // スポット一覧で表示中のカテゴリ
    let mut pending_spot: Option<(f64, f64, String)> = None; // 検索結果からお気に入り登録する際の保留(座標+名前)。カテゴリ選択待ち
    let mut list_offset: usize = 0; // 左袖リストのスクロール開始位置(表示中の1リストで共有・ensure_visibleで追従)
    let mut color_sel: u8 = 0; // 色ピッカーで選択中のパレットindex
    let mut shape_sel: u8 = 0; // 形状ピッカーで選択中の形状index
    let mut set_pick_sel: usize = 0; // 設定画面の一覧選択(SettingsPick)で選択中の候補index
    let mut onboard = onboarded_marker().map_or(false, |p| !p.exists()); // 初回起動なら操作案内を出す
    let mut spot_move_confirm: Option<usize> = None; // m(中心へ移動)の確認待ち。上書きは破壊的なのでy/nを挟む
    let mut save_confirm: Option<String> = None; // 保存名が既存の場合の上書き確認待ち(y=上書き/他=名前を変更して新規登録)
    let mut clear_route_confirm = false; // c(ルート全消去)の確認待ち(y=消去/他=取消)
    let mut route_name_hint = String::new(); // 直近に読み込み/保存したルート名(Sで保存欄を開く際そのまま出す)
    let mut quit_confirm = false; // Map で Esc二連打 → 終了確認(y=終了/他=取消)
    let mut last_esc_at: Option<std::time::Instant> = None; // 直前のEsc押下時刻(二連打判定用)
    apply_spots(&mut spec, &spots, &spot_cats, show_spots);
    // 操作UI効果音(macOS afplay)。設定OFF/非macOS/afplay不在なら no-op。設定トグルで作り直す。
    let mut snd = sound::Sound::new(cfg.sound_enabled);

    // ズーム変更(+/-)直後に呼ぶ。再生(play)中は先読みスレッドが再生開始時のズームを
    // 捕まえたまま動き続けるため、そのままだと「古いズームの先読み画像」と「新ズーム基準の
    // オーバーレイ(クロスヘア/ルート線)」でスケールが食い違い表示が壊れる。再生中にズームが
    // 変わったら先読みを取消し、次フレームで新ズームを使って再起動する(再生距離playは維持)。
    macro_rules! restart_prefetch_on_zoom { () => {
        if play.is_some() {
            play_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            play_prefetch_rx = None;
            play_prefetch_held = None;
            play_wants_prefetch = true;
        }
    }; }

    // 雨雲レーダーをONにする(C と > の共通処理)。時刻一覧の背景ポーラーがまだ無ければ起こし、
    // 表示は必ず最新の実況(now_idx)から始める。一覧が未着なら idx=0 のまま「時刻取得中…」になる。
    macro_rules! radar_turn_on { () => {
        radar_on = true;
        if radar_clock.is_none() { radar_clock = Some(radar::start_clock(radar_refresh_secs(&cfg))); }
        radar_idx = radar_tl.now_idx.min(radar_tl.frames.len().saturating_sub(1));
        radar_follow = true;
        addr = "雨雲レーダー: ON (出典: 気象庁ナウキャスト)".into();
    }; }

    // 雨雲レーダーの ON/OFF を反転する(Spaceメニューの「雨雲レーダー」と設定画面の行から使う。
    // 地図での C キーも同じ処理)。OFFにするとき背景ポーラーの drop はスレッドを join するため、
    // 取得中(HTTPは最大20秒)にここで待つと入力が固まる。停止フラグは drop 側で即座に立つので、
    // join だけを別スレッドへ逃がしてUIを待たせない。
    // 500mメッシュ人口の表示/非表示。雨雲と違い背景ポーラーを持たないので、設定を反転して
    // 保存するだけでよい(次の tick が cfg.population_enabled を見てセルを取りに行く)。
    // ONにした直後は出典と、取得が重いことを1回だけ知らせる(31MBが無言で落ちないように)。
    macro_rules! population_toggle { () => {
        cfg.population_enabled = !cfg.population_enabled;
        let _ = config::save_config(&cfg);
        addr = if cfg.population_enabled {
            format!("人口メッシュ: ON({}年) {}", cfg.population_year, population::ATTRIBUTION)
        } else {
            "人口メッシュ: OFF".to_string()
        };
        // 再描画は cfg.population_enabled を map_sig に混ぜてあるので自動で起きる(force_reemit不要)。
    }; }

    macro_rules! radar_toggle { () => {
        if radar_on {
            radar_on = false;
            if let Some(rc) = radar_clock.take() { std::thread::spawn(move || drop(rc)); }
            addr = "雨雲レーダー: OFF".into();
        } else {
            radar_turn_on!();
        }
    }; }

    // メニュー項目/直接キー どちらからでも同じ処理を走らせる。
    // lat/lon/cols/tr/route_nogos は各ループで再計算されるフレーム値。マクロ衛生性のため引数で受け取る。
    macro_rules! run_action { ($act:expr, $lat:expr, $lon:expr, $cols:expr, $tr:expr, $nogos:expr) => {{
        match $act {
            MenuAction::SearchPlace => { input_cur = 0; focus = Focus::Search(String::new()); }
            MenuAction::SearchPoi => { focus = Focus::PoiMenu; }
            MenuAction::ShowAddress => { addr = reverse_geocode($lat, $lon).unwrap_or_else(|e| format!("({e})")); }
            MenuAction::Recommend => {
                if !cfg.llm_recommend_enabled { snd.play("error"); addr = "おすすめ: 設定でOFF(,でON)".into(); }
                else if !recommend::claude_available(&cfg.llm_command) { snd.play("error"); addr = "おすすめ: claudeが無い(設定のLLM/コマンド確認)".into(); }
                else { input_cur = 0; focus = Focus::Recommend(String::new()); }
            }
            MenuAction::RouteForm => { if wps.is_empty() { addr = "先に v で地点を置いてね".into(); } else { wp_sel = 0; grab = false; focus = Focus::WaypointList; } }
            MenuAction::AddVia => { snd.play("pop"); wp_add(&mut wps, ($lat, $lon)); let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, $nogos); route_note = n_; route_job = j_; addr = format!("地点を追加 #{}", wps.len()); }
            MenuAction::RoadRoute => { input_cur = 0; focus = Focus::RoadSearch(String::new()); }
            MenuAction::Wander => { focus = Focus::WanderForm { dist_km: a.dist.unwrap_or(40.0) }; } // 距離ゲージを開く(Enterで検索開始)
            MenuAction::CycleMode => { mode = match mode_label(&mode) { "下道" => "highway", "高速" => "short", _ => "surface" }.to_string(); let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, $nogos); route_note = n_; route_job = j_; }
            MenuAction::AltRoute => {
                if wps.len() >= 2 {
                    route_alt = (route_alt + 1) % 4;
                    let (nn, jj) = trigger_route(&mut spec, &wps, &pois, &mode, route_alt, &cfg.google_maps_api_key, $nogos);
                    route_note = nn; route_job = jj;
                } else { snd.play("error"); addr = "ルート未確定".into(); }
            }
            MenuAction::ClearRoute => { if !wps.is_empty() || !road_segs.is_empty() { clear_route_confirm = true; } }
            MenuAction::ManageRoads => { if road_segs.is_empty() { snd.play("error"); addr = "道路の塊がまだ無い(rで道路を追加)".into(); } else { road_sel = 0; focus = Focus::RoadList; } }
            MenuAction::ManageSpots => { cat_sel = 0; focus = Focus::SpotCatList; }
            MenuAction::ToggleSpots => { show_spots = !show_spots; apply_spots(&mut spec, &spots, &spot_cats, show_spots); addr = if show_spots { "マイスポット表示".into() } else { "マイスポット非表示".into() }; }
            MenuAction::ToggleElevation => {
                show_elev = !show_elev;
                if show_elev && (spec.routes.is_empty() || !route_ele.iter().any(|&z| z != 0.0)) { addr = "標高: ルート確定後に表示".into(); }
            }
            MenuAction::StreetView => {
                if !streetview::available(&cfg.google_maps_api_key) { snd.play("error"); addr = "実写: APIキー未設定(config.toml [streetview])".into(); }
                else {
                    // 実写取得を別スレッドへ。focus は Map のまま(メニューは既に閉じている)でスピナーが回る。
                    sv_fov = 90.0; // 開き直しなので既定ズームに戻す
                    let (la, lo) = ($lat, $lon);
                    let key = cfg.google_maps_api_key.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let r = streetview::fetch(la, lo, 0, 640, 480, 90.0, &key);
                        let _ = tx.send((la, lo, 0, r));
                    });
                    street_job = Some(rx);
                }
            }
            MenuAction::PlayRoute => {
                if spec.routes.last().map_or(false, |r| r.pts.len() >= 2) {
                    if play.is_some() {
                        play = None; play_last_tick = None;
                        play_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        play_prefetch_rx = None; play_prefetch_held = None;
                        addr = "再生: 停止".into();
                    } else {
                        play = Some(0.0);
                        play_last_tick = Some(std::time::Instant::now());
                        play_wants_prefetch = true; // 実画像モードなら次フレームで先読みスレッドを起動する
                        addr = "再生: 開始(Aで停止)".into();
                    }
                } else { snd.play("error"); addr = "再生: ルート未確定".into(); }
            }
            MenuAction::ToggleGps => {
                if gps_rx.is_some() { gps_rx = None; addr = "ライブ現在地: OFF".into(); }
                else {
                    let bin = if std::path::Path::new("/opt/homebrew/bin/CoreLocationCLI").exists() { "/opt/homebrew/bin/CoreLocationCLI" } else { "CoreLocationCLI" };
                    if gpslive::available(bin) { gps_rx = Some(gpslive::start_poller(bin.to_string(), 5)); gps_trail.clear(); gps_pos = None; addr = "ライブ現在地: ON(5秒ごと)".into(); }
                    else { addr = "ライブ: CoreLocationCLI無し(brew install corelocationcli)".into(); }
                }
            }
            MenuAction::ToggleRadar => { radar_toggle!(); } // 雨雲レーダー(地図の C キーと同じ)
            MenuAction::TogglePopulation => { population_toggle!(); } // 500mメッシュ人口(地図の U キーと同じ)
            MenuAction::ToggleDisasterFill => { disaster_fill_toggle!(); } // 過去災害の塗り(地図の F キーと同じ)
            MenuAction::ViewCamera => { // 道路ライブカメラ(地図の N キーと同じ)
                if !cfg.camera_enabled { snd.play("error"); addr = "道路ライブカメラ: OFF(設定で有効化)".into(); }
                else {
                    // 視野内で中心に一番近いカメラ。ここで層から直接引くのは、フレーム先頭で
                    // 切り出した一覧の借用がこの時点(tick後)まで生きていられないため。
                    let nearest = camera_layer.items(plotlayer::view_bbox(cx, cy, z)).into_iter()
                        .min_by(|a, b| {
                            let da = (a.lat - $lat).powi(2) + (a.lon - $lon).powi(2);
                            let db = (b.lat - $lat).powi(2) + (b.lon - $lon).powi(2);
                            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .cloned();
                    match nearest {
                        None => { snd.play("error"); addr = "道路ライブカメラ: 周辺に無し".into(); }
                        Some(c) => {
                            // キャッシュから読んだカメラは写真URLを持たない(URLに15分ごとの撮影
                            // ディレクトリが入るため保存していない)。その場合だけ整備局ページを
                            // 取り直してURLを補ってから画像を取る(押したときだけの1回)。
                            if c.full_url.is_none() { addr = "📷カメラ情報を更新中…".into(); }
                            let (tx, rx) = std::sync::mpsc::channel();
                            std::thread::spawn(move || {
                                let c = match c.full_url {
                                    Some(_) => c,
                                    None => camera::fetch_bureau(camera::nearest_bureau(c.lat, c.lon))
                                        .ok()
                                        .and_then(|cams| cams.into_iter().find(|x| x.id == c.id))
                                        .unwrap_or(c),
                                };
                                let r = match c.full_url.clone() {
                                    Some(url) => camera::fetch_image(&url),
                                    None => Err("画像URLを取得できない".to_string()),
                                };
                                let _ = tx.send((c, r));
                            });
                            cam_job = Some(rx);
                        }
                    }
                }
            }
            MenuAction::SaveRoute => { input_cur = route_name_hint.chars().count(); focus = Focus::SaveName(route_name_hint.clone()); }
            MenuAction::LoadRoute => { route_names = list_named_routes(); rn_sel = 0; if route_names.is_empty() { addr = "お気に入り無し".into(); } else { focus = Focus::RouteList; } }
            MenuAction::SaveGpx => match spec.routes.last() {
                Some(rt) => addr = match write_gpx("termmap-route.gpx", &rt.pts) { Ok(_) => "GPX保存: termmap-route.gpx".into(), Err(e) => format!("({e})") },
                None => { snd.play("error"); addr = "ルート未確定".into(); }
            },
            MenuAction::ShareQr => {
                if wps.len() >= 2 {
                    let (url, _) = gmaps_url(&wps);
                    match qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L) {
                        Ok(c) => qr_view = Some(build_qr_view(&c, &cfg.qr_style)),
                        Err(_) => addr = "QR生成失敗".into(),
                    }
                } else { snd.play("error"); addr = "ルート未確定".into(); }
            }
            MenuAction::Settings => { set_sel = 0; focus = Focus::Settings; voice::warm_voice_list(); }
            MenuAction::Help => { help = true; help_page = 0; }
        }
    }};}

    // road_segs の変更後に描画用の spec.roads を作り直す(trigger_route等では消えない別レイヤ)。
    macro_rules! sync_roads { () => {
        spec.roads = road_segs.iter().map(|r| Route { pts: r.pts.clone(), color: r.color, thickness: 2 }).collect();
    };}

    // 実画像モードの再emit抑制。直近にemitした地図画像の状態シグネチャを保持し、変化が無い
    // フレームでは PNG を吐き直さない(チラつき/負荷の回避)。force_reemit は popup/ヘルプ/実写
    // など地図矩形を覆う描画の後に、残像を消すため次フレームで1度だけ強制再emitさせる。
    let mut last_map_sig: Option<u64> = None;
    let mut force_reemit = true;
    let mut prev_map_covered = false; // map_coveredの立ち上がり/下がりエッジ検出用(被ってる間は毎フレーム強制しない)
    // 移動検知: 直近に描画した(cx,cy,z)と比べて動いていれば低解像度・止まって一定時間(350ms)経てば設定解像度へ。
    let mut prev_render_cxyz: Option<(f64, f64, u32)> = None;
    let mut moved_at: Option<std::time::Instant> = None;
    let mut emit_count: u64 = 0; // 実画像emit回数。一定間隔でscrollbackを掃除しメモリ肥大を防ぐ
    // 地図パン: 既定は細かい1歩、同方向を短間隔で連打/押しっぱなしすると徐々に加速する。
    let mut pan_streak: u32 = 0;
    let mut last_pan_dir: Option<KeyCode> = None;
    let mut last_pan_at = std::time::Instant::now();
    // web版(ブラウザ)のドラッグ軸モード通知(#87)。前回送った値を覚えておき、変わったフレーム
    // だけ OSC 9997 を送る。req_pending はブラウザからの再送要求(DRAGMODE?)を受けた印で、
    // 値が変わっていなくても次フレームで1回送らせる。
    let mut prev_drag_axes: Option<(dragmode::Axis, dragmode::Axis)> = None;
    let mut drag_mode_req_pending = false;
    // 端末1セルの縦横比(セル高/セル幅)。web版(ブラウザ)から CELL マーカーで届いた値を覚えておく
    // (設計書 docs/web-image-aspect-ratio-design.md §7.2 の経路2)。ネイティブ端末は毎フレーム
    // window_size() から取れるので保持しない。どちらも無ければ既定値 2.0 へ落ちる。
    let mut cell_ratio_web: Option<f64> = None;

    // 過去災害の塗り(コロプレス)の ON/OFF を反転する(Spaceメニュー・設定画面・地図での F キーの
    // 3経路共通処理、population_toggle!と同じ構成)。ONにした直後だけ境界データの出典を1回出す。
    // force_reemit を使うため、その宣言(上の let mut force_reemit)より後に置く必要がある
    // (macro_rules! の自由識別子はマクロが定義された位置で見えている変数に解決されるため)。
    macro_rules! disaster_fill_toggle { () => {
        cfg.disaster_fill = !cfg.disaster_fill;
        let _ = config::save_config(&cfg);
        force_reemit = true; // 今表示している地図の見た目が変わる
        addr = if cfg.disaster_fill && cfg.disaster_enabled {
            "過去災害の塗り: 市区町村境界 気象庁".to_string()
        } else if cfg.disaster_fill {
            "過去災害の塗り: ON(「過去災害」もONにすると出る)".to_string()
        } else {
            "過去災害の塗り: OFF".to_string()
        };
    }; }

    let _ = write!(out, "\x1b[2J");
    loop {
        spin = spin.wrapping_add(1); // 通信中スピナーのアニメ用(毎フレーム進める)
        let (tc, tr) = crossterm::terminal::size().unwrap_or((100, 40));
        let cols = tc.max(20) as u32;
        let map_rows = (tr.max(3) - 1) as u32;
        // このフレームで使う端末セル比。ネイティブ端末(window_size)→ブラウザ通知(CELLマーカー)→
        // 既定 2.0 の順(設計書 §7.2)。フォントサイズ変更や画面回転に毎フレーム追随させたいので
        // キャッシュせず都度引く(window_size は terminal::size と同じ ioctl 1回で、上の size()
        // 呼び出しと同程度のコスト)。
        let cell_ratio = cellratio::resolve_ratio(cellratio::detect_native_ratio(), cell_ratio_web);
        if help { // ヘルプ全画面。画面高に収まらなければページ送り(最終ページで任意キー→閉じる)
            let _ = write!(out, "\x1b[2J\x1b[H");
            for (i, (bold, (r, g, b), ln)) in LOGO.iter().enumerate() { // 先頭に緑ワードマーク
                let bold_code = if *bold { "\x1b[1m" } else { "" };
                let col = format!("{bold_code}{}", sgr_fg(*r, *g, *b, truecolor_safe()));
                let _ = write!(out, "\x1b[{};2H{}{}\x1b[0m\x1b[K", i + 1, col, ln);
            }
            let off = LOGO.len() + 1; // ロゴ4行 + 空行1
            let per_page = (map_rows as usize).saturating_sub(off).max(1);
            let content_len = HELP.len().saturating_sub(1); // 先頭行(見出し)はLOGOと重複するため除く
            let total_pages = content_len.div_ceil(per_page).max(1);
            help_page = help_page.min(total_pages - 1);
            for (i, l) in HELP.iter().skip(1 + help_page * per_page).enumerate().take(per_page) {
                let _ = write!(out, "\x1b[{};1H{}\x1b[K", i + off + 1, l);
            }
            let has_more = help_page + 1 < total_pages;
            let hint = if total_pages > 1 {
                if has_more { format!(" {}/{} ページ (任意のキーで次へ) ", help_page + 1, total_pages) }
                else { format!(" {}/{} ページ (任意のキーで閉じる) ", help_page + 1, total_pages) }
            } else { " 任意のキーで閉じる ".to_string() };
            let _ = write!(out, "\x1b[{};1H\x1b[7m{hint}\x1b[0m\x1b[K", tr);
            let _ = out.flush();
            if let Event::Key(_) = event::read()? {
                if has_more { help_page += 1; } else { help = false; help_page = 0; }
            }
            force_reemit = true; // ヘルプで全画面クリアした→地図に戻ったら画像を再emit
            continue;
        }
        if street.is_some() { // 実写(Street View)全画面。←→で向き、Esc/qで戻る
            { // 描画(不変借用のスコープ)
                let (img, heading, slat, slon) = street.as_ref().unwrap();
                // 実写は 640x480(4:3)の写真で、地図と違って端末の形に合わせて生成していない。
                // 端末全体のセル矩形へそのまま強制フィットすると、iPhone 縦持ちのような縦長端末
                // では縦に約2.6倍伸びる(設計書 §4.1)。写真の縦横比を保ったまま黒帯付きの1枚へ
                // 合成してから出す(設計書 §7.6)。実画像モードもAAフォールバックも同じ考え方。
                let shown = cellratio::crop_photo(img, cols.max(10), map_rows, cell_ratio);
                if cfg.image_mode && image_capable() {
                    // 実画像モード: 実写を全幅×map_rows のインライン画像で表示
                    let _ = write!(out, "\x1b[H");
                    let _ = emit_iterm2_image(&mut out, &shown, cols, map_rows);
                } else {
                    let rs = image::imageops::resize(&shown, cols.max(10), map_rows * 2, FilterType::Triangle);
                    let art = render_halfblock(&rs, truecolor_safe());
                    let sv_lines: Vec<&str> = art.split("\r\n").collect();
                    let _ = write!(out, "\x1b[H");
                    for i in 0..map_rows as usize {
                        let ln = sv_lines.get(i).copied().unwrap_or("");
                        let _ = write!(out, "\x1b[{};1H{}\x1b[K", i + 1, ln);
                    }
                }
                let hd = ((heading % 360) + 360) % 360;
                let arrow = heading_arrow(hd as f64);
                let st = fit_cells_scroll(&format!(" 実写 {arrow} h{hd}° fov{sv_fov:.0}°  ←→向き ↑↓移動(地図も追従) +/-ズーム (Shiftで微調整)  Esc/q戻る  {slat:.4},{slon:.4} "), cols as usize, spin);
                let _ = write!(out, "\x1b[{};1H\x1b[7m{st}\x1b[0m\x1b[K", tr);
                let _ = out.flush();
            }
            let (hd_c, slat_c, slon_c) = { let (_, h, la, lo) = street.as_ref().unwrap(); (*h, *la, *lo) };
            // 押しっぱなし/連打で溜まった同種キーは最新の1個へ間引く(#4のMap focusと同じ理由)。
            // Esc/q等の別系統キーが混ざっていたら間引きを止めてそちらを即座に優先する。
            let is_sv_key = |c: KeyCode| matches!(c, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char('-') | KeyCode::Char('_'));
            let mut ev = event::read()?;
            if let Event::Key(first) = &ev {
                if is_sv_key(first.code) {
                    while let Ok(true) = event::poll(std::time::Duration::from_millis(0)) {
                        match event::read()? {
                            Event::Key(next) if is_sv_key(next.code) => ev = Event::Key(next),
                            other => { ev = other; break; }
                        }
                    }
                }
            }
            // この画面は下の match まで進まず continue するので、セル比の通知はここで取り込む。
            // 取りこぼすと、写真を開いたまま文字サイズを変えたときだけ古い比のまま固定される
            // (JS 側は同じ値を再送しないため、地図へ戻っても直らない)。
            if let Event::Paste(s) = &ev {
                if let Some(r) = cellratio::parse_cell_marker(s) { cell_ratio_web = Some(r); }
            }
            if let Event::Key(k) = ev {
                match k.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                        // ←→=向き回転(既定10°・Shiftで45°) / ↑↓=向き方向に前後移動(既定5m・Shiftで20m)
                        // Map focusのパン(既定=細かく・Shiftで常に高速)と同じ規則に揃える。
                        let fine = !k.modifiers.contains(KeyModifiers::SHIFT);
                        let rot = if fine { 10 } else { 45 };
                        let dist = if fine { 5.0 } else { 20.0 };
                        let (nlat, nlon, nhd) = match k.code {
                            KeyCode::Left => (slat_c, slon_c, hd_c - rot),
                            KeyCode::Right => (slat_c, slon_c, hd_c + rot),
                            KeyCode::Up => { let (a, b) = streetview::step(slat_c, slon_c, hd_c as f64, dist); (a, b, hd_c) }
                            _ => { let (a, b) = streetview::step(slat_c, slon_c, hd_c as f64 + 180.0, dist); (a, b, hd_c) }
                        };
                        if let Ok(im) = streetview::fetch(nlat, nlon, nhd, 640, 480, sv_fov, &cfg.google_maps_api_key) {
                            street = Some((im, nhd, nlat, nlon)); // Err時は現状維持(行き止まり等)
                            // 地図連動: 前後移動(↑↓)で歩いた先に地図の中心も追従させる。実写を
                            // 閉じたとき、元の地点でなく実際に歩いた地点で地図が表示されるようにする。
                            if matches!(k.code, KeyCode::Up | KeyCode::Down) {
                                let (nx, ny) = deg_to_pixel(nlat, nlon, z);
                                cx = nx; cy = ny;
                            }
                        }
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char('-') | KeyCode::Char('_') => {
                        // ズーム: fov(画角)を上げ下げ。小さいほどズームイン。Shiftで細かく調整
                        let fine = k.modifiers.contains(KeyModifiers::SHIFT);
                        let step = if fine { 5.0 } else { 10.0 };
                        let zoom_in = matches!(k.code, KeyCode::Char('+') | KeyCode::Char('='));
                        let nfov = (sv_fov + if zoom_in { -step } else { step }).clamp(20.0, 100.0);
                        if let Ok(im) = streetview::fetch(slat_c, slon_c, hd_c, 640, 480, nfov, &cfg.google_maps_api_key) {
                            sv_fov = nfov;
                            street = Some((im, hd_c, slat_c, slon_c));
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => street = None,
                    KeyCode::Char('I') => { // 実写表示中も画像モードON/OFFを切替できるように(Map focusと同じキー)
                        cfg.image_mode = !cfg.image_mode;
                        addr = if cfg.image_mode {
                            if image_capable() { "実画像モード: ON".into() } else { "実画像モード: ON(この端末は非対応・AA継続)".into() }
                        } else { "実画像モード: OFF".into() };
                    }
                    _ => {}
                }
            }
            force_reemit = true; // 実写で全画面を覆った→地図に戻ったら画像を再emit
            continue;
        }
        if cam_view.is_some() { // 道路ライブカメラの写真を全画面表示。streetと同じ早期returnパターン
            // 道路カメラは固定視点の1枚画像なのでstreetと違いパン/ズームは無い(Esc/qで戻るのみ)。
            { // 描画(不変借用のスコープ)
                let (img, cam) = cam_view.as_ref().unwrap();
                // 実写と同じく、写真の縦横比を保ったまま黒帯付きの1枚へ合成してから出す
                // (設計書 §7.6)。カメラ画像は提供元により縦横比が違うので 4:3 決め打ちにしない。
                let shown = cellratio::crop_photo(img, cols.max(10), map_rows, cell_ratio);
                if cfg.image_mode && image_capable() {
                    let _ = write!(out, "\x1b[H");
                    let _ = emit_iterm2_image(&mut out, &shown, cols, map_rows);
                } else {
                    let rs = image::imageops::resize(&shown, cols.max(10), map_rows * 2, FilterType::Triangle);
                    let art = render_halfblock(&rs, truecolor_safe());
                    let cam_lines: Vec<&str> = art.split("\r\n").collect();
                    let _ = write!(out, "\x1b[H");
                    for i in 0..map_rows as usize {
                        let ln = cam_lines.get(i).copied().unwrap_or("");
                        let _ = write!(out, "\x1b[{};1H{}\x1b[K", i + 1, ln);
                    }
                }
                let st = fit_cells_scroll(&format!(" 道路カメラ {}({})  Esc/q戻る  {:.4},{:.4} ", cam.name, cam.taken_at, cam.lat, cam.lon), cols as usize, spin);
                let _ = write!(out, "\x1b[{};1H\x1b[7m{st}\x1b[0m\x1b[K", tr);
                let _ = out.flush();
            }
            let ev = event::read()?;
            // 実写画面と同じ理由でセル比の通知をここで取り込む(下の match まで進まないため)。
            if let Event::Paste(s) = &ev {
                if let Some(r) = cellratio::parse_cell_marker(s) { cell_ratio_web = Some(r); }
            }
            if let Event::Key(k) = ev {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => cam_view = None,
                    KeyCode::Char('I') => { // 表示中も画像モードON/OFFを切替できるように(Map focusと同じキー)
                        cfg.image_mode = !cfg.image_mode;
                        addr = if cfg.image_mode {
                            if image_capable() { "実画像モード: ON".into() } else { "実画像モード: ON(この端末は非対応・AA継続)".into() }
                        } else { "実画像モード: OFF".into() };
                    }
                    _ => {}
                }
            }
            force_reemit = true;
            continue;
        }
        // 標高プロファイル帯を出すぶん地図の行数を減らす(E)
        let elev_on = show_elev && !spec.routes.is_empty() && route_ele.len() >= 2 && route_ele.iter().any(|&z| z != 0.0);
        let elev_h: u32 = if elev_on { (map_rows / 3).clamp(4, 12) } else { 0 };
        let map_rows = if elev_h > 0 { map_rows.saturating_sub(elev_h + 1).max(3) } else { map_rows };
        let show_routes = matches!(focus, Focus::RouteList);
        let show_wps = matches!(focus, Focus::WaypointList);
        let show_route = (matches!(focus, Focus::Map) && !wps.is_empty() && !route_panel_hidden) || matches!(focus, Focus::RoutePanel); // 地点一覧を左袖に(Map中・R非表示でなければ/パネルフォーカス中は常に)
        let show_splist = matches!(focus, Focus::SpotList);
        let show_catlist = matches!(focus, Focus::SpotCatList);
        let show_settings = matches!(focus, Focus::Settings | Focus::SettingsPick(_));
        let show_menu = matches!(focus, Focus::Menu(_));
        let show_poimenu = matches!(focus, Focus::PoiMenu);
        let show_roadlist = matches!(focus, Focus::RoadList);
        let show_favmenu = matches!(focus, Focus::RouteFavMenu { .. });
        let gut: u32 = if !pois.is_empty() || show_routes || show_wps || show_route || show_splist || show_catlist || show_settings || show_menu || show_poimenu || show_roadlist || show_favmenu { 28 } else { 0 };
        let map_cols = cols.saturating_sub(gut).max(10);
        let (ow, oh) = if opts.braille || opts.edge { (map_cols * 2, map_rows * 4) } else { (map_cols, map_rows * 2) };
        if let Some(p) = &gps_rx { // ライブ現在地を取り込み、自位置に追従
            while let Ok((la, lo)) = p.rx.try_recv() {
                gps_pos = Some((la, lo));
                gps_trail.push((la, lo));
                if gps_trail.len() > 300 { gps_trail.remove(0); }
                let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny;
                maybe_speak_turn(&cfg, &spec, &turn_points, &mut voice_guide, (la, lo));
            }
        }
        let img_inline = cfg.image_mode && image_capable(); // 実画像モード(iTerm2系端末のみ)。play処理より先に要る
        // 雨雲の合成方式は描画モードで2系統に分かれる(設計 §2.1)。どのモードでも表示はできる。
        //   実画像 / halfblock … 地図へ直接アルファ合成(下の地図が透ける)
        //   classify         … recolor()で6色へ量子化した「後」に合成(先に混ぜると淡い青の降水が湖に化ける)
        //   braille / edge   … 背景色の概念が無いので OverlayLayer へディザ間引きしたインクとして焼く
        // mono は単体では描画経路を変えない(render_braille の色を落とすだけ)ので、braille/edge が
        // 立っていなければ halfblock と同じアルファ合成になる。
        let radar_ink = !img_inline && (opts.braille || opts.edge);
        if play.is_some() { // ルート再生: 実時間ベースで位置を進めて自動パン(想定巡航速度×play_speed倍率)
            // 実画像モードは先読みスレッドが返した画像をベース地図に使う。オーバーレイ(ルート線/
            // クロスヘア)をそれと違う位置で描くと、ベースとオーバーレイがズレてルートがガタつい
            // て見えるバグになるため、その画像が実際に描かれた位置(frame_d)を表示位置の正とする。
            if img_inline {
                if let Some(rx) = &play_prefetch_rx {
                    let mut latest = None;
                    while let Ok(f) = rx.try_recv() { latest = Some(f); }
                    if let Some(f) = latest { play_prefetch_held = Some(f); }
                }
            }
            let prefetched_d = if img_inline { play_prefetch_held.as_ref().map(|(d, _)| *d) } else { None };
            if let Some(rt) = spec.routes.last().map(|r| r.pts.clone()) {
                if rt.len() >= 2 {
                    let total = roadtrace::polyline_len(&rt);
                    let d = if let Some(fd) = prefetched_d {
                        fd
                    } else {
                        let now = std::time::Instant::now();
                        let dt = play_last_tick.map_or(0.0, |t| now.duration_since(t).as_secs_f64());
                        play.unwrap() + roadtrace::play_step_distance_m(cfg.route_play_speed_kmh, play_speed, dt)
                    };
                    play_last_tick = Some(std::time::Instant::now()); // 次回差分計算の基点(先読み経路でも維持)
                    if d >= total {
                        play = None; play_last_tick = None;
                        play_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        play_prefetch_rx = None; play_prefetch_held = None;
                        addr = "再生: 終了".into();
                    } else {
                        play = Some(d);
                        let (pla, plo) = roadtrace::point_at(&rt, d);
                        let (nx, ny) = deg_to_pixel(pla, plo, z); cx = nx; cy = ny;
                        maybe_speak_turn(&cfg, &spec, &turn_points, &mut voice_guide, (pla, plo));
                    }
                } else {
                    play = None; play_last_tick = None;
                    play_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    play_prefetch_rx = None; play_prefetch_held = None;
                }
            } else {
                play = None; play_last_tick = None;
                play_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                play_prefetch_rx = None; play_prefetch_held = None;
            }
        }
        let (lat, lon) = pixel_to_deg(cx, cy, z);

        // プロットデータ(道路交通量/主要道路/道路ライブカメラ/通行規制)の、いま表示範囲に
        // 掛かるぶんを1フレーム1回だけ切り出す(地図のオーバーレイ描画とステータス行で共用)。
        // キャッシュは視野より広いセル単位で持っているので、ここで視野へ絞る。
        let plot_view = plotlayer::view_bbox(cx, cy, z);
        let plot_now = plotcache::now_secs(); // 経過時間表示の基準(1フレーム内で揃える)
        let traffic_points = traffic_layer.items(plot_view);
        // 観測点のスナップ先は点列だけあればよいので、線形への参照だけを借りる(複製しない)。
        let major_roads: Vec<&[(f64, f64)]> =
            roads_layer.items(plot_view).into_iter().map(|r| r.pts.as_slice()).collect();
        let camera_points = camera_layer.items(plot_view);
        let regulation_events = regulation_layer.items(plot_view);
        // 規制原因アイコン(#規制原因アイコン): 表示中のClosedイベントから未分類の1件を選び、
        // 他にジョブが走っていなければバックグラウンドで規制原因を取得する
        // (同時に1件だけ=道路情報提供システムへの負荷を抑えるレート制限)。
        if cfg.regulation_enabled && cause_job.is_none() {
            let visible_closed: Vec<&regulation::ClosureEvent> = regulation_events.iter().copied()
                .filter(|e| e.kind == regulation::RegulationKind::Closed)
                .collect();
            if let Some(id) = next_closure_to_categorize(&visible_closed, &cause_cache) {
                let id = id.to_string();
                let (tx, rx) = std::sync::mpsc::channel();
                let id2 = id.clone();
                std::thread::spawn(move || { let _ = tx.send((id2, regulation::fetch_detail(&id))); });
                cause_job = Some(rx);
            }
        }
        let disaster_sites = disaster_layer.items(plot_view);
        let muni_areas = boundary_layer.items(plot_view);
        // 過去災害の見せ方をズームで切り替える(設計 §3.4)。塗りは画面に複数の市区町村が
        // 入るズーム帯でだけ意味を持つ。z14以上は画面幅が1km前後になり「全面が同じ色」に
        // 退化するので、そこは従来の代表点マーカーへ譲る。判定に使うのは実描画ズーム(rz)では
        // なく地図ズーム(z): 実画像モードは rz>z でも写る地理範囲は同じため。
        let fill_on = cfg.disaster_enabled && cfg.disaster_fill;
        let choropleth_fill = fill_on && (11..=13).contains(&z);
        let choropleth_outline = fill_on && (11..=14).contains(&z);
        // 件数リングは塗りが件数を担っている間は出さない(中心の小さな塊は常に残すので、
        // B キーが何を指しているかは画面から分かる)。
        let disaster_rings = !choropleth_fill;
        let population_meshes =
            if cfg.population_enabled { population_layer.items(plot_view) } else { Vec::new() };
        // 表示する年次(設定)。configを手書きで壊されても必ず描ける索引へ落とす。
        let population_year_idx = population::year_index(cfg.population_year)
            .unwrap_or_else(|| population::year_index(config::DEFAULT_POPULATION_YEAR).unwrap_or(1));
        // 中心のクロスヘアが指すメッシュの人口密度(ステータス行に実数値で出す・設計 §7.6)。
        // 階級の幅(1,000〜4,000等)しか読めない色分けを、1つの数字で補う。
        // 中心は必ず視野の中にあるので、切り出し済みの population_meshes から拾う
        // (layer.items() をもう一度呼ぶと全セル(最大16万件)をもう1周することになる)。
        let population_here = mesh::half_mesh_code(lat, lon).and_then(|code| {
            population_meshes
                .iter()
                .find(|m| m.mesh == code)
                .and_then(|m| population::density(m, population_year_idx))
        });

        // 通行止めルート回避(#通行止めを推奨しない)。表示中の視野(plot_view)ではなく、
        // 経由地全体を覆うbboxで実施中の通行止めを見て、BRouterのnogosへ変換する。
        // 通行規制の設定(cfg.regulation_enabled)と連動させる: OFFなら外部へ問い合わせず
        // regulation_layerにデータ自体が無いため、ここでも自然に空になる。
        let (route_nogos, route_nogos_truncated) = if cfg.regulation_enabled {
            match route::waypoints_bbox_with_margin(&wps, 0.05) {
                Some(bbox) => {
                    let closures = regulation_layer.items(bbox);
                    let center = ((bbox.0 + bbox.2) / 2.0, (bbox.1 + bbox.3) / 2.0);
                    let (circles, truncated) = route::closures_to_nogos(&closures, center);
                    (route::nogos_query_param(&circles), truncated)
                }
                None => (String::new(), false),
            }
        } else {
            (String::new(), false)
        };

        // 移動検知(解像度非依存): 直近に描画したフレームと(cx,cy,z)が違えば「動いた」。
        // 動いた直後〜350ms は低解像度(delta=0)で速く描き、動きが止まって350ms経ったら
        // 設定解像度(高/中/低)へ上げる。GPS追従(gps_rx)のように断続的に動くケースは
        // 自然に低解像度のまま張り付く(=負荷とメモリを抑える)。
        // ただしルート再生(play)中は毎フレーム動き続けるため、この判定に従うと恒久的に
        // 低解像度画像が高頻度で切り替わり続けてちらついて見える。プレビューは見た目重視の
        // 機能なので、再生中は「動いている」扱いにせず常に設定解像度を使う。
        if prev_render_cxyz != Some((cx, cy, z)) { moved_at = Some(std::time::Instant::now()); }
        prev_render_cxyz = Some((cx, cy, z));
        let settling = img_inline && cfg.image_settle_low_res && play.is_none() && moved_at.map_or(false, |t| t.elapsed() < std::time::Duration::from_millis(350));

        // 実画像モードの描画寸法とズーム。AAと同じ地理範囲を、深いズーム段(タイルの上限z18まで)
        // で取得して高精細化する。scale=2^Δ で、地図領域のセル数×(横scale/縦2*scale px)の実ピクセル
        // 解像度になる。設定(image_res)で上限を選べる: high=+2(横4/縦8px per cell) / mid=+1 / low=+0。
        // rz>z のときグローバル画素座標は 2^Δ 倍になるので中心 cx/cy も scale 倍する。
        let base_delta: u32 = match cfg.image_res.as_str() { "high" => 2, "low" => 0, _ => 1 };
        // 移動中に落とす先は config::IMAGE_SETTLE_DELTA_CAP(設計 §5.3 C-3 の見直し。
        // 判断の根拠は定数側のコメントに書いてある)。
        let delta = if !img_inline { 0 }
            else if settling { base_delta.min(config::IMAGE_SETTLE_DELTA_CAP).min(18u32.saturating_sub(z)) }
            else { base_delta.min(18u32.saturating_sub(z)) };
        let scale = 1u32 << delta;
        let (rw, rh, rz, rcx, rcy) = if img_inline {
            (map_cols * scale, map_rows * 2 * scale, z + delta, cx * scale as f64, cy * scale as f64)
        } else {
            (ow, oh, z, cx, cy)
        };
        // サブピクセル描画(設計 §5.1 対策A)。窓の切り出しで left/top の小数部を捨てないので、
        // 1出力ピクセル未満の動きも色の遷移として見え、斜めドラッグの階段が消える。
        // braille/edge は閾値でドットの on/off が決まるためちらつく可能性があり、
        // use_subpixel_window() で切り替えられるようにしてある。
        let subpixel = use_subpixel_window(opts.braille, opts.edge, subpixel_env.as_deref());
        let sub_steps = if subpixel { SUBPIXEL_STEPS } else { 1.0 };
        // 描画へ渡す中心は格子へ吸着させる。再描画判定(map_sig)と描画で同じ値を使わないと、
        // 絵が変わったのにシグネチャが変わらない取りこぼしが起きる(設計 §5.2)。
        // 論理座標 cx/cy は連続のまま保持してあるので、指の移動量は 1:1 のまま失われない。
        let (rcx, rcy) = snap_center_to_grid(rcx, rcy, sub_steps);
        // ローダーへ今の表示位置(実描画のズーム/中心)を毎フレーム渡す。need_buildがfalseで再構築を
        // 省くフレームでも、裏取得の近傍優先が最新の現在地を使えるよう常に更新しておく。
        loader.set_view(rcx, rcy, rz, &opts.style);

        // 再生開始直後、実画像モードならrw/rh/rz確定を待って先読みスレッドを起こす(1フレーム遅延)。
        // build_window(重い/ネットワーク)を裏で進めておき、メインは受け取った画像を使うだけにして
        // ちらつきを抑える。ASCII描画時はネットワーク待ちが無く不要なので起こさない。
        if play_wants_prefetch {
            play_wants_prefetch = false;
            if img_inline {
                if let Some(r) = spec.routes.last().filter(|r| r.pts.len() >= 2) {
                    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let speed_bits = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(play_speed.to_bits()));
                    let (tx, rx) = std::sync::mpsc::sync_channel(6);
                    let route_pts = r.pts.clone();
                    let style = opts.style.clone();
                    let (pw, ph, pz) = (rw, rh, rz);
                    let speed_kmh = cfg.route_play_speed_kmh;
                    // 再開位置。再生開始直後はplay=Some(0.0)なので先頭からになるが、ズーム変更に
                    // よる先読み再起動(restart_prefetch_on_zoom!)時はplayが現在の走行距離を持って
                    // いるので、そこから続ける(先頭に戻さない)。
                    let start_d = play.unwrap_or(0.0);
                    let cancel2 = std::sync::Arc::clone(&cancel);
                    let speed_bits2 = std::sync::Arc::clone(&speed_bits);
                    std::thread::spawn(move || {
                        let mut local_cache = Cache::new();
                        let total = roadtrace::polyline_len(&route_pts);
                        let mut d = start_d;
                        let dt = 0.08; // 80ms刻みで先読み(メインの再描画間隔に合わせる)
                        while d < total {
                            if cancel2.load(std::sync::atomic::Ordering::Relaxed) { break; }
                            let (la, lo) = roadtrace::point_at(&route_pts, d);
                            let (gx, gy) = deg_to_pixel(la, lo, pz);
                            if let Ok(img) = build_window(gx, gy, pz, pw, ph, &style, &mut local_cache) {
                                if tx.send((d, img)).is_err() { break; } // 受信側が止まったら終了
                            }
                            let speed = f64::from_bits(speed_bits2.load(std::sync::atomic::Ordering::Relaxed));
                            d += roadtrace::play_step_distance_m(speed_kmh, speed, dt);
                        }
                    });
                    play_cancel = cancel;
                    play_speed_bits = speed_bits;
                    play_prefetch_rx = Some(rx);
                    play_prefetch_held = None;
                }
            }
        }

        // 再描画判定シグネチャ。地図に効く状態(中心/ズーム/寸法/配置/オーバーレイ)が前回emit時と
        // 同じなら描き直さない。以前は実画像モード限定だったが、AAモードも同じ判定に乗せることで
        // タイル非同期ロード中(#35)にloader.generation()だけが変わり続けて毎ポーリング(80ms毎)
        // 無条件に全画面書き込みが発生する問題を解消する(macOS標準Terminal.appでの描画崩れの原因)。
        // このフレームの再描画判定に使うgeneration値をここで固定する。ローダーのワーカーは
        // 「cacheへinsert→generation加算→pending.inflightから除去」の順で動くため、この直後に
        // is_busy()を見る時点までの間に最後の1枚がちょうど着地すると、is_busy()はfalse(もう待たない)
        // だが今フレームの再構築には間に合っていない、という取りこぼしが起き得る(#53)。
        // その場合pollingがfalseになりevent::read()でブロックしてしまい、実際は届いているのに
        // 次のキー入力までLOADING表示が残り続ける。スナップショットして後段で比較し、その間に
        // 進んでいたら強制的にポーリング継続させることでこの1フレーム分の取りこぼしを防ぐ。
        let loader_gen_snapshot = loader.generation();
        let map_sig: Option<u64> = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            // 中心座標は生の f64 ではなく、実際に描画へ効く粒度へ丸めて混ぜる(設計 §5.2 対策B)。
            // これより細かい差はどうせ同じ絵になる。粒度は描画側と揃える必要があるので、
            // rcx/rcy を吸着させたときと同じ sub_steps を渡す(サブピクセル切り出しなら
            // 1/SUBPIXEL_STEPS ピクセル・従来の整数切り出しなら1ピクセル)。
            // rcx/rcy は既に同じ格子へ吸着済みなので、描画とシグネチャの位置は必ず一致する。
            map_center_sig_key(rcx, rcy, rw, rh, sub_steps).hash(&mut h);
            // 切り出し方が変わると同じ中心でも絵が変わるので、モードそのものも混ぜる。
            subpixel.hash(&mut h);
            rz.hash(&mut h); rw.hash(&mut h); rh.hash(&mut h);
            gut.hash(&mut h); map_cols.hash(&mut h); map_rows.hash(&mut h);
            opts.style.hash(&mut h);
            // 裏取得でタイルが1枚届くたびに世代が変わる→sigが変わり次フレームで再構築され、
            // グレーのプレースホルダーが実タイルへ順次置き換わる。
            loader_gen_snapshot.hash(&mut h);
            spec.routes.len().hash(&mut h);
            spec.expressway_segments.len().hash(&mut h);
            spec.roads.len().hash(&mut h);
            spec.traffic_segments.len().hash(&mut h);
            spec.warning_segments.len().hash(&mut h);
            for rt in spec.routes.iter().chain(spec.expressway_segments.iter()).chain(spec.roads.iter()).chain(spec.traffic_segments.iter()).chain(spec.warning_segments.iter()) {
                rt.color.hash(&mut h); rt.thickness.hash(&mut h);
                for &(a2, b2) in &rt.pts { a2.to_bits().hash(&mut h); b2.to_bits().hash(&mut h); }
            }
            spec.pois.len().hash(&mut h);
            for p in &spec.pois { p.lat.to_bits().hash(&mut h); p.lon.to_bits().hash(&mut h); (p.cat as u8).hash(&mut h); }
            spec.rings.len().hash(&mut h);
            for r in &spec.rings {
                r.lat.to_bits().hash(&mut h); r.lon.to_bits().hash(&mut h);
                r.color.hash(&mut h); r.thickness.hash(&mut h);
                for k in &r.radii_km { k.to_bits().hash(&mut h); }
            }
            spec.spots.len().hash(&mut h);
            for &(a2, b2, c2, s2) in &spec.spots { a2.to_bits().hash(&mut h); b2.to_bits().hash(&mut h); c2.hash(&mut h); s2.hash(&mut h); }
            match gps_pos { Some((a2, b2)) => { 1u8.hash(&mut h); a2.to_bits().hash(&mut h); b2.to_bits().hash(&mut h); } None => 0u8.hash(&mut h) }
            gps_trail.len().hash(&mut h);
            for &(a2, b2) in &gps_trail { a2.to_bits().hash(&mut h); b2.to_bits().hash(&mut h); }
            wps.len().hash(&mut h);
            for &(a2, b2) in &wps { a2.to_bits().hash(&mut h); b2.to_bits().hash(&mut h); }
            wp_sel.hash(&mut h);
            // 雨雲レーダー: ON/OFF と表示中コマ(basetime+validtime)が変われば描き直す。
            // < > でコマを送ったとき、また targetTimes 更新で表示時刻が変わったときに効く。
            radar_on.hash(&mut h);
            if radar_on {
                if let Some(f) = radar_tl.get(radar_idx) { f.basetime.hash(&mut h); f.validtime.hash(&mut h); }
            }
            radar_opacity_value(&cfg).to_bits().hash(&mut h); // 濃さ(設定)を変えたら描き直す
            // プロットデータ: セル表が変わる(新しいセルが届く/期限切れで入れ替わる)たびに
            // 世代が進む。これを混ぜておかないと、位置もズームも動いていないフレームでは
            // sigが変わらず、届いたばかりの交通量/規制がオーバーレイに反映されない。
            traffic_layer.generation().hash(&mut h);
            roads_layer.generation().hash(&mut h);
            camera_layer.generation().hash(&mut h);
            regulation_layer.generation().hash(&mut h);
            disaster_layer.generation().hash(&mut h);
            // 過去災害の塗り: 境界セルが届くたび、また塗り/縁取りの出し分けが変わるたびに
            // ラスタライズし直す(逆に、パンもズームもしていないフレームでは1回も走らない)。
            boundary_layer.generation().hash(&mut h);
            choropleth_fill.hash(&mut h);
            choropleth_outline.hash(&mut h);
            population_layer.generation().hash(&mut h);
            // 人口メッシュ: ON/OFF・年次・濃さを変えたら描き直す(セル表は動かないため generation
            // だけでは変化を拾えない)。
            cfg.population_enabled.hash(&mut h);
            if cfg.population_enabled {
                population_year_idx.hash(&mut h);
                population_opacity_value(&cfg).to_bits().hash(&mut h);
            }
            Some(h.finish())
        };

        let mut map_img: Option<RgbImage> = None; // 実画像モードで描く overlay 合成済み画像
        // 状態が前回emitと同一なら、地図の再構築/再emit・AA再描画をスキップ(直近の描画を残す)。
        // 空文字を書いても既存セルは上書きされない(何も描かれない)ため、スキップ時は前フレームの
        // 内容がそのまま画面に残る(iTerm2の画像もAAの文字も同じ理屈で安全にスキップできる)。
        let need_build = force_reemit || last_map_sig != map_sig;
        // 先読みの受信(最新への間引き含む)はplayブロック側で既に行っている(表示位置と
        // ベース画像の位置を一致させるため)。ここではplay_prefetch_heldを読むだけ。
        let body = if !need_build {
            String::new()
        } else {
            let prefetched = if play.is_some() && img_inline { play_prefetch_held.as_ref().map(|(_, img)| img.clone()) } else { None };
            let built = match prefetched {
                Some(img) => Ok(img),
                // 非ブロッキング版: 未取得タイルはグレーで即返し、取得はローダーが裏で進める。
                None => build_window_nowait(rcx, rcy, rz, rw, rh, &opts.style, subpixel, &loader),
            };
            match built {
                Ok(mut img) => {
                    // 雨雲レーダーの降水レイヤ。未取得タイルは透明のまま返る(グレー箱もLOADING
                    // 透かしも出さない)。視野が日本国外/広域すぎる場合は None = 何も重ねない。
                    let radar_layer: Option<RgbaImage> = if radar_on {
                        radar_tl.get(radar_idx)
                            .and_then(|f| build_radar_window_nowait(rcx, rcy, rz, rw, rh, f, &loader))
                    } else { None };
                    // 過去災害のコロプレス層(市区町村を記録の多さで塗り分けたRGBA)。
                    // 雨雲と同じ合成経路に「雨雲より後ろ」として乗せる(雨は今の話・災害履歴は
                    // 土地の話なので、今の情報が上に来るのが正しい)。
                    let choro_shading = choropleth::Shading {
                        // 濃さは層の中身に影響しない(合成時に掛ける)ので、ここで控えておいて
                        // 3経路それぞれへ同じ値を渡す。設定行にするのは Stage2。
                        opacity: choropleth::DEFAULT_OPACITY,
                        fill: choropleth_fill,
                        outline: choropleth_outline,
                    };
                    let choro_opacity = choro_shading.opacity;
                    let choro_layer: Option<RgbaImage> = if choropleth_fill || choropleth_outline {
                        choropleth::build_layer(&disaster_sites, &muni_areas, rcx, rcy, rz, rw, rh, choro_shading)
                    } else { None };
                    // 500mメッシュ人口。人口なし=透明の面レイヤを作る(設計 §7.1)。
                    // 雨雲より背面に置く(人口は数年変わらない下地・雨雲は現況)。
                    let pop_layer: Option<RgbaImage> = if cfg.population_enabled && !population_meshes.is_empty() {
                        Some(build_population_layer(&population_meshes, population_year_idx,
                                                    population::Metric::Density, rcx, rcy, rz, rw, rh))
                    } else { None };
                    // 実画像モードはここで地図へ直接アルファ合成する(オーバーレイはこの後に焼くので
                    // 経路/POI/中心十字は常に雨雲・人口より前面に残る)。
                    // 下から コロプレス → 人口 → 雨雲 の順で重ねる(土地の話が下・今の話が上)。
                    if img_inline {
                        if let Some(l) = &choro_layer { blend_rgba_over(&mut img, l, choro_opacity); }
                        if let Some(l) = &pop_layer { blend_rgba_over(&mut img, l, population_opacity_value(&cfg)); }
                        if let Some(l) = &radar_layer { blend_rgba_over(&mut img, l, radar_opacity_value(&cfg)); }
                    }
                    // braille/edge は OverlayLayer へインクとして焼く(build_overlay の先頭で最背面に入る)。
                    // 配列の順序がそのまま重ね順(先頭が最背面)なので、上と同じ順で積む。
                    // コロプレスの面塗りだけは雨雲・人口のディザではなく疎な点描で間引く
                    // (Bayerだと braille のセルが全部塗り色に化けるため)。
                    let mut inks: Vec<InkLayer> = Vec::new();
                    if radar_ink {
                        if let Some(l) = &choro_layer {
                            inks.push(InkLayer::Stipple { layer: l, spacing: choropleth::STIPPLE_SPACING });
                        }
                        if let Some(l) = &pop_layer {
                            inks.push(InkLayer::Dither { layer: l, density: population_opacity_value(&cfg) });
                        }
                        if let Some(l) = &radar_layer {
                            inks.push(InkLayer::Dither { layer: l, density: radar_opacity_value(&cfg) });
                        }
                    }
                    let mut ov = build_overlay(&spec, rcx, rcy, rz, rw, rh, 1.0, 1.0, rw, rh, &inks);
                    let (mx, my) = (rw as i32 / 2, rh as i32 / 2); // 中心クロスヘア(色は設定で選択可)
                    let cross = SPOT_PALETTE[cfg.cross_color_idx as usize % SPOT_PALETTE.len()];
                    draw_line(&mut ov, mx - 6, my, mx + 6, my, cross, 1);
                    draw_line(&mut ov, mx, my - 6, mx, my + 6, cross, 1);
                    if gps_pos.is_some() { // ライブ現在地: トレイル(薄青)+自位置(赤)
                        for (tla, tlo) in &gps_trail {
                            let (gx, gy) = deg_to_pixel(*tla, *tlo, rz);
                            let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                            let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                            draw_ring(&mut ov, ix, iy, 1, [80, 160, 255], 1);
                        }
                        if let Some((gla, glo)) = gps_pos {
                            let (gx, gy) = deg_to_pixel(gla, glo, rz);
                            let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                            let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                            draw_ring(&mut ov, ix, iy, 4, [255, 60, 60], 2);
                        }
                    }
                    if !wps.is_empty() { // 選択中(Tab)の waypoint を白丸で強調
                        let s = wp_sel.min(wps.len() - 1);
                        let (gx, gy) = deg_to_pixel(wps[s].0, wps[s].1, rz);
                        let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                        let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                        draw_ring(&mut ov, ix, iy, 3, [255, 255, 255], 1);
                    }
                    if cfg.traffic_enabled { // 道路交通量(混雑度の目安。事故情報・渋滞度そのものではない)
                        // 観測点を最寄りの主要道路(major_roads)へスナップし、前後
                        // TRAFFIC_SNAP_RADIUS個ぶんの区間をラインとして色分け表示する。
                        // OSMのway分割は実測でかなり細かい(2026-08-16 東京都内サンプル:
                        // ノード間隔中央値約20m・90%タイル74m、1way平均7ノード≒100〜150m)。
                        // wayを跨いで延長する処理は入れていないため、線の長さはway次第で
                        // ばらつく(短いwayなら前後にはみ出さずクランプされる)。
                        // major_roadsが空(未取得中・取得失敗直後)の間は、従来通り観測点を
                        // 丸で表示するフォールバックにする。
                        const TRAFFIC_SNAP_RADIUS: usize = 15;
                        // 周囲に主要道路データが無い観測点を無関係な遠い道へ誤ってスナップしない
                        // ための上限。500m以内に主要道路の頂点が無ければ点表示のフォールバックへ回る。
                        const TRAFFIC_SNAP_MAX_DIST_M: f64 = 500.0;
                        for p in &traffic_points {
                            let color = match traffic::classify(p.volume) {
                                traffic::CongestionLevel::Light => [80, 200, 90],
                                traffic::CongestionLevel::Moderate => [230, 200, 40],
                                traffic::CongestionLevel::Heavy => [220, 50, 40],
                            };
                            let seg = roadtrace::nearest_way_segment_within(&major_roads, (p.lat, p.lon), TRAFFIC_SNAP_RADIUS, TRAFFIC_SNAP_MAX_DIST_M);
                            if seg.len() >= 2 {
                                let pts: Vec<(i32, i32)> = seg.iter().map(|&(la, lo)| {
                                    let (gx, gy) = deg_to_pixel(la, lo, rz);
                                    ((gx - (rcx - rw as f64 / 2.0)).floor() as i32, (gy - (rcy - rh as f64 / 2.0)).floor() as i32)
                                }).collect();
                                for w in pts.windows(2) { draw_line(&mut ov, w[0].0, w[0].1, w[1].0, w[1].1, color, 3); }
                            } else {
                                let (gx, gy) = deg_to_pixel(p.lat, p.lon, rz);
                                let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                                let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                                draw_ring(&mut ov, ix, iy, 3, color, 3);
                            }
                        }
                    }
                    if cfg.camera_enabled { // 道路ライブカメラ(紫系。Nで中心近くのカメラの写真を表示)
                        for c in &camera_points {
                            let (gx, gy) = deg_to_pixel(c.lat, c.lon, rz);
                            let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                            let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                            draw_ring(&mut ov, ix, iy, 3, [170, 90, 220], 2);
                        }
                    }
                    if cfg.regulation_enabled { // 通行規制(通行止め/車線規制等の区間を種別ごとの色で線描画)
                        for ev in &regulation_events {
                            let pts: Vec<(i32, i32)> = ev.line.iter().map(|&(la, lo)| {
                                let (gx, gy) = deg_to_pixel(la, lo, rz);
                                ((gx - (rcx - rw as f64 / 2.0)).floor() as i32, (gy - (rcy - rh as f64 / 2.0)).floor() as i32)
                            }).collect();
                            for w in pts.windows(2) { draw_line(&mut ov, w[0].0, w[0].1, w[1].0, w[1].1, ev.kind.color(), 3); }
                            // 規制原因アイコン(#規制原因アイコン): 事故✕/工事のみ、区間の中点に重ね描き。
                            if let Some(category) = cause_cache.get(&ev.detail_id) {
                                if let Some((color, shape)) = regulation::cause_icon(*category) {
                                    if let Some((la, lo)) = closure_icon_position(&ev.line) {
                                        let (gx, gy) = deg_to_pixel(la, lo, rz);
                                        let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                                        let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                                        draw_marker(&mut ov, ix, iy, color, 4, shape);
                                    }
                                }
                            }
                        }
                    }
                    if cfg.disaster_enabled { // 過去災害(Bでその地点の事例一覧)
                        // 座標が市区町村の代表点で1点に何十件も重なるため、事例1件=1マーカーには
                        // しない。1座標=1マーカーにして、件数を外周リングの半径、最も多い種別を
                        // 色で表す。外周を細くするのは地図と他レイヤを覆い隠さないため
                        // (中心の塊があるので細くても位置は読める)。
                        // 塗り(コロプレス)が出ているズーム帯では件数の役目が塗りへ移るので、
                        // 外周リングは出さず中心の小さな塊だけを残す(Bキーの対象を示すため)。
                        for s in &disaster_sites {
                            let (gx, gy) = deg_to_pixel(s.lat, s.lon, rz);
                            let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                            let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                            let color = s.dominant().color();
                            draw_ring(&mut ov, ix, iy, 1, color, 2);
                            if disaster_rings {
                                draw_ring(&mut ov, ix, iy, disaster::marker_radius(s.total()), color, 1);
                            }
                        }
                    }
                    last_map_sig = map_sig; // このsigで描いた内容がこのフレームでemitされる
                    if img_inline {
                        // 実画像モード: 取得画像に overlay を焼き込んで保持し、AA文字列は空にする。
                        let mut c = img;
                        composite(&mut c, &ov);
                        map_img = Some(c);
                        String::new()
                    } else {
                        // インク経路(braille/edge)は ov に入れ済みなので render 側では合成しない。
                        // halfblock/classify はここで渡し、classify は recolor 後に混ざる。
                        // 配列の順序がそのまま重ね順。コロプレス → 人口 → 雨雲 の順で地図へ
                        // アルファ合成する(braille/edge は上で ov のインクに入れ済みなので渡さない)。
                        let mut blends: Vec<(&RgbaImage, f64)> = Vec::new();
                        if !radar_ink {
                            if let Some(l) = &choro_layer { blends.push((l, choro_opacity)); }
                            if let Some(l) = &pop_layer { blends.push((l, population_opacity_value(&cfg))); }
                            if let Some(l) = &radar_layer { blends.push((l, radar_opacity_value(&cfg))); }
                        }
                        render(&img, &opts, Some(&ov), &blends)
                    }
                }
                Err(e) => {
                    last_map_sig = None; // 失敗時は次フレームで再取得
                    format!("取得失敗: {e}\r\n")
                }
            }
        };
        force_reemit = false; // 強制再emitは消費済み(image_inlineの被り解消は下でmap_coveredが再設定)

        // 左袖リスト(POI か お気に入り)の各行を組む。組み立ては ui_gutter.rs へ切り出し済み。
        let glines: Vec<String> = ui_gutter::build_gutter_lines(&ui_gutter::GutterCtx {
            gut, map_rows, focus: &focus,
            show_menu, show_route, show_wps, show_splist, show_catlist, show_settings,
            show_poimenu, show_routes, show_favmenu, show_roadlist,
            menu_cat_sel, menu_item_sel,
            wps: &wps, route_sel, grab, wp_sel,
            spots: &spots, cur_cat: &cur_cat, sp_sel, lat, lon,
            spot_cats: &spot_cats, cat_sel,
            opts: &opts, cfg: &cfg, set_sel, set_pick_sel,
            poi_kinds: &poi_kinds, poimenu_sel,
            route_names: &route_names, rn_sel,
            road_segs: &road_segs, road_sel,
            pois: &pois, poi_label: &poi_label, poi_sel,
        }, &mut list_offset);

        // 左袖 + 地図 を絶対座標で配置
        let _ = write!(out, "\x1b[H");
        let lines: Vec<&str> = body.split("\r\n").collect();
        let blank = fit_cells("", gut as usize);
        for i in 0..map_rows as usize {
            let ln = lines.get(i).copied().unwrap_or("");
            if gut > 0 {
                let g = glines.get(i).cloned().unwrap_or_else(|| blank.clone());
                write!(out, "\x1b[{};1H{}\x1b[{};{}H{}", i + 1, g, i + 1, gut + 1, ln)?;
            } else {
                write!(out, "\x1b[{};1H{}", i + 1, ln)?;
            }
        }
        if let Some(mi) = &map_img { // 実画像モード: 地図領域の左上セルへ移動してインライン画像を出力
            let _ = write!(out, "\x1b[1;{}H", gut + 1);
            let _ = emit_iterm2_image(&mut out, mi, map_cols, map_rows);
            // インライン画像はscrollbackに積もりiTermのメモリを肥大させる(Cmd+Kのclear buffer相当)。
            // 可視画面は変えず、一定枚数emitごとにscrollbackだけ捨てて自動で溜め込みを防ぐ。
            emit_count += 1;
            if emit_count % 40 == 0 { let _ = write!(out, "\x1b[3J"); }
        }
        if elev_h > 0 { // 標高プロファイル帯(地図の下・ステータスの上)。描画は ui_overlay.rs へ切り出し済み
            ui_overlay::draw_elevation_band(&mut out, cols, map_rows, elev_h, &route_ele, route_ascend, &spec, lat, lon);
        }
        // ステータス行の文面組み立ては ui_status.rs へ切り出し済み。通信中スピナーの判定に使う
        // 各ジョブは有無しか見ないのでここで1つのフラグに畳んでから渡す。
        let jobs_active = route_job.is_some() || search_job.is_some() || near_job.is_some() || street_job.is_some() || cam_job.is_some() || recommend_job.is_some() || road_job.is_some() || catpoi_job.is_some() || wander_job.is_some() || disaster_job.is_some() || regulation_detail_job.is_some() || traffic_color_job.is_some() || cause_job.is_some();
        // 次の曲がり角の画面表示。音声案内(maybe_speak_turn)と同じくturn_points+現在地から
        // 求めるが、読み上げ済みかの状態は見ない(何度描画しても同じ内容を出したいため)。
        let next_turn = spec.routes.last()
            .and_then(|rt| route::progress_along_route((lat, lon), &rt.pts))
            .and_then(|progress_m| voice::next_turn_display(&turn_points, progress_m))
            .map(|(remaining, phrase)| format!("↳{}m {phrase} ", voice::round_to_50(remaining)));
        let status = ui_status::build_status_line(ui_status::StatusCtx {
            focus: &focus, save_confirm: &save_confirm, spot_move_confirm, spots: &spots,
            cur_cat: &cur_cat, pending_spot: pending_spot.is_some(), set_sel, poi_label: &poi_label,
            route_note: &route_note, clear_route_confirm, jobs_active, spin,
            gps_live: gps_rx.is_some(), web_gps_active, play, play_speed,
            radar_on, radar_tl: &radar_tl, radar_idx, radar_follow,
            loader: &loader, rcx, rcy, rz, rw, rh,
            cfg: &cfg,
            // 主要道路は交通量のスナップ下地で、それ自体はステータスに出さない(描画も未実装)。
            traffic: ui_status::PlotStatus {
                area: None,
                count: traffic_points.len(),
                job_active: traffic_layer.job_active() || roads_layer.job_active(),
                stale_age_secs: traffic_layer.stale_age_secs(plot_now),
                wide_area: traffic_layer.suppressed(),
            },
            camera: ui_status::PlotStatus {
                area: None,
                count: camera_points.len(),
                job_active: camera_layer.job_active(),
                stale_age_secs: camera_layer.stale_age_secs(plot_now),
                wide_area: camera_layer.suppressed(),
            },
            regulation: ui_status::PlotStatus {
                area: None,
                count: regulation_events.len(),
                job_active: regulation_layer.job_active(),
                stale_age_secs: regulation_layer.stale_age_secs(plot_now),
                wide_area: regulation_layer.suppressed(),
            },
            // 過去災害は事例数でなく地点数を出す(1地点に最大166件が重なるため)。
            // 事例一覧(Bキー)の取得中もスピナーではなくこのレイヤの表示で分かるようにする。
            population: ui_status::PopulationStatus {
                job_active: population_layer.job_active(),
                // 取得中のセルキーは都道府県コード2桁。名前に直して「北海道を取得中…」と出す。
                fetching: population_layer
                    .fetching_key()
                    .and_then(|k| k.parse::<u8>().ok())
                    .map(|p| population::pref_name(p).to_string())
                    .filter(|n| !n.is_empty()),
                wide_area: population_layer.suppressed(),
                density: population_here,
            },
            disaster: ui_status::PlotStatus {
                count: disaster_sites.len(),
                job_active: disaster_layer.job_active() || disaster_job.is_some() || boundary_layer.job_active(),
                stale_age_secs: disaster_layer.stale_age_secs(plot_now),
                wide_area: disaster_layer.suppressed(),
                // 塗りが出ているときだけ「いまいる市区町村と、その町の記録件数」を出す
                // (凡例を置く幅が無いので、代わりに読み手が知りたいことへ直接答える)。
                area: choropleth_fill
                    .then(|| choropleth::area_summary(&disaster_sites, &muni_areas, lat, lon))
                    .flatten(),
            },
            weather_warning_count: route_warnings.len(),
            weather_warning_top_name: route_warnings.first().map(|w| w.name.as_str()),
            weather_warning_job_active: route_warning_job.is_some(),
            addr: &addr, wps: &wps, z, lat, lon, next_turn: &next_turn,
        });
        let status = fit_cells_scroll(&status, cols as usize, spin);
        write!(out, "\x1b[{};1H\x1b[7m{status}\x1b[0m", tr)?;

        // 中央に重ねるパネル/ポップアップ類の描画は ui_overlay.rs へ切り出し済み。
        if quit_confirm { ui_overlay::draw_quit_confirm(&mut out, cols, map_rows); }
        if let Some(msg) = &popup { ui_overlay::draw_popup(&mut out, cols, map_rows, msg); }
        if let Some((title, lines)) = &disaster_view {
            ui_overlay::draw_disaster_panel(&mut out, cols, map_rows, title, lines, disaster::truncation_seen());
        }
        if let Some((title, lines)) = &regulation_detail_view {
            ui_overlay::draw_regulation_detail_panel(&mut out, cols, map_rows, title, lines);
        }
        if let Some(QrView::Text(q)) = &qr_view { ui_overlay::draw_qr_text(&mut out, cols, map_rows, tr, q); }
        if let Some(QrView::Image(img)) = &qr_view { ui_overlay::draw_qr_image(&mut out, cols, map_rows, tr, img); }
        if let Focus::SpotForm { name, url, field } = &focus { ui_overlay::draw_spot_form(&mut out, cols, map_rows, name, url, *field, input_cur, &cur_cat); }
        if let Focus::PoiKindForm { label, tag, field } = &focus { ui_overlay::draw_poi_kind_form(&mut out, cols, map_rows, label, tag, *field, input_cur); }
        if let Focus::WanderForm { dist_km } = &focus { ui_overlay::draw_wander_form(&mut out, cols, map_rows, *dist_km); }
        ui_overlay::draw_text_input(&mut out, cols, map_rows, &focus, input_cur);
        if let Focus::ColorPick { .. } = &focus { ui_overlay::draw_color_pick(&mut out, cols, map_rows, color_sel); }
        if let Focus::ShapePick { .. } = &focus { ui_overlay::draw_shape_pick(&mut out, cols, map_rows, shape_sel); }
        if onboard { ui_overlay::draw_onboarding(&mut out, cols, map_rows); }
        // 地図矩形を覆う中央オーバーレイ/パネルが「閉じた」フレーム(エッジ)でだけ画像を再emitして
        // 残像を消す。覆われている間(検索文字入力中など)は毎打鍵で強制再emitしない(メモリ/負荷対策)。
        let map_covered = popup.is_some() || qr_view.is_some() || onboard || quit_confirm || disaster_view.is_some() || regulation_detail_view.is_some()
            || matches!(focus,
                Focus::SpotForm { .. } | Focus::Search(_) | Focus::SaveName(_) | Focus::NearSearch(_)
                | Focus::NewCat(_) | Focus::RoadSearch(_) | Focus::Recommend(_)
                | Focus::SpotRename(..) | Focus::SpotEditName(..) | Focus::ColorPick { .. } | Focus::ShapePick { .. } | Focus::SettingsEdit(..) | Focus::PoiKindForm { .. } | Focus::WanderForm { .. });
        if prev_map_covered && !map_covered { force_reemit = true; }
        prev_map_covered = map_covered;
        // web版(ブラウザ)へ現在のドラッグ軸モードを通知する(#87 設計書 §5.2)。Focus は
        // interactive() 内の30か所以上で書き換わり、非同期ジョブの完了で勝手に変わる箇所も
        // ある(例: 周辺検索の結果適用で Map → PoiList)。変更箇所ごとに通知を足すのではなく
        // フレーム末で前回値と比較する方式にして、呼び出しをこの1か所に閉じている。
        // 認識しない端末(通常のターミナル)では無視されるだけなので、web以外でも害は無い。
        let cur_drag_axes = dragmode::axes(&focus);
        if prev_drag_axes != Some(cur_drag_axes) || drag_mode_req_pending {
            dragmode::emit_web_drag_mode(cur_drag_axes);
            prev_drag_axes = Some(cur_drag_axes);
            drag_mode_req_pending = false;
        }
        out.flush()?;

        // バックグラウンドジョブの結果を毎フレーム取り込む(route/search/near/street/recommend)。
        // Ok=適用しjob=None / Empty=保持 / Disconnected=None。結果を適用したフレームはブロックせず即再描画する。
        use std::sync::mpsc::TryRecvError;
        let mut got_result = false;
        if route_job.is_some() {
            match route_job.as_ref().unwrap().try_recv() {
                Ok(Ok(r)) => {
                    spec.routes.clear();
                    spec.traffic_segments.clear(); // 古いルートの色分けを引き継がない
                    spec.expressway_segments.clear(); // 古いルートの高速区間も同様
                    spec.warning_segments.clear(); // 古いルートの気象警報も同様
                    route_warnings.clear(); route_warning_job = None; // 新ルート確定でturn_jobを待ち直すため一旦クリア
                    route_note = Some(route_summary(&mode, &r));
                    // 通行止め回避が件数上限で一部反映できなかった場合、黙って進めると
                    // 「回避できた」と誤解されるのでひとこと添える。
                    if route_nogos_truncated {
                        route_note = route_note.map(|n| format!("{n} (通行止めの一部は回避対象外)"));
                    }
                    // 渋滞状況の色分け(#渋滞情報): ルートが変わるたびに問い合わせ直す。
                    traffic_color_job = if cfg.route_traffic_enabled && !cfg.google_maps_api_key.trim().is_empty() && r.pts.len() >= 2 {
                        Some(route::trigger_traffic_coloring(&r.pts, &mode, &cfg.google_maps_api_key))
                    } else {
                        None
                    };
                    route_ele = r.ele;
                    route_ascend = r.ascend_m;
                    let tile_coords = geo::route_tile_coords(&r.pts, z);
                    loader.request_route_tiles(&opts.style, z, &tile_coords);
                    // ルートが変わった(=曲がり角も変わりうる)ので、音声案内の状態は一旦捨てる。
                    // 取得は ON にした人だけがBRouterへ追加問い合わせする(既定OFF)。
                    turn_points = Vec::new();
                    voice_guide = None;
                    if cfg.voice_guide_enabled {
                        turn_job = Some(trigger_turn_points(&wps, &mode, 0, &r.pts, &route_nogos));
                    }
                    // 高速区間(#高速区間)の点列は r.pts をムーブする前に作る。ルート結果と
                    // 同時に確定するので、渋滞の色分けのような非同期の受け取り口は要らない。
                    spec.expressway_segments = route::expressway_polylines(&r.pts, &r.hw_segments)
                        .into_iter()
                        .map(|pts| Route { pts, color: route::EXPRESSWAY_COLOR, thickness: 2 })
                        .collect();
                    spec.routes.push(Route { pts: r.pts, color: [0, 220, 255], thickness: 2 });
                    route_job = None; got_result = true;
                }
                Ok(Err(e)) => { route_note = Some(format!("({e})")); route_job = None; got_result = true; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { route_job = None; got_result = true; }
            }
        }
        if turn_job.is_some() {
            match turn_job.as_ref().unwrap().try_recv() {
                Ok(v) => {
                    turn_points = v; voice_guide = Some(voice::VoiceGuide::new(&turn_points)); turn_job = None;
                    // 気象警報(#79・ルートベース)。voice_guide作り直しと同じ「ルート確定時」フックで
                    // ルート沿いの気象台コードを列挙し、まとめて背景取得する。
                    if cfg.weather_warning_enabled {
                        if let Some(pts) = spec.routes.last().map(|rt| rt.pts.clone()) {
                            let office_codes = ui_helpers::route_warning_office_codes(&pts);
                            if !office_codes.is_empty() {
                                let (tx, rx) = std::sync::mpsc::channel();
                                std::thread::spawn(move || {
                                    let mut all = Vec::new();
                                    for code in office_codes {
                                        if let Ok(ws) = warning::fetch_warnings(&code) { all.extend(ws); }
                                    }
                                    let _ = tx.send(all);
                                });
                                route_warning_job = Some(rx);
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { turn_job = None; }
            }
        }
        if let Some(job) = &route_warning_job {
            match job.try_recv() {
                Ok(ws) => {
                    route_warnings = ws;
                    if let Some(pts) = spec.routes.last().map(|rt| rt.pts.clone()) {
                        spec.warning_segments = ui_helpers::build_warning_segments(&pts, &route_warnings);
                    }
                    route_warning_job = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { route_warning_job = None; }
            }
        }
        // プロットデータ4種の取得。各レイヤが「視野を覆うセルのうち、fresh なものが手元に
        // 無いぶん」だけを1本のジョブで取りに行き、ディスクの読み書きもそのジョブの中で行う
        // (詳細は plotlayer.rs)。ここは毎フレーム tick して、セル表が変わったら即座に描き直す。
        // OFFのレイヤも tick は呼ぶ(走っていたジョブを取りこぼさず畳むため)。
        // 主要道路(#73)は交通量の観測点をラインへスナップする下地なので交通量と同じ条件で回す。
        got_result |= traffic_layer.tick(cx, cy, z, cfg.traffic_enabled);
        got_result |= roads_layer.tick(cx, cy, z, cfg.traffic_enabled);
        got_result |= camera_layer.tick(cx, cy, z, cfg.camera_enabled);
        got_result |= regulation_layer.tick(cx, cy, z, cfg.regulation_enabled);
        got_result |= disaster_layer.tick(cx, cy, z, cfg.disaster_enabled);
        // 境界は塗りに使うときだけ取りに行く(塗りをOFFにしている人に通信させない)。
        got_result |= boundary_layer.tick(cx, cy, z, cfg.disaster_enabled && cfg.disaster_fill);
        got_result |= population_layer.tick(cx, cy, z, cfg.population_enabled);
        if let Some(job) = &disaster_job { // Bキーで頼んだ事例一覧(2段目)の到着
            match job.try_recv() {
                Ok(Ok(panel)) => { disaster_view = Some(panel); disaster_job = None; got_result = true; }
                Ok(Err(e)) => { snd.play("error"); addr = format!("災害事例: {e}"); disaster_job = None; got_result = true; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { disaster_job = None; }
            }
        }
        if let Some(job) = &regulation_detail_job { // Tキーで頼んだ規制詳細の到着
            match job.try_recv() {
                Ok(Ok(d)) => { regulation_detail_view = Some(regulation::detail_panel_content(&d)); regulation_detail_job = None; got_result = true; }
                Ok(Err(e)) => { snd.play("error"); addr = format!("通行規制: {e}"); regulation_detail_job = None; got_result = true; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { regulation_detail_job = None; }
            }
        }
        if let Some(job) = &traffic_color_job { // 渋滞状況の色分け(#渋滞情報)の到着
            match job.try_recv() {
                Ok(segs) => {
                    if !segs.is_empty() {
                        spec.traffic_segments = segs.into_iter().map(|(color, pts)| Route { pts, color, thickness: 2 }).collect();
                        route_note = route_note.map(|n| format!("{n} (渋滞あり: 黄/赤)"));
                    } // 空(失敗・APIキー無し等)なら単色ルート線のまま静かに諦める
                    traffic_color_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { traffic_color_job = None; }
            }
        }
        if let Some(job) = &cause_job { // 規制原因アイコン(#規制原因アイコン)の分類結果到着
            match job.try_recv() {
                Ok((id, result)) => {
                    // 失敗時もOther相当でキャッシュする(でないと同じ1件を毎フレーム
                    // 再試行し続け、cause_jobが常にSomeになってレート制限が効かなくなる)。
                    let category = result.map(|d| regulation::categorize_cause(&d.cause)).unwrap_or(regulation::CauseCategory::Other);
                    cause_cache.insert(id, category);
                    cause_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { cause_job = None; }
            }
        }
        if let Some(job) = &voice_preview_job { // 読み上げの声(#78)の試聴結果
            match job.try_recv() {
                Ok(Ok(())) => { voice_preview_job = None; got_result = true; }
                Ok(Err(e)) => { snd.play("error"); addr = e; voice_preview_job = None; got_result = true; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { voice_preview_job = None; }
            }
        }
        if let Some(job) = &cam_job {
            match job.try_recv() {
                Ok((c, Ok(img))) => { cam_view = Some((img, c)); cam_job = None; }
                Ok((_, Err(e))) => { addr = format!("カメラ画像取得失敗: {e}"); cam_job = None; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { cam_job = None; }
            }
        }
        if search_job.is_some() {
            match search_job.as_ref().unwrap().try_recv() {
                Ok((ckey, q, res)) => {
                    match res {
                        Err(e) => { snd.play("error"); addr = format!("検索できません（{e}）"); }
                        Ok(v) if v.is_empty() => { snd.play("error"); addr = format!("見つからない: {q}"); }
                        Ok(v) => {
                            let now = searchcache::now_secs();
                            scache.insert(ckey, searchcache::CacheEntry { results: v.clone(), created_at: now, last_used_at: now });
                            let _ = searchcache::save(&scache);
                            pois = v.into_iter().take(8).map(|(la, lo, nm)| (la, lo, nm, PoiCat::Waypoint)).collect();
                            poi_sel = 0;
                            poi_label = format!("検索:{q}");
                            set_markers(&mut spec, &wps, &pois);
                            if matches!(focus, Focus::Map) { focus = Focus::PoiList; } // 別画面へ移っていたら奪わない
                        }
                    }
                    search_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { search_job = None; got_result = true; }
            }
        }
        if near_job.is_some() {
            match near_job.as_ref().unwrap().try_recv() {
                Ok((q, res)) => {
                    // ローカルの★スポット一致(距離順)を先頭、Overpass結果(距離順)を後ろにマージ。
                    // Overpassが障害の場合でも★一致だけは出す(0件=該当なしと障害を混同しない)。
                    let ql = q.to_lowercase();
                    let mut mine: Vec<(f64, f64, String, PoiCat)> = spots.iter()
                        .filter(|s| s.name.to_lowercase().contains(&ql))
                        .map(|s| (s.lat, s.lon, format!("★{}", s.name), PoiCat::Home)).collect();
                    mine.sort_by(|p, r| haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&haversine_km((lat, lon), (r.0, r.1))).unwrap_or(std::cmp::Ordering::Equal));
                    match res {
                        Ok(osm) => {
                            let mut got: Vec<(f64, f64, String, PoiCat)> = osm.into_iter().map(|(a, b, nm)| (a, b, nm, PoiCat::Other)).collect();
                            got.sort_by(|p, r| haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&haversine_km((lat, lon), (r.0, r.1))).unwrap_or(std::cmp::Ordering::Equal));
                            mine.extend(got);
                            if mine.is_empty() { snd.play("error"); addr = format!("周辺に無し: {q}"); }
                            else {
                                pois = mine; poi_sel = 0; poi_label = format!("周辺:{q}");
                                set_markers(&mut spec, &wps, &pois);
                                if matches!(focus, Focus::Map) { focus = Focus::PoiList; }
                            }
                        }
                        Err(e) => {
                            snd.play("error");
                            if mine.is_empty() {
                                addr = format!("周辺検索: {e}"); // 障害。「該当なし」と文言を分ける
                            } else {
                                addr = format!("★のみ表示({e})");
                                pois = mine; poi_sel = 0; poi_label = format!("周辺:{q}");
                                set_markers(&mut spec, &wps, &pois);
                                if matches!(focus, Focus::Map) { focus = Focus::PoiList; }
                            }
                        }
                    }
                    near_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { near_job = None; got_result = true; }
            }
        }
        if road_job.is_some() {
            match road_job.as_ref().unwrap().try_recv() {
                Ok((name, res)) => {
                    match res {
                        Ok(frags) if !frags.is_empty() => {
                            let rf: Vec<roadtrace::RoadFrag> = frags.into_iter().map(|(pts, oneway)| roadtrace::RoadFrag { pts, oneway }).collect();
                            let poly = roadtrace::assemble_polyline(&rf);
                            let seg = roadtrace::nearest_segment(&poly, (lat, lon), 500.0);
                            if seg.len() >= 2 {
                                let color = road_color_for(road_segs.len());
                                road_segs.push(RoadSeg { name: name.clone(), color, pts: seg });
                                sync_roads!();
                                addr = format!("道路: {name} を塊で追加(計{}本)", road_segs.len());
                            } else { addr = "道路: 点が足りない(拡大/移動して再検索)".into(); }
                        }
                        Ok(_) => addr = format!("道路が見つからない: {name}(view内に無い)"),
                        Err(e) => addr = format!("道路: {e}"),
                    }
                    road_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { road_job = None; got_result = true; }
            }
        }
        if catpoi_job.is_some() {
            match catpoi_job.as_ref().unwrap().try_recv() {
                Ok((label, res)) => {
                    match res {
                        Ok(items) if !items.is_empty() => { pois = items; poi_sel = 0; poi_label = label; set_markers(&mut spec, &wps, &pois); focus = Focus::PoiList; }
                        Ok(_) => { snd.play("error"); addr = format!("周辺2kmに{label}無し"); if matches!(focus, Focus::Map) { focus = Focus::PoiMenu; } }
                        Err(e) => { addr = format!("({e})"); if matches!(focus, Focus::Map) { focus = Focus::PoiMenu; } }
                    }
                    catpoi_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { catpoi_job = None; got_result = true; }
            }
        }
        if wander_job.is_some() {
            match wander_job.as_ref().unwrap().try_recv() {
                Ok(res) => {
                    match res {
                        Ok(w) => { wps = w; wp_sel = 0; route_sel = 0; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; }
                        Err(e) => { snd.play("error"); addr = format!("({e})"); }
                    }
                    wander_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { wander_job = None; got_result = true; }
            }
        }
        if street_job.is_some() {
            match street_job.as_ref().unwrap().try_recv() {
                Ok((la, lo, hd, res)) => {
                    match res {
                        Ok(img) => { street = Some((img, hd, la, lo)); addr.clear(); }
                        Err(e) => addr = format!("実写: {e}"),
                    }
                    street_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { street_job = None; got_result = true; }
            }
        }
        // 雨雲レーダーの時刻一覧(5分ごと)。届いていれば最新の1件だけを採用する。
        // targetTimes は更新のたびに basetime が動き、古いコマは JMA 側から消えるため、
        // 表示位置は index でなく直前に見ていた validtime を基準に取り直す(reanchor)。
        if let Some(rc) = &radar_clock {
            let mut latest: Option<radar::Timeline> = None;
            while let Ok(tl) = rc.rx.try_recv() { latest = Some(tl); }
            if let Some(tl) = latest {
                let prev_vt = radar_tl.get(radar_idx).map(|f| f.validtime.clone());
                let (idx, follow, msg) = tl.reanchor(prev_vt.as_deref(), radar_follow);
                radar_tl = tl;
                radar_idx = idx;
                radar_follow = follow;
                if let Some(m) = msg { addr = format!("雨雲: {m}"); }
                // 一覧から消えたコマのタイルはもう取得できない。キャッシュと取得キューから捨てる。
                loader.drop_radar_frames_except(&radar_tl.frames);
                got_result = true;
            }
        }
        if recommend_job.is_some() {
            match recommend_job.as_ref().unwrap().try_recv() {
                Ok(res) => {
                    match res {
                        Ok(v) if v.is_empty() => addr = "おすすめ: 実在確認できる地点なし".into(),
                        Ok(v) => {
                            pois = v.into_iter().map(|(la, lo, nm)| (la, lo, nm, PoiCat::Home)).collect();
                            poi_sel = 0; poi_label = "おすすめ".into();
                            set_markers(&mut spec, &wps, &pois);
                            if matches!(focus, Focus::Map) { focus = Focus::PoiList; }
                        }
                        Err(e) => addr = format!("おすすめ: {e}"),
                    }
                    recommend_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { recommend_job = None; got_result = true; }
            }
        }

        // 入力待ち。結果適用直後は即再描画(None)。ジョブ/GPS/再生/移動settling中はポーリング。
        // settling中は短間隔(60ms)で見に行き、動きが止まったフレームで高解像度に上げ直す。
        // ローダーがまだ未取得タイルを抱えている間もポーリング側に倒す(read()でブロックすると
        // 無入力時に届いたタイルが画面へ反映されないため)。
        // is_busy()に加えgenerationのスナップショット比較も見る(#53): このフレームの再構築後、
        // is_busy()を読むまでの間に最後の1枚がちょうど着地しinflightが空になっていた場合、
        // is_busy()だけではその1枚の反映漏れを検知できずread()でブロックしてしまうため。
        let polling = route_job.is_some() || search_job.is_some() || near_job.is_some() || street_job.is_some() || cam_job.is_some() || recommend_job.is_some() || road_job.is_some() || catpoi_job.is_some() || wander_job.is_some() || gps_rx.is_some() || play.is_some() || settling || loader.is_busy() || loader.generation() != loader_gen_snapshot
            || radar_clock.is_some() // 雨雲: 背景ポーラーからの時刻一覧を取りこぼさない
            // 道路交通量/主要道路/ライブカメラ/通行規制の背景取得完了を、キー入力無しでも
            // 取りこぼさない(結果が最大60秒(IDLE_SAVE_INTERVAL)反映されない事故を防ぐ)。
            // 主要道路は以前この条件から漏れていたが、4レイヤとも同じ扱いにする。
            // 人口メッシュは1セルの取得に数十秒かかるため、ここから漏れると
            // 「取得中…」の表示すら出ないまま画面が固まって見える(PTY実機で確認済み)。
            || traffic_layer.job_active() || roads_layer.job_active()
            || camera_layer.job_active() || regulation_layer.job_active() || disaster_layer.job_active()
            || boundary_layer.job_active() || population_layer.job_active()
            || disaster_job.is_some() || voice_preview_job.is_some() || regulation_detail_job.is_some() || traffic_color_job.is_some() || cause_job.is_some() || route_warning_job.is_some();
        let mut ev: Option<Event> = if got_result {
            None
        } else if polling {
            let ms = if settling { 60 } else { 80 };
            if event::poll(std::time::Duration::from_millis(ms))? { Some(event::read()?) } else { None }
        } else if event::poll(IDLE_SAVE_INTERVAL)? {
            Some(event::read()?)
        } else {
            // 無操作がIDLE_SAVE_INTERVALだけ続いた。read()で無限ブロックする代わりにpollで
            // 区切り、強制終了/クラッシュに備えて状態を保存する(#69)。キー入力があれば
            // pollは即trueを返すため応答性への影響は無い。
            persist_full_state(cx, cy, z, &opts, &wps, &mode, &mut cfg, radar_on, show_spots);
            // ついでにプロットキャッシュの掃除もここで起こす。プロットデータは取得のたびに
            // その場で1ファイル書いているので「保存待ち」は無く、フラッシュする対象は無い。
            // 一方GCはディレクトリ走査を伴うので無操作中に回すのが都合がよい。
            // ここへ来るのはジョブが1本も走っていない時だけなので取得とも競合しない。
            if !plot_gc_done {
                plot_gc_done = true; // 1セッション1回だけ
                std::thread::spawn(plotcache::gc);
            }
            None
        };
        // 押しっぱなし/連打でパン系イベントが溜まっている間は、都度の再描画を待たずに
        // 溜まった分を最新の1個へ間引く(SSH等で1回の再描画に往復が乗ると、律速して
        // メニュー操作等の割り込みが後回しになるため)。別系統のキーが混ざっていたら
        // 間引きを止めてそちらを即座に優先する。
        if matches!(focus, Focus::Map) {
            if let Some(Event::Key(first)) = &ev {
                let is_pan_key = |c: KeyCode| matches!(c, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Char('h') | KeyCode::Char('j') | KeyCode::Char('k') | KeyCode::Char('l'));
                if is_pan_key(first.code) {
                    while let Ok(true) = event::poll(std::time::Duration::from_millis(0)) {
                        match event::read()? {
                            Event::Key(next) if is_pan_key(next.code) => ev = Some(Event::Key(next)),
                            other => { ev = Some(other); break; }
                        }
                    }
                }
            }
        }
        // web版(ブラウザ)からのパン量マーカー(#87 設計書 §6.3)。上のキー間引きは「溜まった分を
        // 最新1個で上書き」=捨てる方式だが、パン量は相対値なので足し合わせれば取りこぼしがゼロに
        // なる。描画が遅れて数フレーム分溜まっても、指を離した時点の位置に必ず追いつく。
        // ev を別イベントで上書きしても合算値はこの変数に残るので、途中で別のキーが割り込んでも
        // 移動分は失われない。Focus::Map 以外(PoiList の横パン等)でも効かせるためキー間引きの
        // 内側には置かない。
        let mut pan_fx = 0.0f64;
        let mut pan_fy = 0.0f64;
        let mut got_pan = false;
        if matches!(&ev, Some(Event::Paste(s)) if s.starts_with(dragmode::PAN_MARKER)) {
            if let Some(Event::Paste(s)) = &ev {
                if let Some((fx, fy)) = dragmode::parse_pan_marker(s) { pan_fx += fx; pan_fy += fy; }
            }
            got_pan = true;
            ev = None; // このイベントは消費済み。以降の match へ素通しさせない(検索欄への誤入力防止)
            while let Ok(true) = event::poll(std::time::Duration::from_millis(0)) {
                match event::read()? {
                    Event::Paste(s) if s.starts_with(dragmode::PAN_MARKER) => {
                        if let Some((fx, fy)) = dragmode::parse_pan_marker(&s) { pan_fx += fx; pan_fy += fy; }
                    }
                    other => { ev = Some(other); break; }
                }
            }
        }
        if got_pan {
            // 軸ゲート・向きの反転・座標の正規化は dragmode::apply_pan に閉じてある
            // (ここに直書きするとテストが書けないため。設計書 §6.2 の適用条件)。
            // 実画像モードでは実際に描かれる解像度は ow/oh ではなく rw/rh(zoom rz のピクセル)。
            // 表示している地理範囲は zoom z 換算で常に横 map_cols・縦 map_rows*2 ピクセルなので、
            // rw/scale・rh/scale へ戻して渡す(設計 §5.5 対策E)。AA 用に計算された ow/oh を
            // そのまま渡すと、braille(または --edge)と実画像を同時に有効にしたときだけ
            // ow=map_cols*2 / oh=map_rows*4 となり、両軸とも指の2倍地図が動く(§2.5 の実測)。
            let (pan_ow, pan_oh) = if img_inline { (rw / scale, rh / scale) } else { (ow, oh) };
            let lay = dragmode::Layout { cols, rows: tr as u32, map_cols, map_rows, ow: pan_ow, oh: pan_oh };
            let (ncx, ncy, moved) = dragmode::apply_pan(cx, cy, z, dragmode::axes(&focus), pan_fx, pan_fy, &lay);
            if moved {
                cx = ncx;
                cy = ncy;
                // 中心が動いたので、'a'で引いた住所表示は古くなる。矢印キーでのパンと同じく
                // 地図フォーカスのときだけ消す(PoiListの微パンはキー経路でも消していない)。
                if matches!(focus, Focus::Map) { addr.clear(); }
                // キーボードの加速(pan_streak)と混ざらないようリセットする。ドラッグは
                // 指の移動量そのものが移動量なので、加速を掛けると1:1でなくなる。
                pan_streak = 0;
                last_pan_dir = None;
            }
        }
        match ev {
            None => {} // 再描画のみ(計算待ち)
            Some(Event::Key(k)) if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl-C: 進行中の全ジョブを中断(アプリは終了しない)
                let any = route_job.is_some() || search_job.is_some() || near_job.is_some() || street_job.is_some() || cam_job.is_some() || recommend_job.is_some() || road_job.is_some() || catpoi_job.is_some() || wander_job.is_some() || disaster_job.is_some() || regulation_detail_job.is_some() || traffic_color_job.is_some() || cause_job.is_some();
                if any {
                    if route_job.is_some() { route_note = Some("中断".to_string()); }
                    route_job = None; search_job = None; near_job = None; street_job = None; cam_job = None; recommend_job = None; road_job = None; catpoi_job = None; wander_job = None; disaster_job = None; regulation_detail_job = None; traffic_color_job = None; cause_job = None;
                    addr = "中断".into();
                }
            }
            Some(Event::Key(k)) if onboard => { // 何かキーで閉じる。d のときだけ「次回から非表示」マーカーを書く(既定は毎回表示)
                if matches!(k.code, KeyCode::Char('d') | KeyCode::Char('D')) {
                    if let Some(p) = onboarded_marker() { let _ = crate::fsutil::write_atomic(&p, b"1", None); }
                    addr = "オンボーディング: 次回から非表示(設定で再表示)".into();
                }
                onboard = false;
                force_reemit = true; // 次フレームで確実に地図を再構築・再emitし、覆っていた分の残像を消す
                last_map_sig = None; // 実画像モードのsig一致スキップに巻き込まれず必ず再取得させる
            }
            Some(Event::Key(k)) if quit_confirm => { // 終了確認: y=終了/他=取消
                if let KeyCode::Char('y') | KeyCode::Char('Y') = k.code { break; }
                quit_confirm = false;
            }
            Some(Event::Key(_)) if qr_view.is_some() => { qr_view = None; force_reemit = true; } // ポップアップを閉じる(即座に再emitして残像を消す)
            Some(Event::Key(_)) if popup.is_some() => { popup = None; force_reemit = true; } // 名前ポップアップを閉じる(同上)
            // 災害事例パネルを閉じる。qr_view/popup と同じく任意キーで閉じる(Esc/qを含む)。
            // ここで全キーを受け止めないと、パネルに覆われた地図側のキー(v で地点追加等)が
            // 見えないまま発火してしまう。
            Some(Event::Key(_)) if disaster_view.is_some() => { disaster_view = None; force_reemit = true; }
            // 通行規制の詳細パネルを閉じる。disaster_view と同じく任意キーで閉じる。
            Some(Event::Key(_)) if regulation_detail_view.is_some() => { regulation_detail_view = None; force_reemit = true; }
            Some(Event::Key(k)) if spot_move_confirm.is_some() => { // 「中心へ移動」の確認(y=実行/他=取消)
                let gi = spot_move_confirm.take().unwrap();
                if let KeyCode::Char('y') = k.code {
                    snd.play("confirm");
                    if let Some(s) = spots.get_mut(gi) { s.lat = lat; s.lon = lon; }
                    let _ = save_all_spots(&spots); apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                    addr = "スポット位置を中心へ移動".into();
                } else { addr = "移動を取消".into(); }
            }
            Some(Event::Key(k)) if save_confirm.is_some() => { // 同名の上書き確認(y=上書き/他=名前を変更して新規登録)
                let name = save_confirm.take().unwrap();
                if let KeyCode::Char('y') = k.code {
                    addr = match save_named_route(&name, &mode, &wps) { Ok(_) => { snd.play("confirm"); route_name_hint = name.clone(); format!("上書き保存: {name}") }, Err(e) => format!("({e})") };
                    focus = Focus::Map;
                }
                // else: キャンセル。focusは既にFocus::SaveNameのままなので、名前を変えて新規登録できる
            }
            Some(Event::Key(k)) if clear_route_confirm => { // ルート全消去の確認(y=消去/他=取消)
                clear_route_confirm = false;
                if let KeyCode::Char('y') = k.code {
                    wps.clear(); wp_sel = 0; route_sel = 0; road_segs.clear(); spec.roads.clear();
                    let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_;
                    addr = "ルート消去".into();
                } else { addr = "消去を取消".into(); }
            }
            // Map表示中のEscは進行中ジョブの中断に使う(サブ画面のEscは各Focusの取消のまま)
            Some(Event::Key(k)) if k.code == KeyCode::Esc && matches!(focus, Focus::Map)
                && (route_job.is_some() || search_job.is_some() || near_job.is_some() || street_job.is_some() || cam_job.is_some() || recommend_job.is_some() || road_job.is_some() || catpoi_job.is_some() || wander_job.is_some() || disaster_job.is_some() || regulation_detail_job.is_some() || traffic_color_job.is_some() || cause_job.is_some()) => {
                if route_job.is_some() { route_note = Some("中断".to_string()); }
                route_job = None; search_job = None; near_job = None; street_job = None; cam_job = None; recommend_job = None; road_job = None; catpoi_job = None; wander_job = None; disaster_job = None; regulation_detail_job = None; traffic_color_job = None; cause_job = None;
                addr = "中断".into();
            }
            Some(Event::Key(k)) => {
                let cur = std::mem::replace(&mut focus, Focus::Map);
                match cur {
                    Focus::Search(mut buf) => match k.code {
                        KeyCode::Enter => { // 候補を一覧表示(左袖)。Enterで移動/s e vで経路点
                            let q = buf.trim().to_string();
                            if !q.is_empty() {
                                // provider は Google キーの有無で分ける(キーあり=Google優先"g"/無し=Nominatim"n")。言語は ja 固定。
                                let provider = if cfg.google_maps_api_key.trim().is_empty() { "n" } else { "g" };
                                let ckey = searchcache::make_key(provider, "ja", &q, lat, lon);
                                // キャッシュヒットは即適用(同期)。ミス時のみ別スレッドで検索(通信/サーバ障害は0件と区別)。
                                // ヒット時は last_used を更新(LRU破棄の基準。次回 save 時に永続化される)。
                                let hit = scache.get_mut(&ckey).map(|e| { e.last_used_at = searchcache::now_secs(); e.results.clone() });
                                if let Some(v) = hit {
                                    if v.is_empty() { snd.play("error"); addr = format!("見つからない: {q}"); }
                                    else {
                                        pois = v.into_iter().take(8).map(|(la, lo, nm)| (la, lo, nm, PoiCat::Waypoint)).collect();
                                        poi_sel = 0;
                                        poi_label = format!("検索:{q}");
                                        set_markers(&mut spec, &wps, &pois);
                                        focus = Focus::PoiList;
                                    }
                                } else {
                                    let q2 = q.clone(); let ckey2 = ckey.clone();
                                    let key = cfg.google_maps_api_key.clone();
                                    let (tx, rx) = std::sync::mpsc::channel();
                                    std::thread::spawn(move || {
                                        let r = geocode_list(&q2, Some((lat, lon)), &key).map_err(|e| e.to_string());
                                        let _ = tx.send((ckey2, q2, r));
                                    });
                                    search_job = Some(rx);
                                    focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                                }
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::Search(buf); } // ←→/文字/BS/Del/Home/End
                    },
                    Focus::SpotCatList => match k.code { // カテゴリ一覧(P)
                        KeyCode::Up | KeyCode::Char('w') => { snd.play("click"); cat_sel = cat_sel.saturating_sub(1); focus = Focus::SpotCatList; }
                        KeyCode::Down | KeyCode::Char('s') => { snd.play("click"); if cat_sel + 1 < spot_cats.len() { cat_sel += 1; } focus = Focus::SpotCatList; }
                        KeyCode::Char('n') => { input_cur = 0; focus = Focus::NewCat(String::new()); }
                        KeyCode::Char('[') => { // 選択カテゴリを上へ
                            if cat_sel > 0 && cat_sel < spot_cats.len() { spot_cats.swap(cat_sel, cat_sel - 1); cat_sel -= 1; let _ = save_all_cats(&spot_cats); }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Char(']') => { // 選択カテゴリを下へ
                            if cat_sel + 1 < spot_cats.len() { spot_cats.swap(cat_sel, cat_sel + 1); cat_sel += 1; let _ = save_all_cats(&spot_cats); }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Char('r') => { if let Some((n, _, _)) = spot_cats.get(cat_sel) { input_cur = n.chars().count(); focus = Focus::SpotRename(n.clone(), cat_sel); } else { focus = Focus::SpotCatList; } }
                        KeyCode::Char('c') => {
                            match spot_cats.get(cat_sel) {
                                Some((_, ci, _)) => { color_sel = *ci; focus = Focus::ColorPick { cat: cat_sel }; }
                                None => focus = Focus::SpotCatList,
                            }
                        }
                        KeyCode::Char('M') => { // 形状ピッカー(色 c とは独立に形を選ぶ)
                            match spot_cats.get(cat_sel) {
                                Some((_, _, sh)) => { shape_sel = *sh; focus = Focus::ShapePick { cat: cat_sel }; }
                                None => focus = Focus::SpotCatList,
                            }
                        }
                        KeyCode::Char('x') => {
                            if let Some((name, _, _)) = spot_cats.get(cat_sel).cloned() {
                                if spots.iter().any(|s| s.cat == name) { addr = format!("使用中: {name}(先に空に)"); }
                                else { spot_cats.remove(cat_sel); if cat_sel >= spot_cats.len() && cat_sel > 0 { cat_sel -= 1; } let _ = save_all_cats(&spot_cats); }
                            }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Enter => {
                            let cat = spot_cats.get(cat_sel).map(|(c, _, _)| c.clone());
                            if let Some((la, lo, nm)) = pending_spot.take() {
                                // 検索結果からの登録: 選択カテゴリに新規スポットとして保存
                                if let Some(cat) = cat {
                                    snd.play("pop");
                                    let s = Spot { lat: la, lon: lo, cat: cat.clone(), name: spot_clean(&nm) };
                                    let _ = append_spot(&s);
                                    spots.push(s);
                                    show_spots = true;
                                    apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                                    addr = format!("★登録: {} [{}]", if nm.is_empty() { "(無名)" } else { nm.as_str() }, cat);
                                }
                                focus = Focus::Map;
                            } else if let Some(cat) = cat {
                                cur_cat = cat; sp_sel = 0; focus = Focus::SpotList;
                            } else { focus = Focus::SpotCatList; }
                        }
                        // 登録キャンセル時も保留を消す→Mapへ。左袖(カテゴリ一覧)の残像を残さないよう
                        // 全消去してから次フレームで再構築させる(Menu閉じる時と同じ理由)。
                        KeyCode::Esc => { snd.play("back"); pending_spot = None; focus = Focus::Map; let _ = write!(out, "\x1b[2J"); force_reemit = true; }
                        _ => focus = Focus::SpotCatList,
                    },
                    Focus::Settings => { let mut stay = true; let mut changed = false; match k.code { // 設定画面
                        KeyCode::Up | KeyCode::Char('w') => { snd.play("click"); set_sel = set_sel.saturating_sub(1); }
                        // 下端は settings.rs の行数定義から取る(生の数値で持つと項目追加のたびに手で同期する羽目になる)
                        KeyCode::Down | KeyCode::Char('s') => { snd.play("click"); if set_sel + 1 < settings::SETTINGS_ROW_COUNT { set_sel += 1; } }
                        KeyCode::Left | KeyCode::Right => {
                            if set_sel == 6 { let d = if k.code == KeyCode::Left { -100.0 } else { 100.0 }; cfg.sample_interval_m = (cfg.sample_interval_m + d).clamp(100.0, 5000.0); changed = true; }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            if set_sel == 6 { // 道路の点間隔: インライン数値編集を開く
                                let b = format!("{}", cfg.sample_interval_m as i64);
                                input_cur = b.chars().count();
                                focus = Focus::SettingsEdit(6, b);
                                stay = false;
                            } else if set_sel == 17 { // Google APIキー: インラインテキスト編集を開く(Cmd+V貼付も引き続き可)
                                let b = cfg.google_maps_api_key.clone();
                                input_cur = b.chars().count();
                                focus = Focus::SettingsEdit(17, b);
                                stay = false;
                            } else if settings::is_pickable(set_sel) { // 3択以上の項目: サイドの一覧(SettingsPick)を開いて直接選ぶ
                                set_pick_sel = settings::pick_current(set_sel, &cfg, &opts.style);
                                focus = Focus::SettingsPick(set_sel);
                                stay = false;
                            } else {
                                changed = true;
                                match set_sel {
                                    // 表示に効くAAスタイル(braille/classify/edge/mono)は、map_sigに含まれない
                                    // opts側の状態なので、切替時はforce_reemitで確実に次フレーム反映させる。
                                    0 => { opts.braille = !opts.braille; force_reemit = true; }
                                    1 => { opts.classify = !opts.classify; force_reemit = true; }
                                    2 => { opts.edge = !opts.edge; force_reemit = true; }
                                    3 => { opts.mono = !opts.mono; force_reemit = true; }
                                    7 => { cfg.show_spots = !cfg.show_spots; show_spots = cfg.show_spots; apply_spots(&mut spec, &spots, &spot_cats, show_spots); }
                                    8 => cfg.llm_recommend_enabled = !cfg.llm_recommend_enabled,
                                    10 => cfg.streetview_enabled = !cfg.streetview_enabled,
                                    11 => { cfg.image_mode = !cfg.image_mode; force_reemit = true; }
                                    13 => cfg.image_settle_low_res = !cfg.image_settle_low_res,
                                    14 => { cfg.sound_enabled = !cfg.sound_enabled; snd = sound::Sound::new(cfg.sound_enabled); snd.play("confirm"); }
                                    15 => { // オンボーディング: マーカーの削除=毎回表示 / 作成=次回から非表示
                                        if let Some(p) = onboarded_marker() {
                                            if p.exists() { let _ = std::fs::remove_file(&p); addr = "オンボーディング: 毎回表示に戻した".into(); }
                                            else { let _ = crate::fsutil::write_atomic(&p, b"1", None); addr = "オンボーディング: 次回から非表示".into(); }
                                        }
                                    }
                                    19 => { // 雨雲レーダー: 起動時の既定を切り替え、いま表示中の地図にも即反映する
                                        cfg.radar_enabled = !cfg.radar_enabled;
                                        if cfg.radar_enabled != radar_on { radar_toggle!(); }
                                    }
                                    21 => { // ルート音声案内: ONにした時、既にルートがあれば曲がり角を取りに行く
                                        cfg.voice_guide_enabled = !cfg.voice_guide_enabled;
                                        if cfg.voice_guide_enabled {
                                            if let Some(pts) = spec.routes.last().map(|rt| rt.pts.clone()) {
                                                turn_job = Some(trigger_turn_points(&wps, &mode, 0, &pts, &route_nogos));
                                            }
                                        }
                                    }
                                    // 道路交通量/ライブカメラ/通行規制: ONにした時の後始末は不要。
                                    // 次のtickでセル表を見に行き、キャッシュがfreshならディスクから
                                    // 即座に出す(ONにした瞬間に前回の内容が出て、必要なら裏で更新される)。
                                    22 => { cfg.traffic_enabled = !cfg.traffic_enabled; }
                                    23 => { cfg.voice_speak_local = !cfg.voice_speak_local; }
                                    24 => { cfg.camera_enabled = !cfg.camera_enabled; }
                                    25 => { cfg.regulation_enabled = !cfg.regulation_enabled; }
                                    26 => { // 過去災害: ONにした直後だけ出典を1回出す(雨雲レーダーと同じ扱い)
                                        cfg.disaster_enabled = !cfg.disaster_enabled;
                                        if cfg.disaster_enabled { addr = "過去災害: 防災科学技術研究所 災害事例データベース".into(); }
                                    }
                                    28 => { // 渋滞状況の色分け: 次にルートが確定したタイミングで初めて問い合わせる
                                        cfg.route_traffic_enabled = !cfg.route_traffic_enabled;
                                        if cfg.route_traffic_enabled && cfg.google_maps_api_key.trim().is_empty() {
                                            addr = "渋滞状況の色分け: Google APIキー未設定".into();
                                        }
                                    }
                                    29 => { disaster_fill_toggle!(); } // 過去災害の塗り: Fキー・Spaceメニューと共通処理
                                    30 => { population_toggle!(); } // 人口メッシュ: Uキー・Spaceメニューと共通処理
                                    33 => { // ルート沿い気象警報: 次にルートが確定したタイミングで初めて問い合わせる
                                        cfg.weather_warning_enabled = !cfg.weather_warning_enabled;
                                        if cfg.weather_warning_enabled && spec.routes.is_empty() {
                                            addr = "ルート沿い気象警報: ルート未確定".into();
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); stay = false; let _ = write!(out, "\x1b[2J"); force_reemit = true; } // 閉じる→Map。他の左袖パネルと同じく残像防止に全消去+再emit
                        _ => {}
                    }
                    if changed { // 変更のたびに opts→cfg を同期して即保存(sを押さなくてよい)
                        cfg.braille = opts.braille; cfg.classify = opts.classify; cfg.edge = opts.edge; cfg.mono = opts.mono; cfg.style = opts.style.clone();
                        let _ = config::save_config(&cfg);
                    }
                    if stay { focus = Focus::Settings; } },
                    Focus::SettingsEdit(idx, mut buf) => match k.code {
                        KeyCode::Enter => {
                            if idx == 6 {
                                match buf.trim().parse::<f64>() {
                                    Ok(v) => { cfg.sample_interval_m = v.clamp(100.0, 5000.0); let _ = config::save_config(&cfg); addr = format!("道路の点間隔: {}m", cfg.sample_interval_m as i64); }
                                    Err(_) => { snd.play("error"); addr = "数値を入力してください(例: 800)".into(); }
                                }
                            } else if idx == 17 {
                                let v = buf.trim().to_string();
                                if v.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                                    cfg.google_maps_api_key = v; let _ = config::save_config(&cfg); addr = "APIキー設定(自動保存)".into();
                                } else { snd.play("error"); addr = "APIキーに使えない文字が含まれています".into(); }
                            }
                            focus = Focus::Settings;
                        }
                        KeyCode::Esc => { snd.play("back"); focus = Focus::Settings; } // 編集を破棄
                        // 数値欄(道路の点間隔)は数字/小数点/マイナスのみ受け付ける。APIキー欄は制御文字・改行を弾く。
                        KeyCode::Char(c) if idx == 6 && !(c.is_ascii_digit() || c == '.' || c == '-') => {}
                        KeyCode::Char(c) if idx == 17 && !(c.is_ascii_graphic() || c == ' ') => {}
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::SettingsEdit(idx, buf); }
                    },
                    Focus::RoadSearch(mut buf) => match k.code { // 道路名/ref で現在view内をルート化
                        KeyCode::Enter => {
                            let name = buf.trim().to_string();
                            if !name.is_empty() {
                                let (n_lat, w_lon) = pixel_to_deg(cx - ow as f64 / 2.0, cy - oh as f64 / 2.0, z);
                                let (s_lat, e_lon) = pixel_to_deg(cx + ow as f64 / 2.0, cy + oh as f64 / 2.0, z);
                                let (tx, rx) = std::sync::mpsc::channel();
                                let name2 = name.clone();
                                std::thread::spawn(move || {
                                    let r = roadsearch::fetch(&name2, s_lat, w_lon, n_lat, e_lon);
                                    let _ = tx.send((name2, r));
                                });
                                road_job = Some(rx);
                                focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::RoadSearch(buf); }
                    },
                    Focus::Recommend(mut buf) => match k.code { // おすすめ: 方向性→claude -p→実在確認→候補一覧
                        KeyCode::Enter => {
                            let dir = buf.trim().to_string();
                            if !dir.is_empty() {
                                // AI提案→実在確認(geocode)ループを別スレッドで回し、検証済みスポット列を返す。
                                let cmd = cfg.llm_command.clone();
                                let model = cfg.llm_model.clone();
                                let key = cfg.google_maps_api_key.clone();
                                let (tx, rx) = std::sync::mpsc::channel();
                                std::thread::spawn(move || {
                                    let payload: Result<Vec<(f64, f64, String)>, String> = match recommend::recommend(&cmd, &model, &dir) {
                                        Ok(recs) => {
                                            let mut verified: Vec<(f64, f64, String)> = Vec::new();
                                            for r in recs.iter().take(8) {
                                                let q = if r.area.is_empty() { r.name.clone() } else { format!("{} {}", r.area, r.name) };
                                                if let Ok((la, lo)) = geocode(&q, Some((lat, lon)), &key) {
                                                    verified.push((la, lo, r.name.clone()));
                                                }
                                            }
                                            Ok(verified)
                                        }
                                        Err(e) => Err(e),
                                    };
                                    let _ = tx.send(payload);
                                });
                                recommend_job = Some(rx);
                                focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::Recommend(buf); }
                    },
                    Focus::SpotList => match k.code { // cur_cat のスポット一覧
                        KeyCode::Up | KeyCode::Char('w') => { snd.play("click"); sp_sel = sp_sel.saturating_sub(1); focus = Focus::SpotList; }
                        KeyCode::Down | KeyCode::Char('s') => { snd.play("click"); let n = spots.iter().filter(|s| s.cat == cur_cat).count(); if sp_sel + 1 < n { sp_sel += 1; } focus = Focus::SpotList; }
                        KeyCode::Char('n') => { input_cur = 0; focus = Focus::SpotForm { name: String::new(), url: String::new(), field: 0 }; } // 新規スポット登録フォーム
                        KeyCode::Char('[') => { // 選択スポットを同カテゴリ内で上へ
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if sp_sel > 0 && sp_sel < idxs.len() { spots.swap(idxs[sp_sel], idxs[sp_sel - 1]); sp_sel -= 1; let _ = save_all_spots(&spots); }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Char(']') => { // 選択スポットを同カテゴリ内で下へ
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if sp_sel + 1 < idxs.len() { spots.swap(idxs[sp_sel], idxs[sp_sel + 1]); sp_sel += 1; let _ = save_all_spots(&spots); }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Char('r') => { // 選択スポットを改名
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            match idxs.get(sp_sel) { Some(&gi) => { input_cur = spots[gi].name.chars().count(); focus = Focus::SpotEditName(spots[gi].name.clone(), gi); } None => focus = Focus::SpotList }
                        }
                        KeyCode::Char('m') => { // 選択スポットを現在の中心へ移動(破壊的なので確認待ちにするだけ)
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(sp_sel) { spot_move_confirm = Some(gi); }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Enter => {
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(sp_sel) { let (nx, ny) = deg_to_pixel(spots[gi].lat, spots[gi].lon, z); cx = nx; cy = ny; }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Char('x') => {
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(sp_sel) {
                                spots.remove(gi);
                                if sp_sel > 0 && sp_sel >= idxs.len() - 1 { sp_sel -= 1; }
                                let _ = save_all_spots(&spots);
                                apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                            }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Esc => { snd.play("back"); focus = Focus::SpotCatList; }
                        _ => focus = Focus::SpotList,
                    },
                    Focus::SpotEditName(mut buf, gi) => match k.code { // スポット改名
                        KeyCode::Enter => {
                            snd.play("confirm");
                            let new = spot_clean(buf.trim());
                            if let Some(s) = spots.get_mut(gi) { s.name = new; }
                            let _ = save_all_spots(&spots);
                            apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                            focus = Focus::SpotList;
                        }
                        KeyCode::Esc => focus = Focus::SpotList,
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::SpotEditName(buf, gi); }
                    },
                    Focus::NewCat(mut buf) => match k.code {
                        KeyCode::Enter => { let name = buf.trim().to_string(); if !name.is_empty() { snd.play("confirm"); let _ = ensure_spot_cat(&name, &mut spot_cats); } focus = Focus::SpotCatList; }
                        KeyCode::Esc => { snd.play("back"); focus = Focus::SpotCatList; }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::NewCat(buf); }
                    },
                    Focus::SpotRename(mut buf, idx) => match k.code {
                        KeyCode::Enter => {
                            let new = spot_clean(buf.trim());
                            if !new.is_empty() {
                                if let Some(old) = spot_cats.get(idx).map(|(n, _, _)| n.clone()) {
                                    for s in spots.iter_mut() { if s.cat == old { s.cat = new.clone(); } }
                                    if let Some(e) = spot_cats.get_mut(idx) { e.0 = new; }
                                    let _ = save_all_spots(&spots);
                                    let _ = save_all_cats(&spot_cats);
                                    apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                                }
                            }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Esc => focus = Focus::SpotCatList,
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::SpotRename(buf, idx); }
                    },
                    Focus::SpotForm { mut name, mut url, mut field } => match k.code { // 新規スポット登録フォーム
                        KeyCode::Up | KeyCode::BackTab => { field = (field + 3) % 4; input_cur = form_cur(&name, &url, field); focus = Focus::SpotForm { name, url, field }; }
                        KeyCode::Down | KeyCode::Tab => { field = (field + 1) % 4; input_cur = form_cur(&name, &url, field); focus = Focus::SpotForm { name, url, field }; }
                        KeyCode::Esc => { snd.play("back"); focus = Focus::SpotList; } // 取消
                        KeyCode::Enter => match field {
                            0 => { field = 1; input_cur = url.chars().count(); focus = Focus::SpotForm { name, url, field }; } // 次のフィールドへ
                            1 => { field = 2; input_cur = 0; focus = Focus::SpotForm { name, url, field }; }
                            3 => focus = Focus::SpotList, // [戻る]
                            _ => { // 2 = [送信]
                                let u = url.trim();
                                let name_in = spot_clean(name.trim()); // 名称buf(整形済)
                                // URL非空: parse_gmaps_placeで(lat,lon,店名)。空: 現在地(中心)+名称。両方空: 何もしない
                                enum Act { Save(f64, f64, String), Err(String), Nop }
                                let act = if u.is_empty() && name_in.is_empty() { Act::Nop }
                                    else if u.is_empty() { Act::Save(lat, lon, if name_in.is_empty() { "(無名)".into() } else { name_in.clone() }) }
                                    else if u.contains("goo.gl") || u.contains("maps.app") { Act::Err("短縮URLは不可。Googleマップの通常URL(…/@…/!3d…!4d…)を貼って".into()) }
                                    else if let Some((la, lo, nm)) = parse_gmaps_place(u) {
                                        let nm = spot_clean(&nm); // URLの名前
                                        let final_name = if !name_in.is_empty() { name_in.clone() } // 名称buf優先
                                            else if !nm.is_empty() { nm } else { "(無名)".into() };
                                        Act::Save(la, lo, final_name)
                                    } else { Act::Err("URLから位置を取得できません(GoogleマップのURLか確認)".into()) };
                                match act {
                                    Act::Save(la, lo, nm) => {
                                        snd.play("confirm");
                                        let s = Spot { lat: la, lon: lo, cat: cur_cat.clone(), name: nm };
                                        let _ = ensure_spot_cat(&s.cat, &mut spot_cats);
                                        addr = match append_spot(&s) { Ok(_) => format!("スポット保存: {}", s.name), Err(e) => format!("({e})") };
                                        spots.push(s); show_spots = true; apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                                        focus = Focus::SpotList;
                                    }
                                    Act::Err(msg) => { addr = msg; focus = Focus::SpotForm { name, url, field }; }
                                    Act::Nop => focus = Focus::SpotForm { name, url, field },
                                }
                            }
                        },
                        other => { // ←→/文字/BS/Del/Home/End は選択中フィールドを編集(ボタン欄では無視)
                            if field == 0 { edit_line(&mut name, &mut input_cur, other); }
                            else if field == 1 { edit_line(&mut url, &mut input_cur, other); }
                            focus = Focus::SpotForm { name, url, field };
                        }
                    },
                    Focus::PoiKindForm { mut label, mut tag, mut field } => match k.code { // 目的地カテゴリの新規追加フォーム
                        KeyCode::Up | KeyCode::BackTab => { field = (field + 3) % 4; input_cur = form_cur(&label, &tag, field); focus = Focus::PoiKindForm { label, tag, field }; }
                        KeyCode::Down | KeyCode::Tab => { field = (field + 1) % 4; input_cur = form_cur(&label, &tag, field); focus = Focus::PoiKindForm { label, tag, field }; }
                        KeyCode::Esc => { snd.play("back"); focus = Focus::PoiMenu; }
                        KeyCode::Enter => match field {
                            0 => { field = 1; input_cur = tag.chars().count(); focus = Focus::PoiKindForm { label, tag, field }; }
                            1 => { field = 2; input_cur = 0; focus = Focus::PoiKindForm { label, tag, field }; }
                            3 => focus = Focus::PoiMenu, // [戻る]
                            _ => { // 2 = [追加]
                                let label_in = poi_kind_clean(label.trim());
                                let t = tag.trim();
                                let parts: Vec<&str> = t.splitn(2, '=').collect();
                                let bad_char = |s: &str| s.contains('"') || s.contains('\\') || s.contains('\n');
                                if label_in.is_empty() { addr = "表示名を入力してください".into(); focus = Focus::PoiKindForm { label, tag, field }; }
                                else if parts.len() != 2 || parts[0].trim().is_empty() || parts[1].trim().is_empty() || bad_char(t) {
                                    addr = "OSMタグは key=value 形式(例: shop=bakery)".into();
                                    focus = Focus::PoiKindForm { label, tag, field };
                                } else {
                                    let (tk, tv) = (parts[0].trim(), parts[1].trim());
                                    let key = next_free_key(&poi_kinds);
                                    let kind = PoiKind { key, label: label_in.clone(), filter: format!("nwr[\"{tk}\"=\"{tv}\"]"), cat: PoiCat::Other };
                                    poi_kinds.push(kind);
                                    let _ = save_poi_kinds(&poi_kinds);
                                    snd.play("confirm");
                                    addr = format!("カテゴリ追加: {label_in} ({key})");
                                    focus = Focus::PoiMenu;
                                }
                            }
                        },
                        other => {
                            if field == 0 { edit_line(&mut label, &mut input_cur, other); }
                            else if field == 1 { edit_line(&mut tag, &mut input_cur, other); }
                            focus = Focus::PoiKindForm { label, tag, field };
                        }
                    },
                    Focus::WanderForm { mut dist_km } => match k.code { // おまかせ周回: 距離ゲージ
                        KeyCode::Left | KeyCode::Right => {
                            let step = if k.modifiers.contains(KeyModifiers::SHIFT) { 20.0 } else { 5.0 };
                            let d = if k.code == KeyCode::Left { -step } else { step };
                            dist_km = (dist_km + d).clamp(10.0, 200.0);
                            focus = Focus::WanderForm { dist_km };
                        }
                        KeyCode::Esc => { snd.play("back"); focus = Focus::Map; }
                        KeyCode::Enter => {
                            let origin = (lat, lon);
                            let shape = a.shape.clone();
                            let (tx, rx) = std::sync::mpsc::channel();
                            std::thread::spawn(move || {
                                let r = wander_route(origin, dist_km, &shape);
                                let _ = tx.send(r);
                            });
                            wander_job = Some(rx);
                            addr = format!("走りまくり: {dist_km:.0}km圏を検索中…");
                            focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                        }
                        _ => focus = Focus::WanderForm { dist_km },
                    },
                    Focus::NearSearch(mut buf) => match k.code {
                        KeyCode::Enter => {
                            let q = buf.trim().to_string();
                            if !q.is_empty() {
                                // Overpass(遅い)を別スレッドへ。viewbox境界を先に確定して渡す。★マージは結果適用側で行う。
                                let (vt, vl) = pixel_to_deg(cx - ow as f64 * 1.25, cy - oh as f64 * 1.25, z);
                                let (vb, vr) = pixel_to_deg(cx + ow as f64 * 1.25, cy + oh as f64 * 1.25, z);
                                let rlat = 2.0 / 111.0;
                                let rlon = 2.0 / (111.0 * lat.to_radians().cos().abs().max(0.1));
                                let (south, west) = (vb.min(lat - rlat), vl.min(lon - rlon));
                                let (north, east) = (vt.max(lat + rlat), vr.max(lon + rlon));
                                let q2 = q.clone();
                                let (tx, rx) = std::sync::mpsc::channel();
                                std::thread::spawn(move || {
                                    let v = search_nearby(&q2, south, west, north, east);
                                    let _ = tx.send((q2, v));
                                });
                                near_job = Some(rx);
                                focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::NearSearch(buf); }
                    },
                    Focus::PoiMenu => match k.code {
                        KeyCode::Esc => {}
                        KeyCode::Up | KeyCode::Char('w') => { snd.play("click"); poimenu_sel = poimenu_sel.saturating_sub(1); focus = Focus::PoiMenu; }
                        KeyCode::Down | KeyCode::Char('s') => { snd.play("click"); if poimenu_sel + 1 <= poi_kinds.len() { poimenu_sel += 1; } focus = Focus::PoiMenu; }
                        KeyCode::Char('/') => { input_cur = 0; focus = Focus::NearSearch(String::new()); }
                        KeyCode::Char('n') => { input_cur = 0; focus = Focus::PoiKindForm { label: String::new(), tag: String::new(), field: 0 }; } // 新規カテゴリ追加
                        KeyCode::Char('[') if poimenu_sel > 0 && poimenu_sel < poi_kinds.len() => {
                            poi_kinds.swap(poimenu_sel, poimenu_sel - 1); poimenu_sel -= 1;
                            let _ = save_poi_kinds(&poi_kinds);
                            focus = Focus::PoiMenu;
                        }
                        KeyCode::Char(']') if poimenu_sel + 1 < poi_kinds.len() => {
                            poi_kinds.swap(poimenu_sel, poimenu_sel + 1); poimenu_sel += 1;
                            let _ = save_poi_kinds(&poi_kinds);
                            focus = Focus::PoiMenu;
                        }
                        KeyCode::Char('x') if poimenu_sel < poi_kinds.len() => {
                            let removed = poi_kinds.remove(poimenu_sel);
                            if poimenu_sel >= poi_kinds.len() && poimenu_sel > 0 { poimenu_sel -= 1; }
                            let _ = save_poi_kinds(&poi_kinds);
                            addr = format!("カテゴリ削除: {}", removed.label);
                            focus = Focus::PoiMenu;
                        }
                        KeyCode::Enter | KeyCode::Char(_) => {
                            // Enter=選択行 / キー1文字=対応カテゴリ。最終行(=poi_kinds.len())はキーワード周辺検索。
                            let idx = if let KeyCode::Char(c) = k.code { poi_kinds.iter().position(|kk| kk.key == c) } else { Some(poimenu_sel) };
                            match idx {
                                Some(i) if i >= poi_kinds.len() => { input_cur = 0; focus = Focus::NearSearch(String::new()); }
                                Some(i) => {
                                    let kind = poi_kinds[i].clone();
                                    let label = kind.label.clone();
                                    let (tx, rx) = std::sync::mpsc::channel();
                                    std::thread::spawn(move || {
                                        let r = poi_search(&kind, cx, cy, z, ow, oh, lat, lon);
                                        let _ = tx.send((label, r));
                                    });
                                    catpoi_job = Some(rx);
                                    focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                                }
                                None => focus = Focus::PoiMenu,
                            }
                        }
                        _ => focus = Focus::PoiMenu,
                    },
                    Focus::PoiList => match k.code {
                        KeyCode::Up | KeyCode::Char('w') => { snd.play("click"); poi_sel = poi_sel.saturating_sub(1); if let Some(p) = pois.get(poi_sel) { let (nx, ny) = deg_to_pixel(p.0, p.1, z); cx = nx; cy = ny; } focus = Focus::PoiList; } // 選択に地図追従
                        KeyCode::Down | KeyCode::Char('s') => { snd.play("click"); if poi_sel + 1 < pois.len() { poi_sel += 1; } if let Some(p) = pois.get(poi_sel) { let (nx, ny) = deg_to_pixel(p.0, p.1, z); cx = nx; cy = ny; } focus = Focus::PoiList; }
                        KeyCode::Left | KeyCode::Char('a') => { cx -= (oh as f64 / 8.0).max(1.0); focus = Focus::PoiList; } // ←→/hjklで地図を微パン(一覧選択は動かさない)
                        KeyCode::Right | KeyCode::Char('d') => { cx += (oh as f64 / 8.0).max(1.0); focus = Focus::PoiList; }
                        KeyCode::Char('h') => { cx -= (oh as f64 / 8.0).max(1.0); focus = Focus::PoiList; }
                        KeyCode::Char('l') => { cx += (oh as f64 / 8.0).max(1.0); focus = Focus::PoiList; }
                        KeyCode::Char('k') => { cy -= (oh as f64 / 8.0).max(1.0); focus = Focus::PoiList; }
                        KeyCode::Char('j') => { cy += (oh as f64 / 8.0).max(1.0); focus = Focus::PoiList; }
                        KeyCode::Char('+') | KeyCode::Char('=') => { if z < 19 { z += 1; cx *= 2.0; cy *= 2.0; restart_prefetch_on_zoom!(); } focus = Focus::PoiList; } // +/-でズーム
                        KeyCode::Char('-') | KeyCode::Char('_') => { if z > 2 { z -= 1; cx /= 2.0; cy /= 2.0; restart_prefetch_on_zoom!(); } focus = Focus::PoiList; }
                        KeyCode::Enter => { // 選択地点へ移動(明示)
                            if let Some(p) = pois.get(poi_sel) { let (nx, ny) = deg_to_pixel(p.0, p.1, z); cx = nx; cy = ny; }
                            focus = Focus::PoiList;
                        }
                        KeyCode::Char('v') => { // 選択地点をルートに追加(末尾)
                            if let Some(p) = pois.get(poi_sel) {
                                snd.play("pop");
                                wp_add(&mut wps, (p.0, p.1));
                                let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_;
                                addr = format!("地点を追加 #{}", wps.len());
                            }
                            focus = Focus::PoiList;
                        }
                        KeyCode::Char('f') => focus = Focus::PoiMenu,
                        KeyCode::Char('P') => { // 選択結果をお気に入りスポットに登録(カテゴリを選ばせる)
                            if let Some(p) = pois.get(poi_sel) {
                                if spot_cats.is_empty() { let _ = ensure_spot_cat("お気に入り", &mut spot_cats); }
                                pending_spot = Some((p.0, p.1, p.2.clone()));
                                cat_sel = 0;
                                focus = Focus::SpotCatList;
                            } else { focus = Focus::PoiList; }
                        }
                        KeyCode::Esc => { pois.clear(); set_markers(&mut spec, &wps, &pois); }
                        _ => focus = Focus::PoiList,
                    },
                    Focus::SaveName(mut buf) => match k.code {
                        KeyCode::Enter => {
                            let name = buf.trim().to_string();
                            if !name.is_empty() {
                                if list_named_routes().contains(&name) {
                                    save_confirm = Some(name);
                                    focus = Focus::SaveName(buf); // 上書き確認中も編集状態を保持(取消時はそのまま名前を変えられる)
                                } else {
                                    addr = match save_named_route(&name, &mode, &wps) { Ok(_) => { snd.play("confirm"); route_name_hint = name.clone(); format!("保存: {name}") }, Err(e) => format!("({e})") };
                                }
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::SaveName(buf); }
                    },
                    Focus::RouteFavMenu { sel } => match k.code { // お気に入りルート: 保存/呼び出しの小メニュー(Sキー)
                        KeyCode::Up | KeyCode::Char('w') => { focus = Focus::RouteFavMenu { sel: sel.saturating_sub(1) }; }
                        KeyCode::Down | KeyCode::Char('s') => { focus = Focus::RouteFavMenu { sel: (sel + 1).min(1) }; }
                        KeyCode::Enter => {
                            if sel == 0 { input_cur = route_name_hint.chars().count(); focus = Focus::SaveName(route_name_hint.clone()); }
                            else {
                                route_names = list_named_routes(); rn_sel = 0;
                                if route_names.is_empty() { addr = "お気に入り無し".into(); focus = Focus::Map; }
                                else { focus = Focus::RouteList; }
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); focus = Focus::Map; }
                        _ => focus = Focus::RouteFavMenu { sel },
                    },
                    Focus::RouteList => match k.code {
                        KeyCode::Up | KeyCode::Char('w') => { snd.play("click"); rn_sel = rn_sel.saturating_sub(1); focus = Focus::RouteList; }
                        KeyCode::Down | KeyCode::Char('s') => { snd.play("click"); if rn_sel + 1 < route_names.len() { rn_sel += 1; } focus = Focus::RouteList; }
                        KeyCode::Enter => {
                            if let Some(name) = route_names.get(rn_sel) {
                                if let Some((w, m)) = load_named_route(name) {
                                    let (nx, ny) = deg_to_pixel(w[0].0, w[0].1, z); cx = nx; cy = ny;
                                    wps = w; mode = m; wp_sel = 0;
                                    route_name_hint = name.clone(); // 保存時にこの名前をそのまま提示する
                                    { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; }
                                }
                            }
                        }
                        KeyCode::Esc => {}
                        _ => focus = Focus::RouteList,
                    },
                    Focus::RoadList => match k.code { // 道路の塊の一覧(個別削除)
                        KeyCode::Up | KeyCode::Char('w') => { snd.play("click"); road_sel = road_sel.saturating_sub(1); focus = Focus::RoadList; }
                        KeyCode::Down | KeyCode::Char('s') => { snd.play("click"); if road_sel + 1 < road_segs.len() { road_sel += 1; } focus = Focus::RoadList; }
                        KeyCode::Char('x') => { // 選択した道路の塊を削除
                            if road_sel < road_segs.len() {
                                road_segs.remove(road_sel);
                                if road_sel >= road_segs.len() && road_sel > 0 { road_sel -= 1; }
                                sync_roads!();
                            }
                            if road_segs.is_empty() { // 空になったら閉じる。左袖の残像を残さないよう全消去する
                                addr = "道路を全削除".into();
                                focus = Focus::Map;
                                let _ = write!(out, "\x1b[2J");
                                force_reemit = true;
                            } else { focus = Focus::RoadList; }
                        }
                        // 閉じる → Map。左袖(道路一覧)の残像を残さないよう全消去する(Menu閉じる時と同じ理由)。
                        KeyCode::Esc => { snd.play("back"); focus = Focus::Map; let _ = write!(out, "\x1b[2J"); force_reemit = true; }
                        _ => focus = Focus::RoadList,
                    },
                    // 並べ替えビュー: ↑↓で選択(地図が追従)、Spaceで掴む↔置く、掴み中は↑↓で地点を移動
                    Focus::WaypointList => match k.code {
                        KeyCode::Up | KeyCode::BackTab | KeyCode::Char('w') => {
                            if !wps.is_empty() {
                                if grab && wp_sel > 0 { wps.swap(wp_sel, wp_sel - 1); wp_sel -= 1; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; }
                                else { wp_sel = (wp_sel + wps.len() - 1) % wps.len(); }
                                if let Some(&(la, lo)) = wps.get(wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; }
                            }
                            focus = Focus::WaypointList;
                        }
                        KeyCode::Down | KeyCode::Tab | KeyCode::Char('s') => {
                            if !wps.is_empty() {
                                if grab && wp_sel + 1 < wps.len() { wps.swap(wp_sel, wp_sel + 1); wp_sel += 1; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; }
                                else { wp_sel = (wp_sel + 1) % wps.len(); }
                                if let Some(&(la, lo)) = wps.get(wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; }
                            }
                            focus = Focus::WaypointList;
                        }
                        KeyCode::Char(' ') => { if !wps.is_empty() { grab = !grab; snd.play(if grab { "blip" } else { "pop" }); } focus = Focus::WaypointList; }
                        KeyCode::Char('+') | KeyCode::Char('=') => { if z < 19 { z += 1; cx *= 2.0; cy *= 2.0; restart_prefetch_on_zoom!(); } focus = Focus::WaypointList; }
                        KeyCode::Char('-') | KeyCode::Char('_') => { if z > 2 { z -= 1; cx /= 2.0; cy /= 2.0; restart_prefetch_on_zoom!(); } focus = Focus::WaypointList; }
                        KeyCode::Char('[') => { if wp_sel > 0 && wp_sel < wps.len() { wps.swap(wp_sel, wp_sel - 1); wp_sel -= 1; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; if let Some(&(la, lo)) = wps.get(wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; } } focus = Focus::WaypointList; }
                        KeyCode::Char(']') => { if wp_sel + 1 < wps.len() { wps.swap(wp_sel, wp_sel + 1); wp_sel += 1; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; if let Some(&(la, lo)) = wps.get(wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; } } focus = Focus::WaypointList; }
                        KeyCode::Char('x') => {
                            if !wps.is_empty() { let i = wp_sel.min(wps.len() - 1); wps.remove(i); if wp_sel >= wps.len() && wp_sel > 0 { wp_sel -= 1; } let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; }
                            grab = false;
                            if !wps.is_empty() { if let Some(&(la, lo)) = wps.get(wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; } focus = Focus::WaypointList; } // 空になったら閉じる
                        }
                        KeyCode::Char('v') => { // 中心に地点を追加し、追加した点を選択(リストは wps から即再生成される)
                            snd.play("pop");
                            wp_add(&mut wps, (lat, lon));
                            wp_sel = wps.len().saturating_sub(1);
                            grab = false;
                            let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_;
                            addr = format!("地点を追加 #{}", wps.len());
                            focus = Focus::WaypointList;
                        }
                        // 閉じる → Map。左袖(経由地一覧)の残像を残さないよう全消去する(Menu閉じる時と同じ理由)。
                        KeyCode::Esc | KeyCode::Enter => { grab = false; focus = Focus::Map; let _ = write!(out, "\x1b[2J"); force_reemit = true; }
                        _ => focus = Focus::WaypointList,
                    },
                    // Space メニュー・トップ(カテゴリ選択)。文字キーは全カテゴリ横断で直接実行できる。
                    Focus::Menu(MenuLevel::Categories) => match k.code {
                        KeyCode::Up | KeyCode::Char('w') => { snd.play("click"); menu_cat_sel = menu_cat_sel.saturating_sub(1); focus = Focus::Menu(MenuLevel::Categories); }
                        KeyCode::Down | KeyCode::Char('s') => { snd.play("click"); if menu_cat_sel + 1 < MENU_CATEGORIES.len() { menu_cat_sel += 1; } focus = Focus::Menu(MenuLevel::Categories); }
                        KeyCode::Enter => { snd.play("click"); menu_item_sel = 0; focus = Focus::Menu(MenuLevel::Items(menu_cat_sel)); }
                        // メニューを閉じる → Map。左袖(カテゴリ一覧)はマップとは別の列に描かれており、
                        // 通常のマップ再描画では上書きされない列が残ることがあるため、全消去してから
                        // 次フレームで確実に再構築させる(Resize時の扱いと同じ)。
                        KeyCode::Esc => { snd.play("back"); focus = Focus::Map; let _ = write!(out, "\x1b[2J"); force_reemit = true; }
                        KeyCode::Char(c) => match menu_action_for_key(c) {
                            Some(act) => run_action!(act, lat, lon, cols, tr, &route_nogos),
                            None => focus = Focus::Menu(MenuLevel::Categories),
                        },
                        _ => focus = Focus::Menu(MenuLevel::Categories),
                    },
                    // Space メニュー・展開(項目選択)。キーはそのカテゴリ内だけ有効(スコープ限定)。
                    Focus::Menu(MenuLevel::Items(ci)) => {
                        let items = MENU_CATEGORIES[ci].items;
                        match k.code {
                            KeyCode::Up | KeyCode::Char('w') if !items.iter().any(|it| it.key == 'w') => { snd.play("click"); menu_item_sel = menu_item_sel.saturating_sub(1); focus = Focus::Menu(MenuLevel::Items(ci)); }
                            KeyCode::Down | KeyCode::Char('s') if !items.iter().any(|it| it.key == 's') => { snd.play("click"); if menu_item_sel + 1 < items.len() { menu_item_sel += 1; } focus = Focus::Menu(MenuLevel::Items(ci)); }
                            KeyCode::Enter => run_action!(items[menu_item_sel].action, lat, lon, cols, tr, &route_nogos),
                            KeyCode::Esc => { snd.play("back"); focus = Focus::Menu(MenuLevel::Categories); } // 上位カテゴリへ戻る
                            KeyCode::Char(c) => match items.iter().find(|it| it.key == c) {
                                Some(it) => run_action!(it.action, lat, lon, cols, tr, &route_nogos),
                                None => focus = Focus::Menu(MenuLevel::Items(ci)),
                            },
                            _ => focus = Focus::Menu(MenuLevel::Items(ci)),
                        }
                    }
                    // 色ピッカー: ←→でパレット選択、Enterで確定
                    Focus::ColorPick { cat } => {
                        let n = SPOT_PALETTE.len() as u8;
                        match k.code {
                            KeyCode::Left => { color_sel = (color_sel + n - 1) % n; focus = Focus::ColorPick { cat }; }
                            KeyCode::Right => { color_sel = (color_sel + 1) % n; focus = Focus::ColorPick { cat }; }
                            KeyCode::Enter => {
                                if let Some(e) = spot_cats.get_mut(cat) { e.1 = color_sel; let _ = save_all_cats(&spot_cats); apply_spots(&mut spec, &spots, &spot_cats, show_spots); }
                                focus = Focus::SpotCatList;
                            }
                            KeyCode::Esc => { snd.play("back"); focus = Focus::SpotCatList; }
                            _ => focus = Focus::ColorPick { cat },
                        }
                    }
                    Focus::ShapePick { cat } => { // 形状ピッカー(色とは独立に形を選ぶ)
                        let n = NUM_MARKER_SHAPES;
                        match k.code {
                            KeyCode::Left => { shape_sel = (shape_sel + n - 1) % n; focus = Focus::ShapePick { cat }; }
                            KeyCode::Right => { shape_sel = (shape_sel + 1) % n; focus = Focus::ShapePick { cat }; }
                            KeyCode::Enter => {
                                if let Some(e) = spot_cats.get_mut(cat) { e.2 = shape_sel; let _ = save_all_cats(&spot_cats); apply_spots(&mut spec, &spots, &spot_cats, show_spots); }
                                focus = Focus::SpotCatList;
                            }
                            KeyCode::Esc => { snd.play("back"); focus = Focus::SpotCatList; }
                            _ => focus = Focus::ShapePick { cat },
                        }
                    }
                    // 設定画面の一覧ピッカー: 地図種別/既定ルート/AIモデル/画像解像度/中心十字の色を↑↓/w・sで選びEnterで確定
                    Focus::SettingsPick(idx) => {
                        let n = settings::pick_labels(idx, &cfg).len().max(1);
                        match k.code {
                            KeyCode::Up | KeyCode::Char('w') => { set_pick_sel = (set_pick_sel + n - 1) % n; focus = Focus::SettingsPick(idx); }
                            KeyCode::Down | KeyCode::Char('s') => { set_pick_sel = (set_pick_sel + 1) % n; focus = Focus::SettingsPick(idx); }
                            // 読み上げの声(27)だけ: Spaceでカーソル位置の声を試聴(確定せず一覧も閉じない)。
                            KeyCode::Char(' ') if idx == 27 => {
                                if let Some((v, _)) = settings::voice_choices(&cfg).get(set_pick_sel) {
                                    voice_preview_job = Some(voice::preview_voice(v, "300メートル先、左折です"));
                                    let name = if v.is_empty() { "システム既定".to_string() } else { voice::display_voice_name(v).to_string() };
                                    addr = format!("試聴: {name}(この端末で再生)");
                                }
                                focus = Focus::SettingsPick(idx);
                            }
                            KeyCode::Enter => {
                                let eff = settings::apply_pick(idx, set_pick_sel, &mut cfg, &mut opts.style);
                                // スタイル変更時、キャッシュ自体はもう消さない(TileKeyがstyleを含むため
                                // 別styleと混ざる心配は無く、むしろ残しておくことで切替直後に旧styleを
                                // フォールバック仮表示できる)。ローダーの未着手依頼だけ捨てる(旧styleの
                                // 取得依頼が溜まり続けないように)。
                                if eff.cache_clear { loader.clear_pending(); }
                                if eff.force_reemit { force_reemit = true; }
                                let _ = config::save_config(&cfg);
                                // 読み上げの声(27)は確定した声が実際に使えるか、確定時にも1回再生して確かめる。
                                if idx == 27 {
                                    voice_preview_job = Some(voice::preview_voice(&cfg.voice_name, "300メートル先、左折です"));
                                    let name = if cfg.voice_name.is_empty() { "システム既定".to_string() } else { voice::display_voice_name(&cfg.voice_name).to_string() };
                                    addr = format!("試聴: {name}(この端末で再生)");
                                }
                                focus = Focus::Settings;
                            }
                            KeyCode::Esc => { snd.play("back"); focus = Focus::Settings; } // 変更せず閉じる
                            _ => focus = Focus::SettingsPick(idx),
                        }
                    }
                    // ルート一覧にフォーカス中: ↑↓で点/操作行を選択、Enterで実行。矢印はパンでなく選択。
                    Focus::RoutePanel => {
                        match k.code {
                            KeyCode::Up | KeyCode::Char('w') => {
                                route_sel = route_sel.saturating_sub(1);
                                if route_sel < wps.len() { wp_sel = route_sel; let (la, lo) = wps[wp_sel]; let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; }
                                focus = Focus::RoutePanel;
                            }
                            KeyCode::Down | KeyCode::Char('s') => {
                                let total = wps.len() + ROUTE_ACTS.len();
                                if route_sel + 1 < total { route_sel += 1; }
                                if route_sel < wps.len() { wp_sel = route_sel; let (la, lo) = wps[wp_sel]; let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; }
                                focus = Focus::RoutePanel;
                            }
                            KeyCode::Enter | KeyCode::Char(' ') => {
                                if route_sel >= wps.len() { // 操作行を実行(run_action側でfocus遷移する場合あり=その時はそちら優先)
                                    let ai = route_sel - wps.len();
                                    if ai < ROUTE_ACTS.len() { let act = ROUTE_ACTS[ai].1; run_action!(act, lat, lon, cols, tr, &route_nogos); }
                                } else { // 点を選択中: 地図を寄せてパネルに留まる
                                    let (la, lo) = wps[route_sel]; let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny;
                                    focus = Focus::RoutePanel;
                                }
                            }
                            KeyCode::Char('[') => { if route_sel < wps.len() && route_sel > 0 { wps.swap(route_sel, route_sel - 1); route_sel -= 1; wp_sel = route_sel; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; } focus = Focus::RoutePanel; }
                            KeyCode::Char(']') => { if route_sel + 1 < wps.len() { wps.swap(route_sel, route_sel + 1); route_sel += 1; wp_sel = route_sel; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; } focus = Focus::RoutePanel; }
                            KeyCode::Char('x') => {
                                if route_sel < wps.len() { wps.remove(route_sel); if route_sel >= wps.len() && route_sel > 0 { route_sel -= 1; } wp_sel = route_sel.min(wps.len().saturating_sub(1)); let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; }
                                if !wps.is_empty() { focus = Focus::RoutePanel; }
                                else { // 空になったら地図へ。左袖の残像を残さないよう全消去する
                                    focus = Focus::Map;
                                    let _ = write!(out, "\x1b[2J");
                                    force_reemit = true;
                                }
                            }
                            KeyCode::Char('v') => { snd.play("pop"); wp_add(&mut wps, (lat, lon)); let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; addr = format!("地点を追加 #{}", wps.len()); focus = Focus::RoutePanel; }
                            KeyCode::Char('+') | KeyCode::Char('=') => { if z < 19 { z += 1; cx *= 2.0; cy *= 2.0; restart_prefetch_on_zoom!(); } focus = Focus::RoutePanel; }
                            KeyCode::Char('-') | KeyCode::Char('_') => { if z > 2 { z -= 1; cx /= 2.0; cy /= 2.0; restart_prefetch_on_zoom!(); } focus = Focus::RoutePanel; }
                            // 地図へ戻る。左袖(ルート一覧)の残像を残さないよう全消去する(Menu閉じる時と同じ理由)。
                            KeyCode::Esc | KeyCode::Tab => { snd.play("back"); focus = Focus::Map; let _ = write!(out, "\x1b[2J"); force_reemit = true; }
                            _ => { focus = Focus::RoutePanel; }
                        }
                    }
                    Focus::Map => {
                        // Shift+矢印/大文字HJKL=常に高速(固定)。無印(矢印/小文字hjkl)=既定は細かい1歩で、
                        // 同方向を短間隔(220ms以内)で押し続ける/連打するほど徐々に加速し、上限は高速の
                        // 手前まで。方向転換や間隔が空くと streak がリセットされ、また細かい1歩に戻る。
                        // hjklは矢印と全く同じ挙動モデル(大文字/小文字がShiftの有無に対応)。大文字は
                        // 修飾キーの拡張シーケンスに依存しない普通の文字なので、端末がShift+矢印の拡張
                        // CSIを送れない場合(iSH等)でも常時高速パンが確実に効く。
                        let is_pan = matches!(k.code, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                            | KeyCode::Char('h') | KeyCode::Char('j') | KeyCode::Char('k') | KeyCode::Char('l')
                            | KeyCode::Char('H') | KeyCode::Char('J') | KeyCode::Char('K') | KeyCode::Char('L'));
                        if is_pan {
                            if last_pan_dir == Some(k.code) && last_pan_at.elapsed() < std::time::Duration::from_millis(220) {
                                pan_streak = (pan_streak + 1).min(20);
                            } else {
                                pan_streak = 0;
                            }
                            last_pan_dir = Some(k.code);
                            last_pan_at = std::time::Instant::now();
                        }
                        let fine = oh as f64 / 64.0;
                        let fast = oh as f64 / 4.0;
                        let is_fast_key = k.modifiers.contains(KeyModifiers::SHIFT)
                            || matches!(k.code, KeyCode::Char('H') | KeyCode::Char('J') | KeyCode::Char('K') | KeyCode::Char('L'));
                        let step = if is_fast_key {
                            fast
                        } else {
                            (fine * (1.0 + pan_streak as f64 * 0.35)).min(fast)
                        }.max(1.0);
                        let mut quit = false;
                        match k.code {
                            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('H') => { cx -= step; addr.clear(); }
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('L') => { cx += step; addr.clear(); }
                            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => { cy -= step; addr.clear(); }
                            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => { cy += step; addr.clear(); }
                            KeyCode::Char('+') | KeyCode::Char('=') => if z < 19 { z += 1; cx *= 2.0; cy *= 2.0; addr.clear(); restart_prefetch_on_zoom!(); },
                            KeyCode::Char('-') | KeyCode::Char('_') => if z > 2 { z -= 1; cx /= 2.0; cy /= 2.0; addr.clear(); restart_prefetch_on_zoom!(); },
                            KeyCode::Enter if !wps.is_empty() && route_sel >= wps.len() && route_sel < wps.len() + ROUTE_ACTS.len() => {
                                // w/sで操作行(保存/GPX等)を選択中はEnterでその操作を実行
                                let ai = route_sel - wps.len();
                                let act = ROUTE_ACTS[ai].1;
                                run_action!(act, lat, lon, cols, tr, &route_nogos);
                            }
                            KeyCode::Enter => { // 中心付近の最寄りお気に入りにスナップ＋名前表示
                                let mut best: Option<(f64, usize)> = None;
                                for (i, s) in spots.iter().enumerate() {
                                    let (gx, gy) = deg_to_pixel(s.lat, s.lon, z);
                                    let dpx = ((gx - cx).powi(2) + (gy - cy).powi(2)).sqrt();
                                    if best.map_or(true, |(bd, _)| dpx < bd) { best = Some((dpx, i)); }
                                }
                                match best {
                                    Some((dpx, i)) if dpx <= (ow.min(oh) as f64) * 0.25 => {
                                        let s = &spots[i];
                                        let (nx, ny) = deg_to_pixel(s.lat, s.lon, z); cx = nx; cy = ny;
                                        popup = Some(if s.name.is_empty() { "★ (無名スポット)".into() } else { format!("★ {} [{}]", s.name, s.cat) });
                                    }
                                    Some(_) => addr = "近くにお気に入り無し".into(),
                                    None => addr = "お気に入り未登録".into(),
                                }
                            }
                            KeyCode::Char('a') => addr = reverse_geocode(lat, lon).unwrap_or_else(|e| format!("({e})")),
                            KeyCode::Char('/') => { input_cur = 0; focus = Focus::Search(String::new()); }
                            KeyCode::Char('f') => focus = Focus::PoiMenu,
                            KeyCode::Char('S') => { focus = Focus::RouteFavMenu { sel: 0 }; } // お気に入りルート: 保存/呼び出しの小メニュー
                            KeyCode::Char('v') => { // 地図中心に地点を追加(末尾)。役割は並び順で自動(先頭=始点/末尾=終点)
                                snd.play("pop"); wp_add(&mut wps, (lat, lon));
                                wp_sel = wps.len() - 1; route_sel = wp_sel; // 追加した点を選択状態にする(左袖のハイライトが追従)
                                let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_;
                                addr = format!("地点を追加 #{}", wps.len());
                            }
                            // w/s: Tabで一覧へ入らなくても、地図(パン)はそのまま左袖(ルート点+操作行)の
                            // 選択だけ上下できる。操作行(保存/GPX等)まで選べて、Enterでそのまま実行できる
                            KeyCode::Char('w') if !wps.is_empty() => {
                                let total = wps.len() + ROUTE_ACTS.len();
                                route_sel = (route_sel + total - 1) % total;
                                if route_sel < wps.len() {
                                    wp_sel = route_sel;
                                    let (la, lo) = wps[wp_sel]; let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny;
                                }
                            }
                            KeyCode::Char('s') if !wps.is_empty() => {
                                let total = wps.len() + ROUTE_ACTS.len();
                                route_sel = (route_sel + 1) % total;
                                if route_sel < wps.len() {
                                    wp_sel = route_sel;
                                    let (la, lo) = wps[wp_sel]; let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny;
                                }
                            }
                            KeyCode::Tab | KeyCode::BackTab => { if !wps.is_empty() { route_sel = route_sel.min(wps.len() + ROUTE_ACTS.len() - 1); focus = Focus::RoutePanel; } } // 左のルート一覧にフォーカス(そこで↑↓選択・Enter実行)
                            KeyCode::Char(' ') => { snd.play("click"); menu_cat_sel = 0; focus = Focus::Menu(MenuLevel::Categories); } // Space=メニュー(カテゴリ→展開の2階層)
                            KeyCode::Char('?') => { help = true; help_page = 0; }
                            KeyCode::Char('P') => { cat_sel = 0; focus = Focus::SpotCatList; } // マイスポット(カテゴリ一覧)
                            KeyCode::Char(',') => { set_sel = 0; focus = Focus::Settings; voice::warm_voice_list(); } // 設定画面
                            KeyCode::Char('r') => { input_cur = 0; focus = Focus::RoadSearch(String::new()); } // 道路名でルート(現在view内)
                            KeyCode::Char('@') => { // おすすめツーリングスポット提案(claude -p)
                                if !cfg.llm_recommend_enabled { snd.play("error"); addr = "おすすめ: 設定でOFF(,でON)".into(); }
                                else if !recommend::claude_available(&cfg.llm_command) { snd.play("error"); addr = "おすすめ: claudeが無い(設定のLLM/コマンド確認)".into(); }
                                else { input_cur = 0; focus = Focus::Recommend(String::new()); }
                            }
                            KeyCode::Char('V') => { show_spots = !show_spots; apply_spots(&mut spec, &spots, &spot_cats, show_spots); addr = if show_spots { "マイスポット表示".into() } else { "マイスポット非表示".into() }; }
                            // ルート一覧(左袖)の表示切替。ルート自体(wps)は消さない。狙いは
                            // 画面が狭い端末で「ルートがある間ずっと出っぱなし」を隠せるようにすること。
                            // 左袖はマップ本体の再描画では上書きされない列に描かれているため、隠す方向の
                            // 切替では全消去してから次フレームで再構築させる(Menu閉じる時と同じ理由)。
                            KeyCode::Char('R') => {
                                route_panel_hidden = !route_panel_hidden;
                                addr = if route_panel_hidden { "ルート一覧: 非表示".into() } else { "ルート一覧: 表示".into() };
                                if route_panel_hidden { let _ = write!(out, "\x1b[2J"); }
                                force_reemit = true;
                            }
                            KeyCode::Char('E') => { // 標高プロファイルの表示/非表示
                                show_elev = !show_elev;
                                if show_elev && (spec.routes.is_empty() || !route_ele.iter().any(|&z| z != 0.0)) { addr = "標高: ルート確定後に表示".into(); }
                            }
                            KeyCode::Char('C') => { radar_toggle!(); } // 雨雲レーダー(気象庁ナウキャスト)の表示/非表示。Spaceメニュー・設定画面と共通処理
                            // 500mメッシュ人口(国土数値情報)の表示/非表示。Pはマイスポット・Cは雨雲で
                            // 埋まっているため、空いている U を割り当てている。
                            KeyCode::Char('U') => { population_toggle!(); }
                            // 過去災害の塗り(コロプレス)の表示/非表示。Bは詳細パネル・Uは人口メッシュで
                            // 埋まっているため、空いている F(Fill)を割り当てている。
                            KeyCode::Char('F') => { disaster_fill_toggle!(); }
                            KeyCode::Char('>') => { // 表示時刻を未来へ1コマ(OFFなら発見しやすさのためONにする)
                                if !radar_on {
                                    radar_turn_on!();
                                } else if !radar_tl.is_empty() {
                                    radar_idx = (radar_idx + 1).min(radar_tl.frames.len() - 1); // 折り返さない
                                    // 「現在」ちょうどに戻ったら追従モードへ復帰、それより未来なら外れる。
                                    if radar_idx == radar_tl.now_idx { radar_follow = true; }
                                    else if radar_idx > radar_tl.now_idx { radar_follow = false; }
                                    addr = format!("雨雲 {}", radar::frame_label(&radar_tl, radar_idx));
                                }
                            }
                            KeyCode::Char('<') => { // 表示時刻を過去へ1コマ(OFFのときは何もしない=誤爆で勝手にONにしない)
                                if radar_on && !radar_tl.is_empty() {
                                    radar_idx = radar_idx.saturating_sub(1);
                                    radar_follow = false;
                                    addr = format!("雨雲 {}", radar::frame_label(&radar_tl, radar_idx));
                                }
                            }
                            KeyCode::Char('A') => run_action!(MenuAction::PlayRoute, lat, lon, cols, tr, &route_nogos),
                            KeyCode::Char('G') => { // ライブ現在地(ブレッドクラム)の ON/OFF
                                if gps_rx.is_some() { gps_rx = None; addr = "ライブ現在地: OFF".into(); }
                                else {
                                    let bin = if std::path::Path::new("/opt/homebrew/bin/CoreLocationCLI").exists() { "/opt/homebrew/bin/CoreLocationCLI" } else { "CoreLocationCLI" };
                                    if gpslive::available(bin) { gps_rx = Some(gpslive::start_poller(bin.to_string(), 5)); gps_trail.clear(); gps_pos = None; addr = "ライブ現在地: ON(5秒ごと)".into(); }
                                    else { addr = "ライブ: CoreLocationCLI無し(brew install corelocationcli)".into(); }
                                }
                            }
                            KeyCode::Char('i') => { // 実写(Street View)を中心地点で開く
                                if !cfg.streetview_enabled { snd.play("error"); addr = "実写: OFF(設定で有効化)".into(); }
                                else if !streetview::available(&cfg.google_maps_api_key) { snd.play("error"); addr = "実写: Google APIキー未設定([google] maps_api_key)".into(); }
                                else {
                                    // 実写取得を別スレッドへ(focusはMapのまま=スピナーが回る)
                                    sv_fov = 90.0; // 開き直しなので既定ズームに戻す
                                    let (la, lo) = (lat, lon);
                                    let key = cfg.google_maps_api_key.clone();
                                    let (tx, rx) = std::sync::mpsc::channel();
                                    std::thread::spawn(move || {
                                        let r = streetview::fetch(la, lo, 0, 640, 480, 90.0, &key);
                                        let _ = tx.send((la, lo, 0, r));
                                    });
                                    street_job = Some(rx);
                                }
                            }
                            KeyCode::Char('I') => { // 実画像モード(iTerm2インライン画像)の ON/OFF
                                cfg.image_mode = !cfg.image_mode;
                                force_reemit = true; // 切替直後は必ず描き直す
                                addr = if cfg.image_mode {
                                    if image_capable() { "実画像モード: ON".into() } else { "実画像モード: ON(この端末は非対応・AA継続)".into() }
                                } else { "実画像モード: OFF".into() };
                            }
                            // キー選定: C/K/L/V/P/I等の自然な字は全て他機能で使用済みのため空いている'N'を割当
                            KeyCode::Char('N') => run_action!(MenuAction::ViewCamera, lat, lon, cols, tr, &route_nogos),
                            // 過去災害: 中心に一番近い地点の事例一覧を中央パネルへ(防災のB)。
                            KeyCode::Char('B') => {
                                if !cfg.disaster_enabled { snd.play("error"); addr = "過去災害: OFF(設定で有効化)".into(); }
                                else {
                                    // 視野内で中心に一番近い地点。カメラのNと同じく、フレーム先頭で
                                    // 切り出した一覧の借用はここ(tick後)まで生きられないので層から直接引く。
                                    let nearest = disaster_layer.items(plotlayer::view_bbox(cx, cy, z)).into_iter()
                                        .min_by(|a, b| {
                                            let da = (a.lat - lat).powi(2) + (a.lon - lon).powi(2);
                                            let db = (b.lat - lat).powi(2) + (b.lon - lon).powi(2);
                                            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                                        })
                                        .cloned();
                                    match nearest {
                                        None => { snd.play("error"); addr = "過去災害: 周辺に記録無し".into(); }
                                        Some(s) => {
                                            // 事例本体(名称・日付・被害統計)は集計に入っていないので、
                                            // ここで初めて取りに行く。保存はしない(押したときだけ)。
                                            let since = plotlayer::disaster_since();
                                            let (tx, rx) = std::sync::mpsc::channel();
                                            std::thread::spawn(move || {
                                                let r = disaster::fetch_events(s.lat, s.lon, since, disaster::EVENT_LIMIT)
                                                    .map(|evs| disaster::panel_content(&evs, &s, since));
                                                let _ = tx.send(r);
                                            });
                                            disaster_job = Some(rx);
                                            addr = "🌊災害事例を取得中…".into();
                                        }
                                    }
                                }
                            }
                            // 通行規制の詳細(なぜ通れないか): 中心に一番近い区間の規制原因を中央パネルへ。
                            KeyCode::Char('T') => {
                                if !cfg.regulation_enabled { snd.play("error"); addr = "通行規制: OFF(設定で有効化)".into(); }
                                else {
                                    // B/Nと同じく、フレーム先頭で切り出した一覧の借用はここまで生きられないので層から直接引く。
                                    let nearest = regulation_layer.items(plotlayer::view_bbox(cx, cy, z)).into_iter()
                                        .filter(|ev| !ev.detail_id.is_empty())
                                        .min_by(|a, b| {
                                            let da = a.line.iter().map(|&p| (p.0 - lat).powi(2) + (p.1 - lon).powi(2)).fold(f64::INFINITY, f64::min);
                                            let db = b.line.iter().map(|&p| (p.0 - lat).powi(2) + (p.1 - lon).powi(2)).fold(f64::INFINITY, f64::min);
                                            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                                        });
                                    match nearest {
                                        None => { snd.play("error"); addr = "通行規制: 周辺に詳細あり区間無し".into(); }
                                        Some(ev) => {
                                            let id = ev.detail_id.clone();
                                            let (tx, rx) = std::sync::mpsc::channel();
                                            std::thread::spawn(move || { let _ = tx.send(regulation::fetch_detail(&id)); });
                                            regulation_detail_job = Some(rx);
                                            addr = "🚧規制詳細を取得中…".into();
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('n') => { // BRouter の代替ルート候補を巡回
                                if wps.len() >= 2 {
                                    route_alt = (route_alt + 1) % 4;
                                    let (nn, jj) = trigger_route(&mut spec, &wps, &pois, &mode, route_alt, &cfg.google_maps_api_key, &route_nogos);
                                    route_note = nn; route_job = jj;
                                } else { snd.play("error"); addr = "ルート未確定".into(); }
                            }
                            KeyCode::Char('W') => { focus = Focus::WanderForm { dist_km: a.dist.unwrap_or(40.0) }; } // 走りまくり: 距離ゲージを開く
                            KeyCode::Char('o') => { // スマホ共有(GoogleマップQR)
                                if wps.len() >= 2 {
                                    let (url, _) = gmaps_url(&wps);
                                    match qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L) {
                                        Ok(c) => qr_view = Some(build_qr_view(&c, &cfg.qr_style)),
                                        Err(_) => addr = "QR生成失敗".into(),
                                    }
                                } else { snd.play("error"); addr = "ルート未確定".into(); }
                            }
                            KeyCode::Char('x') => { wp_remove(&mut wps, &mut wp_sel); route_sel = wp_sel; { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; } }
                            KeyCode::Char('[') => { if play.is_some() { play_speed = (play_speed / 1.5).max(0.1); play_speed_bits.store(play_speed.to_bits(), std::sync::atomic::Ordering::Relaxed); addr = format!("再生速度 {:.2}x", play_speed); } else { wp_swap(&mut wps, &mut wp_sel, true); route_sel = wp_sel; { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; } } }
                            KeyCode::Char(']') => { if play.is_some() { play_speed = (play_speed * 1.5).min(32.0); play_speed_bits.store(play_speed.to_bits(), std::sync::atomic::Ordering::Relaxed); addr = format!("再生速度 {:.2}x", play_speed); } else { wp_swap(&mut wps, &mut wp_sel, false); route_sel = wp_sel; { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; } } }
                            KeyCode::Char('m') => { mode = match mode_label(&mode) { "下道" => "highway", "高速" => "short", _ => "surface" }.to_string(); { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0, &cfg.google_maps_api_key, &route_nogos); route_note = n_; route_job = j_; } }
                            KeyCode::Char('c') => run_action!(MenuAction::ClearRoute, lat, lon, cols, tr, &route_nogos),
                            KeyCode::Char('g') => match spec.routes.last() {
                                Some(rt) => addr = match write_gpx("termmap-route.gpx", &rt.pts) { Ok(_) => "GPX保存: termmap-route.gpx".into(), Err(e) => format!("({e})") },
                                None => { snd.play("error"); addr = "ルート未確定".into(); }
                            },
                            KeyCode::Char('q') => quit = true, // qは確認なしで即終了
                            KeyCode::Esc => { // Escを600ms以内に2回押すと終了確認を出す(誤爆防止)
                                if last_esc_at.map_or(false, |t| t.elapsed() < std::time::Duration::from_millis(600)) {
                                    quit_confirm = true;
                                    last_esc_at = None;
                                } else {
                                    last_esc_at = Some(std::time::Instant::now());
                                    addr = "もう一度Escで終了確認".into();
                                }
                            }
                            _ => {}
                        }
                        if quit { break; }
                        let n = (TILE as f64) * 2f64.powi(z as i32);
                        if cx < 0.0 { cx += n; } else if cx >= n { cx -= n; }
                        cy = cy.clamp(0.0, n - 1.0);
                    }
                }
            }
            // web/touch-overlay.js が window.term.paste() で送ってくる、ブラウザの
            // Geolocation APIによるライブ現在地。SOH(\u{1})区切りの専用マーカーにしているのは、
            // 普通に貼り付けられるURL/テキストと衝突しない制御文字だから。マーカーに一致しない
            // 通常のペーストは下の既存分岐(検索欄への入力等)へ素通しする。
            Some(Event::Paste(s)) if s.starts_with("\u{1}GPS_STOP\u{1}") => {
                web_gps_active = false;
                addr = "ライブ現在地(スマホ): OFF".into();
            }
            // スマホ側のGeolocation APIが失敗した時の理由(web/touch-overlay.js::gpsErrorReason)。
            // 「ボタンを押しても権限ダイアログすら出ない」といった切り分けをステータス行で
            // できるようにする診断用。成功時は既存のGPSマーカー分岐がaddrを上書きする。
            Some(Event::Paste(s)) if s.starts_with("\u{1}GPS_ERR\u{1}") => {
                let reason = &s["\u{1}GPS_ERR\u{1}".len()..];
                let msg = match reason {
                    "denied" => "権限拒否(Safariのサイト設定/位置情報サービスを確認)",
                    "unavailable" => "位置情報を取得できません",
                    "timeout" => "取得がタイムアウトしました",
                    "unsupported" => "この接続では使えません(HTTPS化を確認)",
                    _ => "原因不明のエラー",
                };
                addr = format!("ライブ現在地(スマホ): 失敗 - {msg}");
            }
            Some(Event::Paste(s)) if s.starts_with("\u{1}GPS\u{1}") => {
                let rest = &s["\u{1}GPS\u{1}".len()..];
                let mut parts = rest.splitn(2, '\u{1}');
                if let (Some(la_s), Some(lo_s)) = (parts.next(), parts.next()) {
                    if let (Ok(la), Ok(lo)) = (la_s.parse::<f64>(), lo_s.parse::<f64>()) {
                        if la.is_finite() && lo.is_finite() && (-90.0..=90.0).contains(&la) && (-180.0..=180.0).contains(&lo) {
                            if !web_gps_active { gps_trail.clear(); addr = "ライブ現在地(スマホ): ON".into(); }
                            web_gps_active = true;
                            gps_pos = Some((la, lo));
                            gps_trail.push((la, lo));
                            if gps_trail.len() > 300 { gps_trail.remove(0); }
                            maybe_speak_turn(&cfg, &spec, &turn_points, &mut voice_guide, (la, lo));
                        }
                    }
                }
            }
            // 軸モードの再送要求(#87 設計書 §5.3)。ブラウザを再読み込みするとJS側の状態は
            // 消えるが termmap 側の Focus は変わらないので通知が飛ばない。ここでは印を立てる
            // だけで、実際の送出は次フレーム末の1か所に任せる。
            Some(Event::Paste(s)) if s.starts_with(dragmode::DRAG_MODE_REQUEST) => {
                drag_mode_req_pending = true;
            }
            // ブラウザが実測したセル寸法(設計書 §7.2 の経路2)。ttyd は pty の ws_xpixel/ws_ypixel
            // を埋めないため、web版ではここが唯一のセル比の入手経路になる。壊れた値・非現実的な
            // 比は parse 側が捨て、その場合は既定値 2.0 のまま(=修正前と同じ)動く。
            Some(Event::Paste(s)) if s.starts_with(cellratio::CELL_MARKER) => {
                if let Some(r) = cellratio::parse_cell_marker(&s) {
                    if cell_ratio_web != Some(r) {
                        cell_ratio_web = Some(r);
                        // 比が変わった=いま出ている画像の形が古い。次フレームで1枚描き直す。
                        force_reemit = true;
                        last_map_sig = None; // sig一致による再構築スキップに巻き込まれないように
                    }
                }
            }
            // パン量マーカーは上の合算ブロックで消費済みなので、通常ここへは来ない。念のための
            // 保険(合算ブロックを通らない経路が将来増えても、マーカーが検索欄へ文字として
            // 入らないようにする)。
            Some(Event::Paste(s)) if s.starts_with(dragmode::PAN_MARKER) => {}
            Some(Event::Paste(s)) => { match &mut focus {
                Focus::Search(buf) | Focus::SaveName(buf) | Focus::NearSearch(buf) | Focus::NewCat(buf) | Focus::RoadSearch(buf) | Focus::Recommend(buf) => insert_str_at(buf, &mut input_cur, &s),
                Focus::SpotForm { name, url, field } => { if *field == 0 { insert_str_at(name, &mut input_cur, &s); } else if *field == 1 { insert_str_at(url, &mut input_cur, &s); } }
                Focus::SpotRename(buf, _) | Focus::SpotEditName(buf, _) => insert_str_at(buf, &mut input_cur, &s),
                Focus::SettingsEdit(idx, buf) => {
                    let filtered: String = if *idx == 6 {
                        s.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect()
                    } else {
                        s.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').collect()
                    };
                    insert_str_at(buf, &mut input_cur, &filtered);
                }
                Focus::Settings if set_sel == 17 => { cfg.google_maps_api_key = s.trim().to_string(); let _ = config::save_config(&cfg); addr = "APIキー設定(自動保存)".into(); }
                _ => {}
            } }
            Some(Event::Resize(..)) => { let _ = write!(out, "\x1b[2J"); force_reemit = true; } // 端末サイズ変更: 全消去して次フレームで再描画(インライン画像の残像防止)
            _ => {}
        }
    }
    // 雨雲の背景ポーラーは drop でスレッドを join する。終了時にちょうど取得中だと、その分
    // (HTTPは最大20秒)終了が固まって見えるので join を別スレッドへ逃がす(プロセス終了で消える)。
    if let Some(rc) = radar_clock.take() { std::thread::spawn(move || drop(rc)); }
    persist_full_state(cx, cy, z, &opts, &wps, &mode, &mut cfg, radar_on, show_spots);
    Ok(())
}


