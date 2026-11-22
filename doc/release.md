# 配布

インストーラー版（MSI）と zip 版を配る。中身は同じ。

## ツール

- インストーラーは WiX v7（MSI）。`wix build` で直接作る（公開版の cargo-wix は WiX v3 にしか対応していない）。
- 要るもの：.NET SDK、WiX v7（`dotnet tool install --global wix`、`wix eula accept wix7`）、WiX の Util 拡張（`wix extension add -g WixToolset.Util.wixext/7.0.1`。newmayu を終わらせるのに使う）。

## ライセンス

修正 BSD。`LICENSE` に newmayu の表示と、yamy・mayu の表示と条文を入れる。

## 中身

yamy と同じセット。

| yamy | newmayu |
|---|---|
| `yamy.exe`、`yamy64`、`yamy64.dll` | `newmayu.exe`、`newmayu_hook.dll` |
| `yamy32`、`yamy32.dll`、`yamyd32` | 無し（32 ビット用は parking lot） |
| .mayu 8 本（104・104on109・109・109on104・default・dot・emacsedit・workaround） | そのまま |
| `yamy.ini` | そのまま |
| `workaround.reg` | そのまま |
| `readme.txt` | newmayu の readme と `LICENSE` |

## インストーラー

- 入れ先：ユーザーごと（`%LOCALAPPDATA%\Programs\newmayu`）。管理者の権限は要らない。
- 起動：スタートメニューのショートカットと、ログオン時の自動起動（`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`）を必ず登録する。アンインストールで消す。
- 更新：動いている newmayu を終わらせてから入れ替え、入れ終わったら起動し直す。ほかのアプリに読み込まれたままのフック DLL は、Windows Installer の使用中ファイルの扱いに任せ、再起動は強制しない。
- 設定：`yamy.ini` は exe と同じフォルダ。前の版から残っていれば上書きせず、アンインストールでも消さない。自分の `.mayu` はこのフォルダにコピーして使う。

## 作り方

`powershell -ExecutionPolicy Bypass -File installer\package.ps1` で、リリースビルドから `target\dist` に `newmayu-<版>-x64.msi` と `newmayu-<版>-x64.zip` を作る。版は `crates\newmayu\Cargo.toml` の `version`。

- インストーラーの中身は `installer\main.wxs`。yamy から持ってきた同梱のファイルと readme は `installer\files\`。
- zip は `newmayu-<版>-x64` フォルダに、インストーラーと同じファイルを入れる。

## 公開

GitHub のリリースに MSI と zip を添付して配る。

1. `crates\newmayu\Cargo.toml` の `version` を上げてコミットする。
2. `installer\package.ps1` で MSI と zip を作る。
3. そのコミットに注釈付きのタグ `v<版>` を付けて push する（`git tag -a v<版> -m "newmayu <版>"`、`git push origin v<版>`）。
4. `gh release create v<版> target/dist/newmayu-<版>-x64.msi target/dist/newmayu-<版>-x64.zip --title "newmayu <版>" --notes-file <本文のファイル> --verify-tag` で公開する。プレリリースにするときは `--prerelease` を付ける。
