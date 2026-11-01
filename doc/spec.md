# 外部仕様（.mayu の構文と &関数）

内部向けの外部仕様。mayu / yamy が受け付ける構文と `&関数` を一覧にし、各行に newmayu の対応を書く。
yamy 側の正本は mayu のマニュアル（yamy リポジトリ `doc/CUSTOMIZE-ja.html`）、yamy の `readme.txt`、`engine.h` の関数宣言。
機能範囲の出発点は yamy と同等（`yamy-rebuild.md` 参照）。

対応の記号：

- ○ 対応している
- △ 一部だけ対応（備考に差分）
- × 未対応。backlog の ID があれば書く。無いものは着手を決めていない

## 1. 文

`.mayu` は上から下へ読まれ、同じ定義は下にあるものが勝つ。`#` 以降はコメント。大文字小文字は区別しない。行末 `\` で次の行へ続く。

| 文 | yamy の動き | newmayu | 備考 |
|---|---|---|---|
| `include "ファイル"` | その場にファイルを挿入する。ホームディレクトリ（yamy は exe のあるフォルダ）から探す | △ | 読んでいるファイルの場所からの相対パスで探す |
| `define シンボル` | 条件分岐のシンボルを定義する。yamy では設定ダイアログの `-Dシンボル` でも定義できる | ○ | コマンドラインからの定義は無い |
| `if ( [!] シンボル )` … `else` … `endif` | シンボルの有無で読む範囲を切り替える。`elseif` / `elsif` / `elif` / `or` が else-if、`and` が入れ子の if | ○ | |
| `def key 名前… = [E0-\|E1-]スキャンコード…` | 物理キーを定義する。スキャンコードは複数並べられる（`def key Pause = E1-0x1d 0x45`） | △ | スキャンコードは 1 個だけ。複数は feature-2。yamy の readme は「E1 プレフィックスのキー（Pause）は置き換えられない」としている |
| `def mod (shift\|alt\|control\|windows) = キー名…` | 物理モディファイヤキーを定義する。alt は meta / menu、control は ctrl、windows は win とも書ける | ○ | |
| `def sync = スキャンコード` | `&Sync` で Windows に送るキー | ○ | |
| `def alias 別名 = キー名` | キーの別名。既存のキー名と同じなら別名が勝つ | ○ | |
| `def subst キー = キー…` | キーマップで変換する前に入力キーを置き換える。右側は先頭のキーだけが効く | × | |
| `def option KL- = enable` | フォーカス移動時に IME を on/off して、カナロックの状態を正しく取る | × | |
| `def option delay-of !!! = N` | `!!!` のキーリピートが始まるまで無視するリピート回数 | × | `!!!` 自体が未対応 |
| `def option sts4mayu = enable` / `cts4mayu` | タッチパッドの `T-` `TS-` 修飾を有効にする（別 DLL が要る） | × | |
| `def option mouse-event = true` | yamy 固有。マウス操作を E1- の擬似スキャンコード（Drag E1-0x00、LButton E1-0x01、RButton 0x02、MButton 0x03、WheelForward 0x04、WheelBackward 0x05、XButton1 0x06、XButton2 0x07、TiltRight 0x08、TiltLeft 0x09）のキーとして割り付けられるようにする | × | |
| `def option drag-threshold = N` | yamy 固有。Drag 擬似イベントが起きるまでの移動量（ピクセル）。0 か未指定なら Drag は起きない | × | |
| `keymap 名前 [: 親] [= 動作…]` | キーマップを宣言し、以降の `key` / `event` / `mod` をそこに入れる。`=` は割付の無いキーの既定動作（既定は `&KeymapParent`、親が無ければ `&Default`）。同じ名前を何度宣言してもよいが、親を矛盾させてはいけない | ○ | 矛盾した親はエラーにしていない（yamy も黙って最初の親を使う） |
| `keymap2 名前 [: 親] [= 動作…]` | keymap と同じ。既定動作だけ `&Undefined` | ○ | |
| `window 名前 /クラス/ [: 親] [= 動作…]` | 窓のクラス名列（`exe のパス:クラス:…:クラス`）が正規表現に一致する窓で使うキーマップ。`( /クラス/ && /タイトル/ )` は両方、`\|\|` はどちらか。正規表現は `/…/` か `\m@…@` | △ | `\m@…@` の形は読めない。正規表現は Rust の regex（yamy は Boost.Regex） |
| 初期キーマップ | 先頭に `window Global ( // \|\| // ) = &OtherWindowClass` が隠れている | ○ | |
| `key 修飾キー… = 動作…` | 割付。左側は複数のキーを並べられる（同じ動作を割り付ける） | ○ | |
| `key 修飾… =` | 既定修飾の変更（`key *IC- =` でこれ以降の `IC-` を dontcare にする）。複数変えるときは 1 行に書く | × | |
| `event prefixed \| before-key-down \| after-key-up = 動作…` | イベントが起きたときの動作。キーマップごとで、親のものは見ない | ○ | |
| `mod モディファイヤ (=\|+=\|-=) [!\|!!\|!!!]キー名…` | キーマップごとにモディファイヤキーを変える。親から引き継ぐ。名前は shift / alt / control / windows / mod0〜mod9 | △ | `!`（真のモディファイヤ）`!!`（One Shot、SandS）`!!!`（One Shot＋リピート）は未対応 |
| `mod !モディファイヤ` / `mod !!モディファイヤ` | そのモディファイヤに割り当てた全キーを真 / One Shot にする | × | |
| `keyseq $名前 = 動作…` | 動作列に名前を付ける。使う側で展開される | ○ | |

## 2. key の左側（入力の条件）

既定は `~C-~M-~S-*NL-*CL-*SL-*KL-*IL-~IC-*MAX-*MIN-*MMAX-*MMIN-*T-*TS-`。`*` はどちらでもよい、`~` は離している、何も付けなければ押している。キー名の直前に `*` を書くと未指定の全部が dontcare になる。

| 修飾 | 意味 | newmayu | 備考 |
|---|---|---|---|
| `C-` `S-` `M-`/`A-` `W-` | Control / Shift / Alt / Windows | ○ | |
| `M0-`〜`M9-` | mayu 内部だけのモディファイヤ（`mod mod0 = Up` で割り当てる）。既定は dontcare | ○ | |
| `L0-`〜`L9-` | mayu 内部だけのロック。`&Toggle(Lock0)` で切り替える | × | `&Toggle` も未対応 |
| `NL-` `CL-` `SL-` `KL-` | NumLock / CapsLock / ScrollLock / カナロック | ○ | |
| `IL-` | IME がオン | ○ | |
| `IC-` / `I-` | IME で変換中 | △ | IMM32 のアプリでは取れる。TSF だけのアプリは feature-7 |
| `MAX-` `MIN-` | 窓が最大化 / 最小化されている | × | |
| `MMAX-` `MMIN-` | MDI 子窓が最大化 / 最小化されている | × | |
| `T-` `TS-` | タッチパッドに指が触れている（TS- は全キーを離すまで続く） | × | |
| `D-` `U-` | 押したとき / 離したとき。既定は `*D-*U-`（両方） | ○ | |
| `R-` | キーリピートで来た押し | × | |

## 3. key の右側（動作列）

| 書き方 | yamy の動き | newmayu | 備考 |
|---|---|---|---|
| `[修飾]キー名` | そのキーを押して離す。`D-` だけなら押すだけ、`U-` なら離すだけ。修飾は押す前に合わせ、終わったら戻す | ○ | |
| `*` 付きの修飾（`C-*S-B`） | `*` の修飾は入力されたときの状態をそのまま使う | ○ | |
| `$名前` | keyseq の展開 | ○ | |
| `( 動作… )` | 括りの中を 1 つの動作として扱う（`&Repeat` の引数など） | ○ | |
| `&関数(引数…)` | 4 章。引数の `( )` は省略できる | ○ | |
| 関数の前の修飾（`C-&Sync *&WindowMaximize`） | `&Sync` と組むと、関数を実行する間に押しておく / 離しておくモディファイヤを指定できる | × | |
| 引数置換 `$Clipboard` `$WindowClassName` `$WindowTitleName` | 文字列引数に、実行時のクリップボード / フォーカス窓のクラス名 / タイトルを入れる | × | 文字列引数を取る関数が無い |
| 文字列 `"…"` のエスケープ | `\a \e \f \n \r \t \v \' \" \\ \cX \xXXXX \0XXXX` | × | 同上 |

## 4. &関数

引数の既定値は yamy の `engine.h` の宣言どおり。`対象` は省略すると最上位の窓、`MDI` で MDI 子窓。

### キーマップの引き直し・制御

| 関数 | yamy の動き | newmayu | 備考 |
|---|---|---|---|
| `&Default` | 入力されたキーをそのまま Windows へ | ○ | |
| `&KeymapParent` | 親キーマップで引き直す | ○ | |
| `&KeymapWindow` | いまの窓に一致する最初の window キーマップで引き直す | ○ | |
| `&KeymapPrevPrefix([N])` | `&Prefix` で来る前のキーマップ（N 段前）で引き直す | × | |
| `&OtherWindowClass` | いまの窓に一致する次の window キーマップで引き直す。無ければ `&Default` | ○ | |
| `&Keymap(名前)` | 指定のキーマップで引き直す | × | |
| `&Prefix(名前 [, ignore_modifiers])` | 次のキーを指定のキーマップで解釈する（2 ストローク）。ignore_modifiers（既定 true）なら遷移先では全モディファイヤを真のモディファイヤ扱い | △ | 第 2 引数は未対応 |
| `&CancelPrefix` | yamy 固有。プレフィックス状態を解除する | × | |
| `&Sync` | 出したキーがフォーカス窓で処理されるまで待つ（最長 5 秒）。コンソール窓では待たない | ○ | |
| `&Toggle(LockN [, on\|off])` | ロック L0〜L9 を切り替える / 強制する | × | |
| `&EditNextModifier(修飾)` | 次に入力するキーにその修飾が押されていることにする | × | |
| `&Variable(mag, inc)` | 内部変数を mag 倍して inc を足す | ○ | |
| `&Repeat(動作列 [, 最大])` | 内部変数の回数だけ動作列を実行する。最大は既定 10 | ○ | |
| `&Undefined` | 割付が無いことにしてベルを鳴らす | ○ | |
| `&Ignore` | 何もしない | ○ | |
| `&Wait(ミリ秒)` | 実行を止める（最長 5 秒）。その間キーは入力できない | × | |
| `&VK(仮想キー)` | 仮想キーを入力する。`E-` で拡張、`D-` `U-` で押す / 離す。LButton などでマウスボタンも | ○ | |

### 窓

| 関数 | yamy の動き | newmayu | 備考 |
|---|---|---|---|
| `&WindowRaise([対象])` / `&WindowLower` | Z 順の先頭 / 末尾へ | ○ | |
| `&WindowMinimize` / `&WindowMaximize` / `&WindowHMaximize` / `&WindowVMaximize` `([対象])` | 最小化 / 最大化 / 横だけ / 縦だけ。もう一度で戻す | ○ | |
| `&WindowMove(dx, dy [, 対象])` | 相対移動 | ○ | |
| `&WindowMoveTo(向き, dx, dy [, 対象])` | 向き（C N E W S NE NW SE SW）を基準にして dx, dy | ○ | |
| `&WindowClingToLeft` / `Right` / `Top` / `Bottom` `([対象])` | 画面の端に寄せる（`&WindowMoveTo(W\|E\|N\|S, 0, 0)`） | ○ | |
| `&WindowMoveVisibly([対象])` | 画面からはみ出た分を戻す | ○ | |
| `&WindowMonitor(monitor [, adjust_position, adjust_size])` | 別のモニタへ移す。`&WindowMonitorTo(primary, …)` と同じ | × | |
| `&WindowMonitorTo(primary\|current, monitor [, adjust_position, adjust_size])` | 基準モニタから数えて monitor 番目のモニタへ移す。はみ出すなら位置（既定 true）と大きさ（既定 false）を合わせる | × | |
| `&WindowResizeTo(幅, 高さ [, 対象])` | 大きさを変える。0 はそのまま、負は画面からの差 | ○ | |
| `&WindowClose([対象])` | 閉じる | ○ | |
| `&WindowToggleTopMost` | 最前面フラグを切り替える | ○ | |
| `&WindowSetAlpha(alpha)` | 半透明化の切り替え（0 透明〜100 不透明）。-1 で全部戻す | ○ | |
| `&WindowRedraw` | 再描画 | ○ | |
| `&WindowIdentify` | クラス名・タイトル・位置と大きさをログに出す | × | feature-5 |
| `&SetForegroundWindow(/クラス/ [, &&\|\|\|, /タイトル/])` | yamy 固有。一致する窓を前面にする | × | |
| `&PostMessage(宛先, message, wParam, lParam)` | 窓にメッセージを送る。宛先は ToItself / ToMainWindow / ToOverlappedWindow / ToParentWindow / 正の数（N 代上の親） | △ | 数での指定は未対応 |

### マウス・クリップボード・IME

| 関数 | yamy の動き | newmayu | 備考 |
|---|---|---|---|
| `&MouseMove(dx, dy)` | カーソルの相対移動 | ○ | |
| `&MouseWheel(delta)` | ホイールを回す（120 で 1 単位） | × | |
| `&MouseHook(type, parameter)` | マウスの動きをホイール（Wheel）や窓の移動（WindowMove）に変える。None で戻す | × | |
| `&ClipboardCopy(文字列)` | 文字列をクリップボードへ | × | |
| `&ClipboardUpcaseWord` / `&ClipboardDowncaseWord` | クリップボードの文字列を大文字 / 小文字に | ○ | |
| `&SetImeStatus([on\|off\|toggle])` | IME のオン・オフ | × | |
| `&SetImeString(文字列)` | IME 経由で文字列を入力する | × | |
| `&EmacsEditKillLineFunc` / `&EmacsEditKillLinePred(動作列1, 動作列2)` | emacs の kill-line を Edit / IE の中で実現する一組（マニュアルに手順の説明がある） | × | feature-6 |
| `&Recenter` | Edit / RichEdit でキャレット行を縦の中央に | × | feature-6 |

### 本体の UI・外部連携

| 関数 | yamy の動き | newmayu | 備考 |
|---|---|---|---|
| `&HelpMessage([title, message])` | トレイ付近にメッセージを出す。引数なしで消す | × | feature-4。出し方は yamy と異なる（6 章） |
| `&HelpVariable(title)` | 内部変数の値を title と一緒に出す | × | feature-4。出し方は 6 章 |
| `&DescribeBindings` | いまのキーマップの割付を表示する | × | |
| `&InvestigateCommand` | 窓に届く WM_COMMAND / WM_SYSCOMMAND をログに出すモードの切り替え | × | feature-5 |
| `&LoadSetting([設定名])` | 設定を読み直す | × | feature-5 |
| `&MayuDialog(Investigate\|Log, HIDE\|SHOW\|SHOWNA…)` | 調査 / ログのダイアログを出す・隠す | × | |
| `&LogClear` | ログを消す | × | |
| `&ShellExecute(operation, file, parameters, directory, show_command)` | プログラムやファイルを開く | × | |
| `&DirectSSTP(/名前/, protocol [, header…])` | Direct SSTP でゴーストに送る | × | |
| `&PlugIn(DLL名 [, 関数名, 引数, runAsThread])` | Plugins フォルダの DLL の関数を呼ぶ | × | |

## 5. 本体（.mayu の外）

| 項目 | yamy | newmayu | 備考 |
|---|---|---|---|
| 設定ファイルの場所 | exe と同じフォルダの `.mayu`（mayu はホームディレクトリ） | △ | 起動引数でパスを渡す |
| 設定の保存先 | `yamy.ini`（mayu はレジストリ） | × | 保存する設定が無い |
| トレイメニュー | 調査 / ログ / 設定 / 選択（キーボード種別）/ 一時停止 / 再読込 / 終了 | △ | 再読込 / 一時停止 / 終了 |
| 調査ダイアログ | 窓のクラス名・タイトルと、押したキーのスキャンコードを見る | × | ログの FOCUS 行で代用 |
| ログダイアログ | 動作のログ | △ | `--log` でファイルへ |
| 32 ビットプロセス | yamyd32.exe を別に起こしてフックを入れる | × | parking lot |
| 画面ロック中 | 置換は働かない。ロック前に押していたキーが押しっぱなしになることがある（私家版は復帰時にモディファイヤを離す） | 未確認 | |
| NLS キーのエスケープ | 半角/全角・英数・ひらがな・無変換は WH_KEYBOARD_LL の前に処理されるため正しくフックできない。Scancode Map で別キーに置き換える回避策（workaround.reg / workaround.mayu）と、動作中だけレジストリを書き換える実験機能（`yamy.ini` の escapeNLSKeys） | × | |

## 6. 案内の窓（&HelpMessage / &HelpVariable）

yamy はトレイ付近に出す。newmayu はトレイ通知を使わず、次の窓で出す。

- 場所：フォーカス窓がある画面の下の中央。
- 出るとき：関数を実行したらすぐ。待ちは入れない。
- 見た目：Windows 11 の音量表示に合わせる。ライト / ダークは OS の設定に従う。背景は透けない。
- 中身：1 行目にタイトル（太字）、その下に本文。`&HelpVariable` は本文が `&Variable` の値。
- 消えるとき：5 秒たったとき、または引数なしの `&HelpMessage`。
- 窓の扱い：タスクバーにも Alt+Tab にも出さない。フォーカスを奪わない。クリックは下の窓へ素通りする。
- ピン：板の上にピンのボタンを置く。ボタンだけはクリックを受け、押してもフォーカスを奪わない。
- ピン留めした窓：
  - その場に残り、時間でも引数なしの `&HelpMessage` でも消えない。
  - 窓全体がクリックを受け、どこを掴んでもドラッグで動かせる。
  - ピンをもう一度押すと消える。
  - 何枚でも残せる。
- ピン留め中に次の案内が来たら、別の窓として下の中央に出す。
