//! `.mayu` を読み込んで、キーマップごとの修飾キーと割付数を表示する。
//!
//! ```text
//! cargo run --example check -- path/to/dot.mayu
//! ```

use std::path::Path;
use std::process::ExitCode;

use newmayu::config::parser;
use newmayu::model::BasicMod;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("使い方: check <file.mayu>");
        return ExitCode::FAILURE;
    };
    let config = match parser::load(Path::new(&path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    for (i, km) in config.keymaps.iter().enumerate() {
        let parent = km
            .parent
            .map(|p| config.keymaps[p.0].name.clone())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "keymap {i} {} (parent: {parent}) assignments: {}",
            km.name,
            km.assignments.len()
        );
        for m in BasicMod::ALL {
            let keys = &km.mod_keys[m.index()];
            if !keys.is_empty() {
                println!("  mod {m:?}: {keys:?}");
            }
        }
    }
    ExitCode::SUCCESS
}
