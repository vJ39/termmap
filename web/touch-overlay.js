/* termmap タッチ操作オーバーレイ
 *
 * ttyd(xterm.js)のページへ後付けして、キーボードの無い端末(iPhone等)から
 * termmap を操作できるようにする。ttyd の既定ページの body 終了タグ直前へ
 * script 要素として埋め込まれる前提(scripts/build-web-index.sh が生成する)。
 *
 * termmap 本体(Rust)には一切手を入れない。ここでやることは
 * 「タッチ操作 → xterm.js が理解するキーイベント」の変換だけで、
 * その先(xterm.js → ttyd → WebSocket → pty → termmap)は既存経路をそのまま使う。
 *
 * ── xterm.js のキー処理をどう通すか(ttyd 1.7.7 同梱版のコードを実際に読んで確認した) ──
 *
 * xterm.js は隠し要素 .xterm-helper-textarea に keydown / keypress を張っている。
 * keydown 側(_keyDown → evaluateKeyboardEvent)の挙動は次の通り:
 *
 *   1. switch(ev.keyCode) に case 13(Enter) / 27(Esc) / 37-40(矢印) がある
 *      → ここで解決したキーは keydown の中で端末へ送られる。
 *   2. 上記に当たらない文字キーは既定の分岐
 *        ev.key && !ctrl && !alt && !meta && ev.keyCode >= 48 && ev.key.length === 1
 *      でのみ解決される。
 *   3. ただし _keyDown の戻り値で
 *        ev.key が A-Z(charCode 65..90)の1文字なら「送らずに true を返す」
 *      という分岐があり、大文字はここで keydown からは送られない。
 *   4. keydown で解決できなかったものは keypress(_keyPress)側が
 *      ev.charCode / ev.which を見て String.fromCharCode() して送る。
 *
 * この結果、送出経路はキーごとに次のように分かれる。KEYS の mode はこれを表す:
 *
 *   mode:'down'  … keydown だけで送られる
 *                  矢印(37-40) / Enter(13) / Escape(27) は 1. で解決。
 *                  '+' '-' '<' '>' '?' は keyCode が 187/189/188/190/191 で
 *                  いずれも >= 48 のため 2. で解決し、A-Z ではないので 3. に
 *                  引っかからずそのまま送られる。
 *                  → keypress を足すと二重入力になるので送らない。
 *
 *   mode:'press' … keypress でしか送られない
 *                  'C' は大文字なので 3. に該当して keydown からは出ない。
 *                  ' '(空白)は keyCode 32 で、case 32 が無く 2. の keyCode >= 48 も
 *                  満たさないため keydown では未解決のまま落ちる。
 *                  → この2つは keypress(charCode 付き)で送る必要がある。
 *
 * なお ttyd 1.7.7 同梱の xterm.js には ev.isTrusted の検査が一切無いため、
 * スクリプトから合成したイベントでも実キー入力と同じ経路に乗る(確認済み)。
 *
 * ── フォーカスについて ──
 * 「.xterm-helper-textarea を focus してから発火する」方式は採らない。
 * iOS はユーザー操作起因で文字入力欄に focus が当たるとソフトキーボードを
 * せり上げるため、キーボード無しで使うという本来の目的と真正面からぶつかる。
 * イベントリスナは textarea 自身に張られているので、focus していなくても
 * dispatchEvent すれば発火する(送信経路 triggerDataEvent は focus 非依存)。
 * 副作用としてカーソルが非フォーカス表示(中抜き)になるが実害は無い。
 */
