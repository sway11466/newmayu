//! newmayu: YAMY（窓使いの憂鬱）の Rust による再構築。
//!
//! - `model`  : キー・修飾・キーマップ・割付のデータ構造（OS 非依存）
//! - `engine` : キー入力を受けて出力を決める状態機械（OS 非依存）
//! - `platform`: Win32 との結線（`SendInput`・フック構造体の変換）
//! - `config` : `.mayu` の読み込み（字句解析・構文解析→ `model::Config`）
//! - `describe`: `&DescribeBindings` の書き出し（yamy の形）
//! - `notify` : フック DLL（`newmayu-hook`）の読み込みと、そこから届く通知の受信
//! - `window` : 窓を操作する `&関数`（`&WindowMove` など）の Win32 側
//! - `log`    : ログ（時刻付き。標準エラーか `--log` のファイル、とログダイアログ）
//! - `setting`: 設定の一覧（`yamy.ini`）
//! - `setting_dialog`: 設定ダイアログ（一覧の追加・編集・削除・並べ替え）
//! - `log_dialog`: ログダイアログ（ログの表示・クリア・フォント・詳細）
//! - `ui`     : タスクトレイのアイコンとメニュー
//! - `investigate`: 調査用の `&関数`（`&WindowIdentify`・`&InvestigateCommand`）の Win32 側
//! - `help`   : 案内の窓（`&HelpMessage` / `&HelpVariable`。yamy と異なる）

pub mod config;
pub mod describe;
mod dialog;
pub mod engine;
pub mod help;
pub mod investigate;
pub mod log;
pub mod log_dialog;
pub mod model;
pub mod notify;
pub mod platform;
pub mod setting;
pub mod setting_dialog;
pub mod ui;
pub mod window;
