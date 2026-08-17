// 対話UI(ui.rs の interactive())が持つ状態をまとめた構造体。
//
// もともと interactive() のローカル変数として約110個並んでいたものをここへ移した。
// 目的は「キー処理・ジョブ取り込みを関数へ切り出せるようにする」こと。裸のローカル変数だと
// 引数が40個を超えて関数化できず、そのために macro_rules! を使うしかなかった。
// 設計の経緯と分割の段取りは docs/ui-refactor-design.md を参照。
//
// 端末ハンドル(out)・タイルローダー(loader)・端末復元用の TermGuard はここに入れず、
// interactive() のローカルのままにしてある(前者2つは Env として関数へ渡す)。
// 理由は、この構造体を設定ファイルもネットワークも触らずに作れるようにして
// テストから状態遷移を検証できる状態を保つため。

use crate::focus::Focus;
use crate::geo::*;
use crate::poi::*;
use crate::roadseg::RoadSeg;
use crate::route::*;
use crate::spots::*;
use crate::ui_helpers::*;
use crate::*;
use image::RgbImage;

pub(crate) struct UiState {
    // ---- 地図位置 ----
    pub cx: f64,
    pub cy: f64,
    pub z: u32,

    // ---- 画面状態・メッセージ ----
    pub addr: String,                  // 'a' 住所 / 一時メッセージ
    pub focus: Focus,
    pub cfg: config::Config,           // 設定(streetview key / 描画既定 等・設定画面で書き換え)
    pub opts: Args,                    // 実行中に変えられる描画設定(Argsのコピー)
    // サブピクセル切り出しの上書き(設計 §5.1 のリスク項目)。起動時に1回だけ読む
    // (毎フレーム std::env::var を呼ぶ必要は無い)。未設定なら use_subpixel_window の既定。
    pub subpixel_env: Option<String>,
    pub set_sel: usize,                // 設定画面の選択行
    pub input_cur: usize,              // テキスト入力欄のカーソル位置(文字単位)。テキストFocus開始時に該当バッファ末尾へ
    pub menu_cat_sel: usize,           // Space メニュー: トップのカテゴリ選択
    pub menu_item_sel: usize,          // Space メニュー: 展開後の項目選択
    pub poimenu_sel: usize,            // 目的地カテゴリの選択行
    pub street: Option<(RgbImage, i32, f64, f64)>, // 実写(画像, heading, lat, lon)
    pub sv_fov: f64, // 実写のズーム(画角・度。小さいほどズームイン)。実写を開き直すたび既定値に戻す

    // ---- ルート ----
    pub spec: OverlaySpec,             // 描画に渡すオーバーレイ一式(--range のリングは保持)
    pub wps: Vec<(f64, f64)>,          // 始点..終点
    pub wp_sel: usize,                 // Tab で巡回する選択 waypoint
    pub road_segs: Vec<RoadSeg>,       // 道路名検索(r)で追加した道路の塊(別色レイヤ・spec.roadsへ同期)
    pub road_sel: usize,               // 道路一覧(RoadList)の選択行
    pub grab: bool,                    // 並べ替えビューで地点を「掴んで」移動中か
    pub route_sel: usize,              // Map左袖ルートパネルの選択(0..n=点 / 以降=操作行)
    // ルートパネルの操作行(Enterで既存のMenuActionを実行・ロジック再利用)は menu.rs の ROUTE_ACTS。
    pub mode: String,
    pub pois: Vec<(f64, f64, String, PoiCat)>, // 目的地検索結果
    pub poi_sel: usize,
    pub poi_label: String,
    pub route_names: Vec<String>,      // お気に入り一覧(L)
    pub rn_sel: usize,
    pub help: bool,                    // ? でヘルプ表示
    pub help_page: usize,              // ヘルプが画面高に収まらない時のページ送り(0始まり)
    pub qr_view: Option<QrView>,       // o でGoogleマップQRをポップアップ表示
    pub route_alt: u32,                // n で BRouter の代替ルート(0..=3)を巡回
    pub route_ele: Vec<f64>,           // 直近ルートの標高列(pts と同数)
    pub route_ascend: f64,             // 直近ルートの累積登り(m)
    pub show_elev: bool,               // E で標高プロファイル表示

    // ---- 現在地(GPS) ----
    pub gps_rx: Option<gpslive::GpsPoller>, // G ライブ現在地(drop で停止)
    pub gps_pos: Option<(f64, f64)>,        // 最新の自位置
    pub gps_trail: Vec<(f64, f64)>,         // 通過ブレッドクラム
    // web/touch-overlay.js からブラウザのGeolocation APIで送られてくるライブ現在地。
    // gps_rx(CoreLocationCLI・Mac本体の位置)とは別経路だが、描画(gps_pos/gps_trail)は共有する。
    pub web_gps_active: bool,
    // R でルート一覧(左袖)を隠す。ルート自体(wps)は保持したまま表示だけ消す
    // (画面が狭いWeb版で、ルートがある間ずっと出っぱなしなのが邪魔だという要望への対応)。
    // Tab等でRoutePanelへ実際にフォーカスした時は隠さない(操作したいのに何も見えないと困るため)。
    pub route_panel_hidden: bool,

