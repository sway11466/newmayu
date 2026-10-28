//! newmayu: YAMY（窓使いの憂鬱）の Rust による再構築。
//!
//! - `model`  : キー・修飾・キーマップ・割付のデータ構造（OS 非依存）
//! - `engine` : キー入力を受けて出力を決める状態機械（OS 非依存）

pub mod engine;
pub mod model;
