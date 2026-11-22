# newmayu

Windows のキーカスタマイズソフト「窓使いの憂鬱（mayu）」と「YAMY」を、Rust で作り直したものです。
mayu / yamy の設定ファイル（`.mayu`）を、そのまま使えます。

## mayu と yamy への敬意

- 窓使いの憂鬱（mayu）は、TAGA Nayuta さんが作ったキーカスタマイズソフトです。`.mayu` という設定言語で、キーの置き換え・修飾キー・前置キー・ウィンドウごとのキーマップを書けます。
- YAMY（Yet Another Mado tsukai no Yuutsu）は、KOBAYASHI Yoshiaki さんが mayu を元に、ドライバを使わずフックで動くようにしたものです。newmayu が直接の手本にしているのは、ampmmn さんの YAMY 私家版です。

newmayu は、この 2 つが作ってきた設定言語と動きを受け継ぎます。`.mayu` の文法・`&関数`・キーの判定の仕方は、mayu / yamy のマニュアルとソースを読み、同じ動きになるように作っています。同梱の `.mayu` は yamy のものをそのまま使い、両者の著作権表示を [LICENSE](LICENSE) に残しています。

## 作り直すにあたっての方針

詳しくは [doc/principle.md](doc/principle.md)。

1. mayu / yamy 互換を何より大事にする。勝手に動きを変えない。
2. アーキテクチャとビルドツールを新しくする。古い Visual Studio が無いとビルドできない状態をなくすことが、作り直す動機です。
3. AI に対応する。開発に AI を使えるようにし、`.mayu` の設定用に AI 向けのスキルやマニュアルを同梱する。

## yamy との違い

[doc/spec.md](doc/spec.md) に、mayu / yamy の構文と `&関数` を 1 行ずつ並べ、newmayu の対応（○ 対応・△ 一部・× 未対応）と yamy との違いを書いています。

- 未対応の機能と今後の作業は [doc/backlog.md](doc/backlog.md)。
- 32 ビットのアプリには、まだフックが入りません。そのアプリでは IME の状態とフォーカスの変化が取れません。

## 使い方

インストーラー版（MSI）と zip 版があります。中身は同じです。

- 起動するとタスクトレイに常駐します。トレイのメニューから、再読込・設定の切り替え・設定・一時停止・ログ・終了ができます。
- 設定の一覧（`yamy.ini`）と `.mayu` は、`newmayu.exe` と同じフォルダから読みます（yamy と同じ）。自分の `.mayu` を使うときは、このフォルダに置いてください。
- インストーラー版はユーザーごとに入れ（`%LOCALAPPDATA%\Programs\newmayu`）、ログオン時に起動します。

## ビルド

- Rust（edition 2024）。`cargo build --workspace` でビルド、`cargo test --workspace` でテストします。
- 構成：本体 `crates/newmayu/`、フック DLL `crates/hook/`、両者の通知の形 `crates/ipc/`。
- newmayu を動かした後は、ほかのアプリに読み込まれた `newmayu_hook.dll` がしばらく残り、ビルドが上書きできずに失敗します。名前を変えて退けて（例：`newmayu_hook.dll.old`）から `cargo build --workspace` でビルドし直してください。`--workspace` を付けないと DLL が作り直されません。
- 配布物（MSI と zip）の作り方は [doc/release.md](doc/release.md)。

## ドキュメント

- [doc/principle.md](doc/principle.md) — 基本方針
- [doc/spec.md](doc/spec.md) — 外部仕様（`.mayu` の構文と `&関数`、newmayu の対応と yamy との違い）
- [doc/test.md](doc/test.md) — テスト方針
- [doc/backlog.md](doc/backlog.md) — 未完了の作業
- [doc/release.md](doc/release.md) — 配布

## ライセンス

修正 BSD ライセンスです。[LICENSE](LICENSE) に、newmayu と、yamy・mayu の著作権表示と条文があります。