    // ---- 雨雲レーダー(気象庁ナウキャスト) ----
    // C で ON/OFF、< > で表示時刻を前後。起動時の状態は設定 [radar] enabled(既定OFF)に従う。
    // ONにした人だけが外部サービスへ問い合わせる。
    pub radar_on: bool,
    pub radar_tl: radar::Timeline, // フレーム時刻の一覧(RadarClock が5分ごとに更新)
    pub radar_idx: usize,          // 表示中のコマ
    pub radar_follow: bool,        // 最新の実況に追従するか(< > でスクラブすると外れる)
    // 時刻一覧の背景ポーラー(drop で停止)。起動時ONなら最初から立てておく(一覧が届くまでは「時刻取得中…」)。
    pub radar_clock: Option<radar::RadarClock>,

    // ---- ルート再生(A) ----
    pub play: Option<f64>,      // 先頭からの距離m。Noneで停止
    pub play_speed: f64,        // 再生速度倍率(再生中に [ ] で 0.25〜8x)
    pub play_last_tick: Option<std::time::Instant>, // 再生の実時間ベース進行用(前回フレームの時刻)
    // 実画像モードでのルート再生ちらつき対策: 先読みスレッドがbuild_window(重い/ネットワーク)を
    // 事前に進めておき、メインは受け取った画像を使うだけにする(無ければ従来通り同期取得にfallback)。
    pub play_prefetch_rx: Option<std::sync::mpsc::Receiver<(f64, RgbImage)>>,
    pub play_prefetch_held: Option<(f64, RgbImage)>,
    pub play_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub play_speed_bits: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub play_wants_prefetch: bool, // 再生開始直後に一度だけ、rw/rh/rz確定後に先読みスレッドを起こすフラグ

    // ---- 検索キャッシュ・ポップアップ ----
    pub scache: std::collections::HashMap<String, searchcache::CacheEntry>, // 検索結果キャッシュ(キーワード+位置→結果。API節約)
    pub popup: Option<String>, // 中央に出す一時ポップアップ(スポット名等・任意キーで閉じる)

    // ---- バックグラウンドジョブ ----
    // ルート計算の受信(マーカーは即時、ルート線は別スレッド)
    pub route_note: Option<String>,
    pub route_job: Option<route::RouteRx>,
    // ルート音声案内(cfg.voice_guide_enabled時のみ使う)。曲がり角一覧はルートが決まる
    // (route_jobが完了する)たびに背景取得し直す。voice_guideはturn_pointsと対で持ち、
    // ルートが変わったら作り直す(VoiceGuide::matches_lenで長さ不一致を検知)。
    pub turn_points: Vec<route::TurnPoint>,
    pub turn_job: Option<route::TurnRx>,
    pub voice_guide: Option<voice::VoiceGuide>,
    // 気象警報(#79・ルートベース)。turn_jobと同じ「ルート確定時」フックで作り直す。
    pub route_warnings: Vec<warning::ActiveWarning>,
    pub route_warning_job: Option<std::sync::mpsc::Receiver<Vec<warning::ActiveWarning>>>,

    // ---- 地図に重ねる7種のプロットデータ ----
    // 取得単位(メッシュ/整備局/都道府県)・TTL・ズーム下限・ディスク永続化はすべて
    // plotlayer/plotcache 側が持つ。ここは毎フレーム tick して結果を読むだけ。道路交通量は
    // cfg.traffic_enabled、カメラは camera_enabled、規制は regulation_enabled、
    // 過去災害は disaster_enabled、500mメッシュ人口は population_enabled で ON/OFFする。
    // 主要道路(#73)は交通量の観測点をラインへスナップする下地なので交通量と連動する。
    pub traffic_layer: plotlayer::PlotLayer<traffic::TrafficPoint>,
    pub roads_layer: plotlayer::PlotLayer<plotlayer::RoadShape>,
    pub camera_layer: plotlayer::PlotLayer<camera::RoadCamera>,
    pub regulation_layer: plotlayer::PlotLayer<regulation::ClosureEvent>,
    pub disaster_layer: plotlayer::PlotLayer<disaster::DisasterSite>,
    // 市区町村境界(気象庁 class20s)。過去災害を塗り分ける(コロプレス)ためだけの下地なので、
    // 過去災害がONでかつ塗りがONのときだけ取りに行く。
    pub boundary_layer: plotlayer::PlotLayer<muni::MuniArea>,
    pub population_layer: plotlayer::PlotLayer<population::PopMesh>,
    // 期限切れ/上限超過のキャッシュ掃除は1セッション1回、最初のアイドル到達時に別スレッドで走らせる
    // (起動を遅くせず、無操作のたびにディレクトリを走査もしない)。
    pub plot_gc_done: bool,

