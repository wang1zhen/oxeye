# Oxeye

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md)

Rust と egui で作られた軽量なデスクトップ画像ビューアーです。フォルダーの素早い閲覧、大きな画像の表示、シンプルでクロスプラットフォームな UI を重視しています。軽量な編集機能を計画中です。

## 機能

- 下部のプレビュー付きでフォルダー内をすばやく閲覧。
- 大きな画像もタイル表示で元のディテールを保って表示。
- ズーム、パン、ウィンドウに合わせる、原寸表示、90° 回転、全画面。
- 現在の画像をクリップボードへコピー。
- 右から左に進む見開きの Book モード。
- 隣接画像のプリロードで切り替えを滑らかに。
- すっきりとしたシンプルな UI。

## ロードマップ

計画中・未実装：トリミング、回転結果の保存、複数画像のコラージュなど、軽量な編集機能。詳細なロードマップと現在の状況は [docs/roadmap.md](docs/roadmap.md) を参照してください。

## ダウンロード

インストーラーは [GitHub Releases](https://github.com/wang1zhen/oxeye/releases) にあります。
Windows MSI は未署名のため SmartScreen が警告する場合があります。macOS DMG は
ad-hoc 署名のみで未公証です。Gatekeeper に拒否された場合は右クリックの「開く」、
またはシステム設定から許可してください。Linux x86_64 は Ubuntu 22.04 / glibc 2.35
以降が基準です。AppImage にもホスト側の OpenGL/EGL、X11/Wayland ライブラリと
デスクトップ portal が必要です。

## クイックスタート

```sh
cargo run --release --locked -- path/to/image.png
```

パスを指定しない場合は空のウィンドウが開きます。Open ボタン、`Ctrl`/`Cmd`+`O`、またはファイルのドラッグ＆ドロップで開けます。

## 操作

| 操作 | キー |
| --- | --- |
| 開く | `Ctrl`+`O` / `Cmd`+`O` |
| 前 / 次 | `PgUp` / `PgDn` |
| 10 ファイル移動 | `Up` / `Down` |
| Book モード切替 | `M` |
| フィット / 原寸 | `1` または `9` / `0` |
| ズーム | `+`、`-`、またはホイール |
| 画像をコピー | `Ctrl`+`C` / `Cmd`+`C` |
| 回転（表示のみ） | `Ctrl`+`R` / `Ctrl`+`L`（macOS は `Cmd`） |
| 全画面 | `F11` または `Alt`+`Enter` |
| 設定 | `Ctrl`+`,` / `Cmd`+`,` |
| 終了 | `Ctrl`+`W` / `Cmd`+`W` |

詳しいキー操作は [docs/development.md](docs/development.md#keyboard-map) を参照してください。

## ビルド

Rust 1.95 以降が必要です（Rust 1.99 で検証済み）。Linux、macOS、Windows に対応し、3 プラットフォームで CI が通っています。プラットフォーム別の依存パッケージとビルド手順は [docs/development.md](docs/development.md#platform-prerequisites) にあります。