(function () {
  'use strict';

  if (window.__termmapTouch) { return; } // 二重読み込み防止

  var OVERLAY_VERSION = '1.0.0';

  // ── 調整パラメータ ───────────────────────────────────────────────
  var TAP_SLOP_PX      = 12;   // 移動量がこれ以下ならタップ(Enter)扱い
  var TAP_MAX_MS       = 400;  // 接触時間がこれより長いものはタップにしない
  var PX_PER_STEP      = 40;   // スワイプ何 px ごとに矢印キー1回送るか
  var MAX_STEPS        = 15;   // 1スワイプで送る矢印キーの上限
  var STEP_INTERVAL_MS = 16;   // 連続発火の間隔。termmap 側は同方向 220ms 以内の
                               // 連続入力で pan_streak が伸びて加速する(Rust側の既存実装)
  var SWIPE_MIN_PX     = 24;   // これ未満の移動はスワイプとして扱わない(タップにも当たらなければ無視)

  // スワイプの向き。true = 地図を指でつかんで動かす向き(Googleマップ等と同じ)。
  //   指を右へ払う → 地図が右へ動く → 見えるのは西側 → ArrowLeft を送る
  // false にすると「指の向き = 視点の移動方向」になる(右へ払う → 東へ進む)。
  var DRAG_MAP = true;

  // ── キー定義 ────────────────────────────────────────────────────
  // keyCode は US 配列の実キーボードが出す値に合わせてある(xterm.js が keyCode を見るため)。
  // mode は上のコメントで説明した送出経路。charCode は keypress 用。
  var KEYS = {
    ArrowLeft:  { key: 'ArrowLeft',  code: 'ArrowLeft',  keyCode: 37,  mode: 'down' },
    ArrowUp:    { key: 'ArrowUp',    code: 'ArrowUp',    keyCode: 38,  mode: 'down' },
    ArrowRight: { key: 'ArrowRight', code: 'ArrowRight', keyCode: 39,  mode: 'down' },
    ArrowDown:  { key: 'ArrowDown',  code: 'ArrowDown',  keyCode: 40,  mode: 'down' },
    Enter:      { key: 'Enter',      code: 'Enter',      keyCode: 13,  mode: 'down' },
    Escape:     { key: 'Escape',     code: 'Escape',     keyCode: 27,  mode: 'down' },

    '+':        { key: '+', code: 'Equal',  keyCode: 187, shift: true, mode: 'down' },
    '-':        { key: '-', code: 'Minus',  keyCode: 189,              mode: 'down' },
    '<':        { key: '<', code: 'Comma',  keyCode: 188, shift: true, mode: 'down' },
    '>':        { key: '>', code: 'Period', keyCode: 190, shift: true, mode: 'down' },
    '?':        { key: '?', code: 'Slash',  keyCode: 191, shift: true, mode: 'down' },

    // 大文字 A-Z は keydown からは出ないので keypress で送る
    'C':        { key: 'C', code: 'KeyC',  keyCode: 67, charCode: 67, shift: true, mode: 'press' },
    // 空白は keyCode 32 で keydown の分岐に乗らないので keypress で送る
    ' ':        { key: ' ', code: 'Space', keyCode: 32, charCode: 32,              mode: 'press' }
  };

  // ── キーイベント合成 ────────────────────────────────────────────
  function defineIfNeeded(ev, name, value) {
    if (ev[name] === value) { return; }
    // KeyboardEvent の keyCode/charCode/which は prototype 側の getter のため、
    // 環境によっては init 辞書の値が反映されない。その場合だけ上書きする。
    try {
      Object.defineProperty(ev, name, { get: function () { return value; }, configurable: true });
    } catch (e) { /* 上書きできない環境では init 辞書の値に任せる */ }
  }

  function fireKeyEvent(target, type, spec) {
    var isPress  = (type === 'keypress');
    var charCode = isPress ? (spec.charCode || 0) : 0;
    var which    = isPress ? charCode : spec.keyCode;

    var ev;
    try {
      ev = new KeyboardEvent(type, {
        key: spec.key,
        code: spec.code,
        keyCode: spec.keyCode,
        charCode: charCode,
        which: which,
        shiftKey: !!spec.shift,
        ctrlKey: false,
        altKey: false,
        metaKey: false,
        bubbles: true,
        cancelable: true,
        composed: true
      });
    } catch (e) {
      return false; // KeyboardEvent が使えない環境は対象外
    }
    defineIfNeeded(ev, 'keyCode', spec.keyCode);
    defineIfNeeded(ev, 'charCode', charCode);
    defineIfNeeded(ev, 'which', which);

    target.dispatchEvent(ev);
    return true;
  }

  function findTextarea() {
    return document.querySelector('.xterm-helper-textarea');
  }

  // xterm.js の同梱コードを実際に読むと、_keyDown ハンドラは(特定の修飾キーを除く)
  // ほぼ全てのキーで this.focus() → this.textarea.focus(...) を呼んでいる
  // (カーソル/IME状態を合わせるための内部処理と見られる)。つまり sendKey() が合成の
  // keydown を投げるたびに毎回 textarea が実フォーカスされ、iOS のソフトキーボードが
  // せり上がってしまう(地図側のタッチ伝播を止めるだけでは防げない)。
  // 対策: textarea 自身の focus() を、実フォーカスの代わりに合成 focus イベントを
  // 発火するだけの関数に差し替える。xterm.js 側は addEventListener('focus', ...) で
  // _isFocused 等の内部状態を更新しているだけなので、このイベントさえ受け取れれば
  // 実際にDOMフォーカスを移さなくても内部状態は壊れない。1つのtextareaにつき1回だけ適用する。
  var FOCUS_PATCHED = '__termmapFocusPatched';
  function neutralizeFocus(ta) {
    if (!ta || ta[FOCUS_PATCHED]) { return; }
    ta[FOCUS_PATCHED] = true;
    ta.focus = function () { ta.dispatchEvent(new Event('focus')); };
  }

  // キーを1回分(keydown [+ keypress] + keyup)送る
  function sendKey(name) {
    var spec = KEYS[name];
    if (!spec) { return false; }
    var ta = findTextarea();
    if (!ta) { return false; } // 端末がまだ描画されていない
    neutralizeFocus(ta);

    fireKeyEvent(ta, 'keydown', spec);
    if (spec.mode === 'press') { fireKeyEvent(ta, 'keypress', spec); }
    fireKeyEvent(ta, 'keyup', spec);
    return true;
  }

  // 同じキーを一定間隔で連続して送る(termmap 側の加速パンに乗せるため)
  function sendKeyBurst(name, times) {
    var left = Math.max(1, times | 0);
    (function step() {
      sendKey(name);
      if (--left > 0) { setTimeout(step, STEP_INTERVAL_MS); }
    })();
  }

  // ── ジェスチャー判定 ────────────────────────────────────────────
  var TERMINAL_SELECTOR = '#terminal-container';
  var BAR_ID = 'termmap-touchbar';

  function inTerminal(node) {
    if (!node || !node.closest) { return false; }
    if (node.closest('#' + BAR_ID)) { return false; } // ボタンバー上の操作は対象外
    return !!node.closest(TERMINAL_SELECTOR);
  }

  // スワイプの主方向(縦横のうち移動量が大きい方)を矢印キー名へ変換する
  function directionKey(dx, dy) {
    var horizontal = Math.abs(dx) >= Math.abs(dy);
    if (horizontal) {
      if (DRAG_MAP) { return dx > 0 ? 'ArrowLeft' : 'ArrowRight'; }
      return dx > 0 ? 'ArrowRight' : 'ArrowLeft';
    }
    if (DRAG_MAP) { return dy > 0 ? 'ArrowUp' : 'ArrowDown'; }
    return dy > 0 ? 'ArrowDown' : 'ArrowUp';
  }

  // 1ジェスチャーの終了時に呼ぶ。タップなら Enter、スワイプなら矢印キーの連打。
  function resolveGesture(dx, dy, dt) {
    var dist = Math.sqrt(dx * dx + dy * dy);

    if (dist <= TAP_SLOP_PX && dt <= TAP_MAX_MS) {
      sendKey('Enter');
      return 'tap';
    }
    if (dist < SWIPE_MIN_PX) { return 'none'; }

    var steps = Math.round(dist / PX_PER_STEP);
    if (steps < 1) { steps = 1; }
    if (steps > MAX_STEPS) { steps = MAX_STEPS; }
    sendKeyBurst(directionKey(dx, dy), steps);
    return 'swipe';
  }

  var gesture = null;   // { x, y, t } 追跡中のジェスチャー
  var sawTouch = false; // タッチ端末ではマウス側の代替処理を止める

  function bindGestures() {
    // タッチ(本番: iPhone等)
    document.addEventListener('touchstart', function (e) {
      if (!inTerminal(e.target)) { return; }
      // ブラウザ既定のスクロール/ダブルタップ拡大/フォーカス移動を止める「だけ」では不十分:
      // preventDefault はブラウザの既定動作を止めるだけで、ttyd 同梱 xterm.js が端末要素に
      // 直接張っている touchstart/mousedown ハンドラ(タップで .xterm-helper-textarea を
      // focus してソフトキーボードをせり上げる処理)までは止められない。document の capture
      // フェーズで stopPropagation して、そのハンドラにイベントを届かせないようにする。
      e.preventDefault();
      e.stopPropagation();
      sawTouch = true;
      if (e.touches.length !== 1) { gesture = null; return; } // ピンチ等は無視
      var t = e.touches[0];
      gesture = { x: t.clientX, y: t.clientY, t: Date.now() };
    }, { capture: true, passive: false });

    document.addEventListener('touchmove', function (e) {
      if (!inTerminal(e.target)) { return; }
      e.preventDefault();                       // 慣性スクロール/ピンチ拡大の暴発を抑える
      e.stopPropagation();                       // 理由は touchstart 側のコメント参照
      if (e.touches.length !== 1) { gesture = null; }
    }, { capture: true, passive: false });

    document.addEventListener('touchend', function (e) {
      if (!inTerminal(e.target)) { return; }
      e.preventDefault();
      e.stopPropagation();                       // 理由は touchstart 側のコメント参照
      if (!gesture) { return; }
      var t = e.changedTouches && e.changedTouches[0];
      if (t) { resolveGesture(t.clientX - gesture.x, t.clientY - gesture.y, Date.now() - gesture.t); }
      gesture = null;
    }, { capture: true, passive: false });

    document.addEventListener('touchcancel', function () { gesture = null; }, { capture: true });

    // マウス(PCブラウザでの動作確認用。タッチが一度でも来たら無効化する)
    document.addEventListener('mousedown', function (e) {
      if (sawTouch || !inTerminal(e.target)) { return; }
      gesture = { x: e.clientX, y: e.clientY, t: Date.now() };
    }, true);

    document.addEventListener('mouseup', function (e) {
      if (sawTouch || !gesture || !inTerminal(e.target)) { return; }
      resolveGesture(e.clientX - gesture.x, e.clientY - gesture.y, Date.now() - gesture.t);
      gesture = null;
    }, true);
  }

  // ── 画面下部のボタンバー ────────────────────────────────────────
  // q(終了)は意図的に置いていない。誤タップでセッションごと落ちる方が損失が大きいため、
  // 離脱はブラウザのタブを閉じる操作に任せる。
  var BUTTONS = [
    { label: 'Menu', key: ' ',      title: 'メニュー (Space)' },
    { label: '−',    key: '-',      title: 'ズームアウト' },
    { label: '＋',   key: '+',      title: 'ズームイン' },
    { label: '☂',    key: 'C',      title: '雨雲レーダー 表示/非表示' },
    { label: '◀',    key: '<',      title: '雨雲を5分前へ' },
    { label: '▶',    key: '>',      title: '雨雲を5分後へ' },
    { label: 'Esc',  key: 'Escape', title: '戻る / 取消' },
    { label: '?',    key: '?',      title: 'ヘルプ' }
  ];

  // レイアウトの考え方:
  // ボタンバーを position:fixed で端末の上に浮かせると、見た目の占有分だけ地図の行が
  // 隠れるうえ、ttyd(FitAddon/ResizeObserver)が見ている #terminal-container の実寸は
  // 縮まないままなので、端末の桁数/行数と実際に見えている領域がズレる。
  // そこで body を flex 縦積みにして、バーを通常フローの要素として最後に置き、
  // #terminal-container を flex:1 で「残り全部」にする。こうすると端末の箱そのものが
  // バーの分だけ小さくなるので、画面回転時も ResizeObserver → PTY リサイズが
  // 正しい寸法で走り、バーが地図に被らない。
  var CSS = [
    'html { height: 100%; }',
    'body {',
    '  height: 100%;',
    '  height: 100dvh;',                 // 対応ブラウザではこちらが勝つ(iOS のURLバー伸縮対策)
    '  margin: 0; overflow: hidden; overscroll-behavior: none;',
    '  display: flex; flex-direction: column;',
    '}',
    // ttyd 既定の #terminal-container{height:100%} を打ち消して flex アイテムにする
    '#terminal-container {',
    '  flex: 1 1 auto;',
    '  min-height: 0;',                  // flex アイテムが内容量で縮まなくなるのを防ぐ
    '  height: auto !important;',
    // ttyd 既定の margin:0 auto を必ず打ち消す。flex アイテムは交差軸に auto マージンが
    // あると stretch されず内容幅に合わせて広がるため、そのままだと端末が画面幅を超えて
    // 右側が見切れる(画面幅 390px に対し端末 550px になる現象を実測で確認)。
    '  margin: 0 !important;',
    '  width: 100% !important;',
    '  max-width: 100%;',
    '  box-sizing: border-box;',
    '  align-self: stretch;',
    '  order: 1;',                       // DOM 上の並びに関わらず端末を上に置く
    '  touch-action: none;',
    '}',
    '#' + BAR_ID + ' {',
    '  flex: 0 0 auto;',
    '  order: 2;',                       // バーは常に一番下(ttyd 側が再描画しても崩れない)
    '  display: flex; gap: 4px; padding: 0 4px;',
    '  padding-bottom: env(safe-area-inset-bottom, 0px);',
    '  height: calc(52px + env(safe-area-inset-bottom, 0px));',
    '  box-sizing: border-box; align-items: stretch;',
    '  background: #14161a; border-top: 1px solid #2c3038;',
    '  touch-action: none; user-select: none; -webkit-user-select: none;',
    '}',
    '#' + BAR_ID + ' button {',
    '  flex: 1 1 0; min-width: 0; margin: 4px 0; padding: 0;',
    '  background: #22262d; color: #d7dbe0; border: 1px solid #343a44; border-radius: 8px;',
    // 8 個を横並びにすると幅 390px の端末で 1 個あたり約 44px しかない。
    // 17px だと "Menu" が枠からはみ出したため 15px に下げ、念のため溢れも隠す。
    '  font: 600 15px/1 -apple-system, BlinkMacSystemFont, "Helvetica Neue", sans-serif;',
    '  white-space: nowrap; overflow: hidden; text-overflow: clip;',
    '  -webkit-tap-highlight-color: transparent; touch-action: none; cursor: pointer;',
    '}',
    '#' + BAR_ID + ' button:active, #' + BAR_ID + ' button.tm-active { background: #3a7d4d; color: #fff; }'
  ].join('\n');

  function injectStyle() {
    var st = document.createElement('style');
    st.id = 'termmap-touch-style';
    st.textContent = CSS;
    document.head.appendChild(st);
  }

  function flash(btn) {
    btn.classList.add('tm-active');
    setTimeout(function () { btn.classList.remove('tm-active'); }, 120);
  }

  function buildBar() {
    var bar = document.createElement('div');
    bar.id = BAR_ID;

    BUTTONS.forEach(function (def) {
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.textContent = def.label;
      btn.title = def.title;
      btn.setAttribute('aria-label', def.title);
      btn.dataset.key = def.key;

      // タッチとクリックの二重発火を避ける。タッチ端末では touchstart 側だけを使う。
      btn.addEventListener('touchstart', function (e) {
        e.preventDefault();
        e.stopPropagation();
        sawTouch = true;
        flash(btn);
        sendKey(def.key);
      }, { passive: false });

      btn.addEventListener('click', function (e) {
        e.preventDefault();
        e.stopPropagation();
        if (sawTouch) { return; }
        flash(btn);
        sendKey(def.key);
      });

      bar.appendChild(btn);
    });

    document.body.appendChild(bar);
  }

  // iPhone で等倍表示にするための viewport 指定。ttyd 既定ページには入っていないため、
  // 万一 build スクリプト側の挿入が外れていてもここで補う。
  function ensureViewportMeta() {
    if (document.querySelector('meta[name="viewport"]')) { return; }
    var m = document.createElement('meta');
    m.name = 'viewport';
    m.content = 'width=device-width, initial-scale=1, maximum-scale=1, viewport-fit=cover';
    document.head.appendChild(m);
  }

  // 端末領域を縮めた分、xterm の桁数/行数を再計算させる。
  // ttyd は window の resize で FitAddon.fit() を呼ぶので、resize を投げれば足りる。
  // レイアウト確定のタイミングに幅があるため数回投げる。
  function refit() {
    [0, 60, 250, 700].forEach(function (ms) {
      setTimeout(function () { window.dispatchEvent(new Event('resize')); }, ms);
    });
  }

  function init() {
    ensureViewportMeta();
    injectStyle();
    buildBar();
    bindGestures();
    refit();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }

  // 動作確認・デバッグ用の入口
  window.__termmapTouch = {
    version: OVERLAY_VERSION,
    sendKey: sendKey,
    sendKeyBurst: sendKeyBurst,
    resolveGesture: resolveGesture,
    keys: KEYS,
    findTextarea: findTextarea
  };
})();