    // Nキーで中心近くのカメラを選び、フル画像を取得して全画面表示する(実写Street Viewと同じ
    // 早期returnパターン)。パン/ズームは無い(道路カメラは固定視点の1枚画像のため)。
    pub cam_view: Option<(RgbImage, camera::RoadCamera)>,
    pub cam_job: Option<std::sync::mpsc::Receiver<(camera::RoadCamera, Result<RgbImage, String>)>>,
    // Bキーで中心近くの災害履歴の地点を選び、その地点の事例一覧(2段目)を取って中央パネルに出す。
    // 集計(1段目)には事例の名称も日付も入っていないので、押したときだけ引く。結果は保存しない。
    pub disaster_view: Option<(String, Vec<String>)>, // (見出し, 本文行)
    pub disaster_job: Option<std::sync::mpsc::Receiver<Result<(String, Vec<String>), String>>>,
    // 通行規制の詳細(Tキー。なぜ通れないかの規制原因等)。disaster_viewと同じ「見出し+本文行」形。
    pub regulation_detail_view: Option<(String, Vec<String>)>,
    pub regulation_detail_job: Option<std::sync::mpsc::Receiver<Result<regulation::ClosureDetail, String>>>,
    // 渋滞状況の色分け(#渋滞情報)。ルート成功のたびに、設定ONならGoogle Directionsへ別途確認する。
    pub traffic_color_job: Option<route::TrafficColorRx>,
    // 規制原因アイコン(事故✕/工事)。表示中のClosedイベントについて1件ずつ規制原因を
    // バックグラウンドで取得し分類する(セッション内メモリのみ、無期限保持)。
    // 結果にdetail_idを添えて返す(ClosureDetail自体はidを持たないため紐付けに必要)。
    pub cause_cache: std::collections::HashMap<String, regulation::CauseCategory>,
    pub cause_job: Option<std::sync::mpsc::Receiver<(String, Result<regulation::ClosureDetail, String>)>>,
    // 読み上げの声(#78)の試聴。SettingsPick(27)でSpace=試聴/Enter確定後の1回再生の両方で使う。
    pub voice_preview_job: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    // ルート計算と同じ非同期パターンで、検索/周辺/実写/おすすめの通信もバックグラウンド化する。
    // 新規spawn時に古いrxはdropされる=最新のみ採用(generation ID不要)。
    pub search_job: Option<std::sync::mpsc::Receiver<(String, String, Result<Vec<(f64, f64, String)>, String>)>>, // (ckey, query, geocode結果)
    pub near_job: Option<std::sync::mpsc::Receiver<(String, Result<Vec<(f64, f64, String)>, ApiError>)>>, // (query, search_nearbyのosm結果)
    pub street_job: Option<std::sync::mpsc::Receiver<(f64, f64, i32, Result<image::RgbImage, String>)>>, // (lat, lon, heading, 実写画像)
    pub recommend_job: Option<std::sync::mpsc::Receiver<Result<Vec<(f64, f64, String)>, String>>>, // 実在確認済みスポット列
    pub road_job: Option<std::sync::mpsc::Receiver<(String, Result<Vec<(Vec<(f64, f64)>, bool)>, String>)>>, // (道路名, roadsearch::fetch結果)
    pub wander_job: Option<std::sync::mpsc::Receiver<Result<Vec<(f64, f64)>, String>>>, // おまかせ周回(wander_route)結果
    pub catpoi_job: Option<std::sync::mpsc::Receiver<(String, Result<Vec<(f64, f64, String, PoiCat)>, ApiError>)>>, // (カテゴリ名, poi_search結果)。ラベルは起動時に確定して送るので途中でpoi_kindsを編集されても安全
    pub spin: usize, // 通信中スピナーのフレーム(毎ループ+1)

    // ---- 目的地カテゴリ・マイスポット ----
    pub poi_kinds: Vec<PoiKind>, // 目的地カテゴリ(並べ替え/追加/削除可・~/.config/termmap/poi-kinds.txt)
    pub spots: Vec<Spot>,        // マイスポット
    pub spot_cats: Vec<(String, u8, u8)>,
    pub show_spots: bool,        // 前回終了時の表示/非表示を引き継ぐ
    pub sp_sel: usize,
    pub cat_sel: usize,
    pub cur_cat: String, // スポット一覧で表示中のカテゴリ
    pub pending_spot: Option<(f64, f64, String)>, // 検索結果からお気に入り登録する際の保留(座標+名前)。カテゴリ選択待ち
    pub list_offset: usize, // 左袖リストのスクロール開始位置(表示中の1リストで共有・ensure_visibleで追従)
    pub color_sel: u8,      // 色ピッカーで選択中のパレットindex
    pub shape_sel: u8,      // 形状ピッカーで選択中の形状index
    pub set_pick_sel: usize, // 設定画面の一覧選択(SettingsPick)で選択中の候補index
    pub onboard: bool,       // 初回起動なら操作案内を出す

    // ---- 確認待ち(y/n) ----
    pub spot_move_confirm: Option<usize>, // m(中心へ移動)の確認待ち。上書きは破壊的なのでy/nを挟む
    pub save_confirm: Option<String>, // 保存名が既存の場合の上書き確認待ち(y=上書き/他=名前を変更して新規登録)
    pub clear_route_confirm: bool,    // c(ルート全消去)の確認待ち(y=消去/他=取消)
    pub route_name_hint: String,      // 直近に読み込み/保存したルート名(Sで保存欄を開く際そのまま出す)
    pub quit_confirm: bool,           // Map で Esc二連打 → 終了確認(y=終了/他=取消)
    pub last_esc_at: Option<std::time::Instant>, // 直前のEsc押下時刻(二連打判定用)

    // 操作UI効果音(macOS afplay)。設定OFF/非macOS/afplay不在なら no-op。設定トグルで作り直す。
    pub snd: sound::Sound,

