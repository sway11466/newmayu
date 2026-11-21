# 配布物（MSI と zip）を作る。使い方は doc/release.md。
#   powershell -ExecutionPolicy Bypass -File wix\package.ps1
# 出力: target\dist\newmayu-<版>-x64.msi と newmayu-<版>-x64.zip
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# 版は Cargo.toml の [package] の version
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$name = "newmayu-$version-x64"
$dist = Join-Path $root 'target\dist'

cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { throw 'cargo build に失敗' }

New-Item -ItemType Directory -Force $dist | Out-Null

$wix = Join-Path $env:USERPROFILE '.dotnet\tools\wix.exe'
& $wix build wix\main.wxs -ext WixToolset.Util.wixext -arch x64 -d "Version=$version" -d BinDir=target\release -o (Join-Path $dist "$name.msi")
if ($LASTEXITCODE -ne 0) { throw 'wix build に失敗' }

# zip はインストーラーと同じファイルを newmayu-<版>-x64 フォルダに入れる
$stage = Join-Path $dist $name
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory $stage | Out-Null
Copy-Item target\release\newmayu.exe, target\release\newmayu_hook.dll, LICENSE $stage
Copy-Item dist\* $stage
$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
# Compress-Archive（PowerShell 5.1）はパスの区切りが \ の zip を作るので、Windows の tar で作る
& (Join-Path $env:SystemRoot 'System32\tar.exe') -a -c -f $zip -C $dist $name
if ($LASTEXITCODE -ne 0) { throw 'zip の作成に失敗' }
Remove-Item -Recurse -Force $stage

Get-ChildItem $dist -Filter "$name.*" | Format-Table Name, Length
