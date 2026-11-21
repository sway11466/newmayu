# CLAUDE.md

newmayu — キーカスタマイズソフト mayu/yamy を Rust で作り直すプロジェクト。基本方針 → [doc/principle.md](doc/principle.md)

## 開発上の制約（重要）

- 実装着手はオーナーの明示的な許可後。設計・ドキュメント作業は随時OK。
- git 操作は都度許可を取る。
- ブランチは main。
- セッション開始時にリモートとの差分を確認する。main ブランチの場合、`git fetch origin main` で差分があればオーナーに報告する。
- mayu/yamy の挙動を勝手に変えない。yamy と違う挙動にしたくなったら、先にオーナーに聞く。

## 説明の仕方

話を勝手に広げない。オーナーからの質問には端的に答え、話を広げる必要がある場合は事前に許可を取る。

オーナーが分かっている前提で話を進めない。専門用語・略号は、初めて出すときにそれが何を指すのかを説明してから使う。プログラミングは説明なしで専門用語を使ってよい。

## 技術スタック

- 言語: Rust（edition 2024）。Win32 API は `windows` クレート。
- 構成: Cargo ワークスペース。パッケージは `crates/` に並べる。
  - `crates/newmayu/` — 本体。フックを張り、`.mayu` の設定でキーを差し替え、タスクトレイに常駐する。
  - `crates/hook/` — フック DLL（yamy の hook.cpp に当たる）。各 GUI プロセスに読み込まれ、IME・フォーカスの変化を本体へ通知する。
  - `crates/ipc/` — フック DLL から本体へ届く通知の形（メールスロット）。DLL と本体の両方が使う。

## ドキュメント

- [doc/principle.md](doc/principle.md) — 基本方針（mayu/yamy 互換・アーキテクチャとビルドツールを新しくする・AI 対応）
- [doc/spec.md](doc/spec.md) — 外部仕様（.mayu の構文と &関数。各行に newmayu の対応）
- [doc/test.md](doc/test.md) — テスト方針（目的・TDD・互換のテスト・テストサイズ・テストしやすい設計・運用）
- [doc/backlog.md](doc/backlog.md) — 未完了の作業（バグ・機能追加・リファクタリング）
- [doc/release.md](doc/release.md) — 配布（インストーラー・zip・ライセンス・中身）