    // ---- 実画像モードの再emit抑制 ----
    // 直近にemitした地図画像の状態シグネチャを保持し、変化が無いフレームでは PNG を吐き直さない
    // (チラつき/負荷の回避)。force_reemit は popup/ヘルプ/実写など地図矩形を覆う描画の後に、
    // 残像を消すため次フレームで1度だけ強制再emitさせる。
    pub last_map_sig: Option<u64>,
    pub force_reemit: bool,
    pub prev_map_covered: bool, // map_coveredの立ち上がり/下がりエッジ検出用(被ってる間は毎フレーム強制しない)
    // 移動検知: 直近に描画した(cx,cy,z)と比べて動いていれば低解像度・止まって一定時間(350ms)経てば設定解像度へ。
    pub prev_render_cxyz: Option<(f64, f64, u32)>,
    pub moved_at: Option<std::time::Instant>,
    pub emit_count: u64, // 実画像emit回数。一定間隔でscrollbackを掃除しメモリ肥大を防ぐ

    // ---- 地図パン ----
    // 既定は細かい1歩、同方向を短間隔で連打/押しっぱなしすると徐々に加速する。
    pub pan_streak: u32,
    pub last_pan_dir: Option<crossterm::event::KeyCode>,
    pub last_pan_at: std::time::Instant,
    // web版(ブラウザ)のドラッグ軸モード通知(#87)。前回送った値を覚えておき、変わったフレーム
    // だけ OSC 9997 を送る。req_pending はブラウザからの再送要求(DRAGMODE?)を受けた印で、
    // 値が変わっていなくても次フレームで1回送らせる。
    pub prev_drag_axes: Option<(dragmode::Axis, dragmode::Axis)>,
    pub drag_mode_req_pending: bool,
    // 端末1セルの縦横比(セル高/セル幅)。web版(ブラウザ)から CELL マーカーで届いた値を覚えておく
    // (設計書 docs/web-image-aspect-ratio-design.md §7.2 の経路2)。ネイティブ端末は毎フレーム
    // window_size() から取れるので保持しない。どちらも無ければ既定値 2.0 へ落ちる。
    pub cell_ratio_web: Option<f64>,
}

