# バックログ

未完了の作業（バグ・機能追加・リファクタリング）を追跡する統合リスト。

## index

次回採番: bug=2 / feature=8 / refactoring=1.

項目（バグ bug / 機能追加 feature / リファクタリング refactoring）を追加するときは、該当カテゴリの採番を +1 して ID を継ぐ。完了した項目は本書から削除し、番号は再利用しない（過去の使用済み番号は `git log -p -- doc/backlog.md | grep -oE '(bug|feature|refactoring)-[0-9]+' | sort -u` で確認できる）。状態は「本書に載っていれば未完了／消えていれば完了」で表す（状態列は持たない）。ゴールは、その作業で何が達成されていれば終わりなのかを1文で書く。手段ではなく到達点を書く。

考慮外は、外したい軸があるときだけ足す。「この作業では○○は考えない」と書く。書いていなければ制限は無い。

## バグ

判明済みの不具合。採番は本書冒頭「index」。各エントリは 背景／ゴール／対応／該当 で記す。

## 機能追加

実装済みコードに足す機能。採番は本書冒頭「index」。各エントリは 背景／ゴール／対応／該当 で記す。

### feature-2

**複数スキャンコードのキー（Pause）**
- ゴール：`def key Pause = E1-0x1d 0x45` のように複数スキャンコードで定義したキーが、yamy と同じく押下 1 回として割付に使える。
- 背景：yamy の `Key` はスキャンコードを最大 4 個持ち、エンジンは届いたコードを順に貯めて、定義の先頭部分に一致するうちは次を待ち、完全一致で 1 キーとして処理する（`searchKey` / `searchPrefixKey`）。配布ファイルで使うのは Pause の 1 件。パーサーの最小サブセットでは、スキャンコードが 2 個以上の `def key` は読み込みエラーにしている。
- 対応：(1) `model::Key` を複数スキャンコード対応にする。(2) エンジンに「コードを貯めて定義と照合する」処理を足す。(3) WH_KEYBOARD_LL で Pause がどう届くか（E1-0x1d と 0x45 が順に来るか、0x45 単体か）を実機で測り、フック側の変換を合わせる。(4) パーサーのエラーを外す。
- 該当：`src/model.rs`・`src/engine.rs`・`src/platform.rs`・`src/config/`。

### feature-5

**&WindowIdentify / &InvestigateCommand / &LoadSetting（調査・読込の UI）**
- ゴール：3 関数を含む `.mayu` が読み込め、押すと yamy と同じ結果が得られる。
- 背景：yamy では `&WindowIdentify` はフォーカス窓のクラス名・タイトルを画面に出す（`window` 行を書くための調査用）。`&InvestigateCommand` はフォーカス窓に届く WM_COMMAND をログに出すモードの切替。`&LoadSetting(名前)` は設定を読み直す（名前なしなら今の設定）。default.mayu の C-S-D / C-S-F1 / C-S-S に割り付けてある。
- 対応：(1) パーサーに 3 関数を足す。(2) `&LoadSetting` はトレイの再読込と同じ処理につなぐ。(3) `&WindowIdentify` はフォーカス通知で得ている `exe名:クラス:…` の列を表示に回す。(4) `&InvestigateCommand` はフック DLL で WM_COMMAND を見て通知する。未決：表示先（ログだけにするか、窓を出すか）、`&MayuDialog(Log, SHOW)` のようなダイアログをどうするか。
- 該当：`src/config/parser.rs`・`src/model.rs`・`src/main.rs`・`hook/src/lib.rs`。

### feature-6

**&Recenter / &EmacsEditKillLineFunc（編集コントロールの中での操作）**
- ゴール：emacsedit.mayu の C-L（再描画と中央寄せ）と `$EmacsEdit/kill-line` が yamy と同じく動く。
- 背景：yamy は 2 関数をフック DLL に送り、フォーカス窓のプロセスの中で Edit / RichEdit に `EM_GETSEL` などのメッセージを投げて処理する。`&Recenter` はキャレット行が画面の中央に来るようにスクロールする。`&EmacsEditKillLineFunc` はキャレットが行末なら改行を消し、そうでなければ行末までを切り取る（続けて押すと切り取り分を足す）。どちらも本体プロセスからのメッセージ送信では足りず、DLL の中で動かす必要がある。
- 対応：(1) 本体から DLL へ関数の実行を頼む経路（yamy の `WM_MAYU_MESSAGE` に当たるもの）を足す。(2) DLL に 2 関数の処理を書く。(3) パーサーに足す。
- 該当：`hook/src/lib.rs`・`ipc/src/lib.rs`・`src/window.rs`・`src/config/parser.rs`。

### feature-7

**TSF のアプリで変換中を取る**
- ゴール：ストア版メモ帳のような TSF だけで IME を使うアプリでも、`IC-`（変換中）修飾が yamy と同じく効く。
- 背景：変換中はフック DLL が `WM_IME_STARTCOMPOSITION` / `WM_IME_ENDCOMPOSITION` を見て取っている。IMM32 のアプリ（Win+R の入力欄）では取れたが、TSF のアプリにはこのメッセージが来ない。IME のオン・オフは取れている。
- 対応：TSF の `ITfThreadMgr` / `ITfContext` の状態を DLL の中から読む、または `ITfUIElementSink` 系の通知を受ける方法を調べ、取れる方法で DLL に足す。
- 該当：`hook/src/lib.rs`・`ipc/src/lib.rs`。

## リファクタリング

挙がった改善項目。採番は本書冒頭「index」。各エントリは 背景／ゴール／対応／該当 で記す。

## 実機で未確認

実装とテストは済んでいるが、実機で動かして見ていないもの。見て問題なければ本書から消す。

- トレイメニューの「再読込」：押すとログに「再読込: パス」が出て、`.mayu` の変更が効く。
- 起動できないときのメッセージボックス：存在しない `.mayu` のパスで起こすと、ログと同じ理由がメッセージボックスにも出る。
- 案内の窓（&HelpMessage / &HelpVariable）：出たとき、ピンを押したときに、フォーカス窓からフォーカスを奪わない。

## parking lot

後回し・いつかやる候補の置き場（特定の作業に紐付かない将来アイデア）。着手が決まった段で機能追加・リファクタリングへ引き上げる。

- 32 ビットプロセスへの注入：64 ビットのフック DLL は 32 ビットプロセスに入らないので、そのプロセスではフォーカス・IME の通知が来ない。yamy は 32 ビットの補助プロセス（yamyd32.exe）を別に起こしていた。
- Smart App Control：ビルドし直した exe は初回起動でブロックされる（再実行で通る）。配布するなら署名が要る。
