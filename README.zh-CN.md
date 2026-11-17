# Oxeye

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md)

一个用 Rust 与 egui 构建的轻量桌面看图工具。它专注于快速浏览文件夹、显示大图，以及简洁的跨平台界面。轻量编辑功能已在计划中。

## 功能

- 快速浏览文件夹，底部带预览条。
- 大图通过分块显示，保留原始细节。
- 缩放、平移、适应窗口、原始尺寸、90° 旋转和全屏。
- 将当前图片复制到剪贴板。
- 从右到左的双页书本模式。
- 预载相邻图片，切换更顺畅。
- 简洁清爽的界面。

## 路线图

计划中、尚未实现：轻量编辑功能，包括裁剪、保存旋转结果和多图拼贴。

## 快速开始

```sh
cargo run --release --locked -- path/to/image.png
```

不带路径时会打开空窗口。可以用 Open 按钮、`Ctrl`/`Cmd`+`O`，或把文件拖进窗口。

## 操作

| 操作 | 按键 |
| --- | --- |
| 打开 | `Ctrl`+`O` / `Cmd`+`O` |
| 上一张 / 下一张 | `PgUp` / `PgDn` |
| 前后跳 10 个文件 | `Up` / `Down` |
| 切换书本模式 | `M` |
| 适应窗口 / 原始尺寸 | `1` 或 `9` / `0` |
| 缩放 | `+`、`-` 或滚轮 |
| 复制图片 | `Ctrl`+`C` / `Cmd`+`C` |
| 旋转（仅显示） | `Ctrl`+`R` / `Ctrl`+`L`（macOS 用 `Cmd`） |
| 全屏 | `F11` 或 `Alt`+`Enter` |
| 设置 | `Ctrl`+`,` / `Cmd`+`,` |
| 退出 | `Ctrl`+`W` / `Cmd`+`W` |

完整快捷键见 [docs/development.md](docs/development.md#keyboard-map)。

## 构建

需要 Rust 1.95 或更新版本；已在 Rust 1.99 下测试。Oxeye 支持 Linux、macOS 和 Windows，CI 已在三个平台通过。平台依赖与构建详情见 [docs/development.md](docs/development.md#platform-prerequisites)。