impl UiState {
    // 設定・ディスク・ネットワークに触らない素の初期状態。new() とテストの共通土台。
    fn blank(a: &Args, cx: f64, cy: f64, z: u32, cfg: config::Config) -> UiState {
        let mut opts = a.clone();
        // config を既定として適用(CLIフラグは ON 方向で優先。style は CLI が既定osmなら config 採用)
        opts.braille = opts.braille || cfg.braille;
        opts.classify = opts.classify || cfg.classify;
        opts.edge = opts.edge || cfg.edge;
        opts.mono = opts.mono || cfg.mono;
        if opts.style == "osm" {
            opts.style = cfg.style.clone();
        }
        let (home_lat, home_lon) = pixel_to_deg(cx, cy, z);
        UiState {
            cx,
            cy,
            z,
            addr: String::new(),
            focus: Focus::Map,
            set_sel: 0,
            input_cur: 0,
            menu_cat_sel: 0,
            menu_item_sel: 0,
            poimenu_sel: 0,
            street: None,
            sv_fov: 90.0,
            spec: build_spec(a, home_lat, home_lon),
            wps: a.route.clone().unwrap_or_default(),
            wp_sel: 0,
            road_segs: Vec::new(),
            road_sel: 0,
            grab: false,
            route_sel: 0,
            mode: a.route_mode.clone(),
            pois: Vec::new(),
            poi_sel: 0,
            poi_label: String::new(),
            route_names: Vec::new(),
            rn_sel: 0,
            help: false,
            help_page: 0,
            qr_view: None,
            route_alt: 0,
            route_ele: Vec::new(),
            route_ascend: 0.0,
            show_elev: false,
            gps_rx: None,
            gps_pos: None,
            gps_trail: Vec::new(),
            web_gps_active: false,
            route_panel_hidden: false,
            radar_on: cfg.radar_enabled,
            radar_tl: radar::Timeline::default(),
            radar_idx: 0,
            radar_follow: true,
            radar_clock: None,
            play: None,
            play_speed: 1.0,
            play_last_tick: None,
            play_prefetch_rx: None,
            play_prefetch_held: None,
            play_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            play_speed_bits: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1.0f64.to_bits())),
            play_wants_prefetch: false,
            scache: std::collections::HashMap::new(),
            popup: None,
            route_note: None,
            route_job: None,
            turn_points: Vec::new(),
            turn_job: None,
            voice_guide: None,
            route_warnings: Vec::new(),
            route_warning_job: None,
            traffic_layer: plotlayer::traffic(),
            roads_layer: plotlayer::roads(),
            camera_layer: plotlayer::camera(),
            regulation_layer: plotlayer::regulation(),
            disaster_layer: plotlayer::disaster(),
            boundary_layer: plotlayer::boundary(),
            population_layer: plotlayer::population(),
            plot_gc_done: false,
            cam_view: None,
            cam_job: None,
            disaster_view: None,
            disaster_job: None,
            regulation_detail_view: None,
            regulation_detail_job: None,
            traffic_color_job: None,
            cause_cache: std::collections::HashMap::new(),
            cause_job: None,
            voice_preview_job: None,
            search_job: None,
            near_job: None,
            street_job: None,
            recommend_job: None,
            road_job: None,
            wander_job: None,
            catpoi_job: None,
            spin: 0,
            poi_kinds: Vec::new(),
            spots: Vec::new(),
            spot_cats: Vec::new(),
            show_spots: cfg.show_spots,
            sp_sel: 0,
            cat_sel: 0,
            cur_cat: String::new(),
            pending_spot: None,
            list_offset: 0,
            color_sel: 0,
            shape_sel: 0,
            set_pick_sel: 0,
            onboard: false,
            spot_move_confirm: None,
            save_confirm: None,
            clear_route_confirm: false,
            route_name_hint: String::new(),
            quit_confirm: false,
            last_esc_at: None,
            snd: sound::Sound::new(cfg.sound_enabled),
            last_map_sig: None,
            force_reemit: true,
            prev_map_covered: false,
            prev_render_cxyz: None,
            moved_at: None,
            emit_count: 0,
            pan_streak: 0,
            last_pan_dir: None,
            last_pan_at: std::time::Instant::now(),
            prev_drag_axes: None,
            drag_mode_req_pending: false,
            cell_ratio_web: None,
            // 環境変数の読み取りはディスクにもネットワークにも触らないので blank() で行う
            // (interactive() と同じ「起動時に1回だけ」を保つ)。
            subpixel_env: std::env::var("TERMMAP_SUBPIXEL").ok(),
            opts,
            cfg,
        }
    }

    // 実際の起動用。設定・マイスポット・検索キャッシュの読み込みと、
    // 起動時ONならレーダー時刻ポーラーの起動・初回ルート計算まで行う。
    pub(crate) fn new(a: &Args, cx: f64, cy: f64, z: u32) -> UiState {
        let cfg = config::load_config();
        let mut st = UiState::blank(a, cx, cy, z, cfg);
        if st.radar_on {
            st.radar_clock = Some(radar::start_clock(radar_refresh_secs(&st.cfg)));
        }
        // ループ開始前(regulation_layerがまだ何も取得していない)なので通行止め回避は無し。
        let (n_, j_) = trigger_route(
            &mut st.spec,
            &st.wps,
            &st.pois,
            &st.mode,
            0,
            &st.cfg.google_maps_api_key,
            "",
        );
        st.route_note = n_;
        st.route_job = j_;
        st.scache = searchcache::load();
        st.poi_kinds = load_poi_kinds();
        st.spots = load_spots();
        st.spot_cats = load_spot_cats();
        st.onboard = onboarded_marker().map_or(false, |p| !p.exists());
        apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots);
        st
    }

    // ズーム変更(+/-)直後に呼ぶ。再生(play)中は先読みスレッドが再生開始時のズームを
    // 捕まえたまま動き続けるため、そのままだと「古いズームの先読み画像」と「新ズーム基準の
    // オーバーレイ(クロスヘア/ルート線)」でスケールが食い違い表示が壊れる。再生中にズームが
    // 変わったら先読みを取消し、次フレームで新ズームを使って再起動する(再生距離playは維持)。
    pub(crate) fn restart_prefetch_on_zoom(&mut self) {
        if self.play.is_some() {
            self.play_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.play_prefetch_rx = None;
            self.play_prefetch_held = None;
            self.play_wants_prefetch = true;
        }
    }

    // 雨雲レーダーONの表示状態だけを作る(コマ位置は必ず最新の実況 now_idx から始める。
    // 一覧が未着なら idx=0 のまま「時刻取得中…」になる)。
    // 時刻ポーラーの起動と分けてあるのは、ここをネットワークに触らずテストできるようにするため。
    fn radar_view_turn_on(&mut self) {
        self.radar_on = true;
        self.radar_idx = self
            .radar_tl
            .now_idx
            .min(self.radar_tl.frames.len().saturating_sub(1));
        self.radar_follow = true;
        self.addr = "雨雲レーダー: ON (出典: 気象庁ナウキャスト)".into();
    }

    // 雨雲レーダーをONにする(C と > の共通処理)。時刻一覧の背景ポーラーがまだ無ければ起こす。
    pub(crate) fn radar_turn_on(&mut self) {
        if self.radar_clock.is_none() {
            self.radar_clock = Some(radar::start_clock(radar_refresh_secs(&self.cfg)));
        }
        self.radar_view_turn_on();
    }

    // 雨雲レーダーの ON/OFF を反転する(Spaceメニューの「雨雲レーダー」と設定画面の行から使う。
    // 地図での C キーも同じ処理)。OFFにするとき背景ポーラーの drop はスレッドを join するため、
    // 取得中(HTTPは最大20秒)にここで待つと入力が固まる。停止フラグは drop 側で即座に立つので、
    // join だけを別スレッドへ逃がしてUIを待たせない。
    pub(crate) fn radar_toggle(&mut self) {
        if self.radar_on {
            self.radar_on = false;
            if let Some(rc) = self.radar_clock.take() {
                std::thread::spawn(move || drop(rc));
            }
            self.addr = "雨雲レーダー: OFF".into();
        } else {
            self.radar_turn_on();
        }
    }

    // 500mメッシュ人口の表示/非表示。雨雲と違い背景ポーラーを持たないので、設定を反転して
    // 保存するだけでよい(次の tick が cfg.population_enabled を見てセルを取りに行く)。
    // ONにした直後は出典と、取得が重いことを1回だけ知らせる(31MBが無言で落ちないように)。
    // 設定の保存と分けてあるのは radar_view_turn_on と同じ理由で、ここをディスクに触らず
    // テストできるようにするため(保存は cfg を書き換えた後なのでどちらの順でも結果は同じ)。
    fn population_toggle_view(&mut self) {
        self.cfg.population_enabled = !self.cfg.population_enabled;
        self.addr = if self.cfg.population_enabled {
            format!(
                "人口メッシュ: ON({}年) {}",
                self.cfg.population_year,
                population::ATTRIBUTION
            )
        } else {
            "人口メッシュ: OFF".to_string()
        };
        // 再描画は cfg.population_enabled を map_sig に混ぜてあるので自動で起きる(force_reemit不要)。
    }

    pub(crate) fn population_toggle(&mut self) {
        self.population_toggle_view();
        let _ = config::save_config(&self.cfg);
    }

    // 過去災害の塗り(コロプレス)の ON/OFF を反転する(Spaceメニュー・設定画面・地図での F キーの
    // 3経路共通処理、population_toggle と同じ構成)。ONにした直後だけ境界データの出典を1回出す。
    fn disaster_fill_toggle_view(&mut self) {
        self.cfg.disaster_fill = !self.cfg.disaster_fill;
        self.force_reemit = true; // 今表示している地図の見た目が変わる
        self.addr = if self.cfg.disaster_fill && self.cfg.disaster_enabled {
            "過去災害の塗り: 市区町村境界 気象庁".to_string()
        } else if self.cfg.disaster_fill {
            "過去災害の塗り: ON(「過去災害」もONにすると出る)".to_string()
        } else {
            "過去災害の塗り: OFF".to_string()
        };
    }

    pub(crate) fn disaster_fill_toggle(&mut self) {
        self.disaster_fill_toggle_view();
        let _ = config::save_config(&self.cfg);
    }

    // 「利用者が結果を待っている」ジョブが1本でも走っているか。ステータス行のスピナー・
    // 入力待ちのポーリング判定・中断(Ctrl-C / 地図のEsc)の可否で同じ一覧を見る必要があり、
    // もとは同じ13本の並びが ui.rs に4回書かれていた。ジョブを増やすときはここだけ直す。
    // 声の試聴(voice_preview_job)は中断の対象外なので、この一覧には入れない。
    pub(crate) fn jobs_active(&self) -> bool {
        self.route_job.is_some()
            || self.search_job.is_some()
            || self.near_job.is_some()
            || self.street_job.is_some()
            || self.cam_job.is_some()
            || self.recommend_job.is_some()
            || self.road_job.is_some()
            || self.catpoi_job.is_some()
            || self.wander_job.is_some()
            || self.disaster_job.is_some()
            || self.regulation_detail_job.is_some()
            || self.traffic_color_job.is_some()
            || self.cause_job.is_some()
    }

    // 進行中ジョブを全部捨てる(Ctrl-C と 地図の Esc)。受信側を落とすだけで、走っている
    // スレッドは結果を送れずに終わる。ルート計算だけは経路欄にも「中断」を残す。
    pub(crate) fn cancel_jobs(&mut self) {
        if self.route_job.is_some() {
            self.route_note = Some("中断".to_string());
        }
        self.route_job = None;
        self.search_job = None;
        self.near_job = None;
        self.street_job = None;
        self.cam_job = None;
        self.recommend_job = None;
        self.road_job = None;
        self.catpoi_job = None;
        self.wander_job = None;
        self.disaster_job = None;
        self.regulation_detail_job = None;
        self.traffic_color_job = None;
        self.cause_job = None;
        self.addr = "中断".into();
    }

    // road_segs の変更後に描画用の spec.roads を作り直す(trigger_route等では消えない別レイヤ)。
    pub(crate) fn sync_roads(&mut self) {
        self.spec.roads = self
            .road_segs
            .iter()
            .map(|r| Route {
                pts: r.pts.clone(),
                color: r.color,
                thickness: 2,
            })
            .collect();
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    // テスト用の Args(CLI 既定値と同じ)。parse_args() を通さずに UiState を作るために使う。
    pub(crate) fn test_args() -> Args {
        Args {
            lat: None,
            lon: None,
            place: None,
            zoom: 14,
            width: None,
            win_px: 640,
            style: "osm".to_string(),
            braille: false,
            mono: false,
            classify: false,
            edge: false,
            here: false,
            threshold: None,
            range: Vec::new(),
            home: None,
            route: None,
            route_mode: "surface".to_string(),
            gpx: None,
            load_route: None,
            save_route: None,
            list_routes: false,
            share: false,
            wander: false,
            dist: None,
            shape: "loop".to_string(),
            image: None,
            png: None,
        }
    }

    // テスト用の設定。効果音だけは必ずOFFにする。Sound::new(true) は一時ディレクトリへ
    // wav を書き出して CoreAudio へ登録するので、sound.rs のテストと並列に走ると同じ
    // ファイルを同時に触ってプロセスごと落ちる。
    pub(crate) fn test_cfg() -> config::Config {
        config::Config { sound_enabled: false, ..config::Config::default() }
    }

    // 設定ファイルもネットワークも触らない UiState。状態遷移のテスト専用。
    pub(crate) fn test_state() -> UiState {
        UiState::blank(&test_args(), 0.0, 0.0, 14, test_cfg())
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn frame(basetime: &str) -> radar::Frame {
        radar::Frame {
            basetime: basetime.to_string(),
            validtime: basetime.to_string(),
            kind: radar::FrameKind::Observed,
            product: radar::RadarProduct::Nowcast,
        }
    }

    #[test]
    fn blank_applies_config_defaults_to_draw_options() {
        let mut cfg = test_cfg();
        cfg.braille = true;
        cfg.style = "gsi".to_string();
        let st = UiState::blank(&test_args(), 0.0, 0.0, 14, cfg);
        assert!(st.opts.braille, "config の braille が描画設定へ入る");
        assert_eq!(st.opts.style, "gsi", "CLIが既定osmなら config の style を採る");
    }

    #[test]
    fn blank_keeps_cli_style_over_config() {
        let mut a = test_args();
        a.style = "gsi_photo".to_string();
        let mut cfg = test_cfg();
        cfg.style = "gsi".to_string();
        let st = UiState::blank(&a, 0.0, 0.0, 14, cfg);
        assert_eq!(st.opts.style, "gsi_photo", "CLI指定があれば config で上書きしない");
    }

    #[test]
    fn radar_view_turn_on_jumps_to_now_frame() {
        let mut st = test_state();
        st.radar_tl.frames = vec![frame("a"), frame("b"), frame("c")];
        st.radar_tl.now_idx = 2;
        st.radar_idx = 0;
        st.radar_follow = false;
        st.radar_view_turn_on();
        assert!(st.radar_on);
        assert_eq!(st.radar_idx, 2, "最新の実況のコマから始める");
        assert!(st.radar_follow, "ONにしたら追従に戻る");
        assert!(st.addr.contains("ON"));
    }

    #[test]
    fn radar_view_turn_on_clamps_index_to_frames() {
        let mut st = test_state();
        // 時刻一覧が未着(frames空)なら idx は 0 のまま(範囲外を指さない)
        st.radar_tl.now_idx = 5;
        st.radar_view_turn_on();
        assert_eq!(st.radar_idx, 0);
    }

    #[test]
    fn radar_toggle_turns_off_without_touching_network() {
        let mut st = test_state();
        st.radar_on = true;
        st.radar_idx = 3;
        st.radar_toggle();
        assert!(!st.radar_on);
        assert_eq!(st.addr, "雨雲レーダー: OFF");
        assert_eq!(st.radar_idx, 3, "OFFではコマ位置を触らない");
    }

    // 人口メッシュ・過去災害の塗りは、設定の保存だけを別関数(population_toggle /
    // disaster_fill_toggle)へ出してある。ここで呼ぶのは保存しない *_view 側だけなので、
    // $HOME/.config/termmap/config.toml には一切触らない。
    #[test]
    fn population_toggle_view_flips_the_flag_and_names_the_source() {
        let mut st = test_state();
        assert!(!st.cfg.population_enabled);
        st.cfg.population_year = 2020;
        st.population_toggle_view();
        assert!(st.cfg.population_enabled);
        assert!(st.addr.contains("ON(2020年)"), "何年の値を出しているかを言う: {}", st.addr);
        assert!(st.addr.contains(population::ATTRIBUTION), "出典を1回出す");

        st.population_toggle_view();
        assert!(!st.cfg.population_enabled);
        assert_eq!(st.addr, "人口メッシュ: OFF");
    }

    #[test]
    fn population_toggle_view_does_not_force_a_reemit() {
        // 再描画は cfg.population_enabled が map_sig に入っていることで起きる。
        let mut st = test_state();
        st.force_reemit = false;
        st.population_toggle_view();
        assert!(!st.force_reemit);
    }

    #[test]
    fn disaster_fill_toggle_view_tells_whether_the_fill_will_actually_show() {
        // 既定は塗りON・過去災害本体OFF。まず塗りを消してから、戻したときの文言を見る。
        let mut st = test_state();
        assert!(st.cfg.disaster_fill && !st.cfg.disaster_enabled, "既定の組み合わせ");
        st.disaster_fill_toggle_view();
        assert!(!st.cfg.disaster_fill);
        assert_eq!(st.addr, "過去災害の塗り: OFF");

        // 本体がOFFのまま塗りをONへ戻すと、それだけでは出ないことを伝える
        st.force_reemit = false;
        st.disaster_fill_toggle_view();
        assert!(st.cfg.disaster_fill);
        assert_eq!(st.addr, "過去災害の塗り: ON(「過去災害」もONにすると出る)");
        assert!(st.force_reemit, "いま出ている地図の見た目が変わるので再emitする");

        // 本体もONなら境界データの出典を出す
        let mut st = test_state();
        st.cfg.disaster_enabled = true;
        st.cfg.disaster_fill = false;
        st.disaster_fill_toggle_view();
        assert!(st.cfg.disaster_fill);
        assert_eq!(st.addr, "過去災害の塗り: 市区町村境界 気象庁");
    }

    #[test]
    fn restart_prefetch_on_zoom_is_noop_while_stopped() {
        let mut st = test_state();
        st.play = None;
        st.play_wants_prefetch = false;
        st.restart_prefetch_on_zoom();
        assert!(!st.play_wants_prefetch, "再生していなければ何もしない");
        assert!(!st.play_cancel.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn restart_prefetch_on_zoom_cancels_and_requests_restart() {
        let mut st = test_state();
        st.play = Some(1234.0);
        let (_tx, rx) = std::sync::mpsc::channel();
        st.play_prefetch_rx = Some(rx);
        st.restart_prefetch_on_zoom();
        assert!(st.play_cancel.load(std::sync::atomic::Ordering::Relaxed), "先読みへ取消を伝える");
        assert!(st.play_prefetch_rx.is_none());
        assert!(st.play_prefetch_held.is_none());
        assert!(st.play_wants_prefetch, "次フレームで新ズームで再起動させる");
        assert_eq!(st.play, Some(1234.0), "再生距離は維持する");
    }

    #[test]
    fn sync_roads_rebuilds_spec_roads_from_segments() {
        let mut st = test_state();
        st.road_segs = vec![
            RoadSeg { name: "国道1号".into(), pts: vec![(35.0, 139.0), (35.1, 139.1)], color: [1, 2, 3] },
            RoadSeg { name: "県道".into(), pts: vec![(34.0, 138.0)], color: [4, 5, 6] },
        ];
        st.spec.roads = vec![Route { pts: vec![(0.0, 0.0)], color: [9, 9, 9], thickness: 9 }];
        st.sync_roads();
        assert_eq!(st.spec.roads.len(), 2, "古い内容を残さず作り直す");
        assert_eq!(st.spec.roads[0].pts, vec![(35.0, 139.0), (35.1, 139.1)]);
        assert_eq!(st.spec.roads[0].color, [1, 2, 3]);
        assert_eq!(st.spec.roads[1].thickness, 2);
    }

    // 13本すべてに受信口を差す。送信側は捨てる(結果は使わず有無だけを見るテストのため)。
    fn fill_all_jobs(st: &mut UiState) {
        st.route_job = Some(std::sync::mpsc::channel().1);
        st.search_job = Some(std::sync::mpsc::channel().1);
        st.near_job = Some(std::sync::mpsc::channel().1);
        st.street_job = Some(std::sync::mpsc::channel().1);
        st.cam_job = Some(std::sync::mpsc::channel().1);
        st.recommend_job = Some(std::sync::mpsc::channel().1);
        st.road_job = Some(std::sync::mpsc::channel().1);
        st.catpoi_job = Some(std::sync::mpsc::channel().1);
        st.wander_job = Some(std::sync::mpsc::channel().1);
        st.disaster_job = Some(std::sync::mpsc::channel().1);
        st.regulation_detail_job = Some(std::sync::mpsc::channel().1);
        st.traffic_color_job = Some(std::sync::mpsc::channel().1);
        st.cause_job = Some(std::sync::mpsc::channel().1);
    }

    #[test]
    fn jobs_active_is_false_while_nothing_runs() {
        let st = test_state();
        assert!(!st.jobs_active());
    }

    #[test]
    fn jobs_active_notices_each_job_one_at_a_time() {
        // 1本ずつ差して、13本とも一覧に入っていることを確かめる(足し忘れ検出が目的)。
        let mut probes: Vec<Box<dyn Fn(&mut UiState)>> = Vec::new();
        probes.push(Box::new(|st: &mut UiState| st.route_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.search_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.near_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.street_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.cam_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.recommend_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.road_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.catpoi_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.wander_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.disaster_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.regulation_detail_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.traffic_color_job = Some(std::sync::mpsc::channel().1)));
        probes.push(Box::new(|st: &mut UiState| st.cause_job = Some(std::sync::mpsc::channel().1)));
        assert_eq!(probes.len(), 13);
        for (i, set) in probes.iter().enumerate() {
            let mut st = test_state();
            set(&mut st);
            assert!(st.jobs_active(), "{i}番目のジョブが一覧から漏れている");
        }
    }

    #[test]
    fn jobs_active_ignores_the_background_layers_and_the_weather_warning() {
        // 背景で取りに行くだけのもの(市区町村境界・人口メッシュ・ルート沿い気象警報)は
        // 「利用者が結果を待っている」ジョブではないので、スピナーも中断の対象にもしない。
        // 取りこぼし防止のポーリング判定(ui.rs の polling)にだけ入れてある。
        let mut st = test_state();
        st.route_warning_job = Some(std::sync::mpsc::channel().1);
        assert!(!st.jobs_active());
        st.cancel_jobs();
        assert!(st.route_warning_job.is_some(), "気象警報は中断で止めない");
    }

    #[test]
    fn jobs_active_ignores_the_voice_preview() {
        // 声の試聴は中断の対象外。スピナーも出さない(短時間で終わるため)。
        let mut st = test_state();
        st.voice_preview_job = Some(std::sync::mpsc::channel().1);
        assert!(!st.jobs_active());
    }

    #[test]
    fn cancel_jobs_drops_everything_and_reports() {
        let mut st = test_state();
        fill_all_jobs(&mut st);
        st.voice_preview_job = Some(std::sync::mpsc::channel().1);
        st.turn_job = Some(std::sync::mpsc::channel().1);
        st.cancel_jobs();
        assert!(!st.jobs_active());
        assert_eq!(st.addr, "中断");
        assert_eq!(st.route_note.as_deref(), Some("中断"), "経路欄にも残す");
        assert!(st.voice_preview_job.is_some(), "試聴は止めない");
        assert!(st.turn_job.is_some(), "曲がり角の取得も止めない(ルート線に付随する裏方)");
    }

    #[test]
    fn cancel_jobs_keeps_the_route_note_quiet_when_no_route_was_running() {
        let mut st = test_state();
        st.route_note = Some("40.0km".into());
        st.search_job = Some(std::sync::mpsc::channel().1);
        st.cancel_jobs();
        assert_eq!(st.route_note.as_deref(), Some("40.0km"), "ルート計算中でなければ経路欄は触らない");
        assert_eq!(st.addr, "中断");
    }
}
