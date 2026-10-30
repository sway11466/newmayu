//! newmayu: YAMY（窓使いの憂鬱）の Rust による再構築。
//!
//! - `model`  : キー・修飾・キーマップ・割付のデータ構造（OS 非依存）
//! - `engine` : キー入力を受けて出力を決める状態機械（OS 非依存）
//! - `platform`: Win32 との結線（`SendInput`・フック構造体の変換）
//! - `config` : `.mayu` の読み込み（字句解析・構文解析→ `model::Config`）
//! - `notify` : フック DLL（`newmayu-hook`）の読み込みと、そこから届く通知の受信

pub mod config;
pub mod engine;
pub mod model;
pub mod notify;
pub mod platform;
