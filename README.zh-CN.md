# Oxeye

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md)

一个用 Rust 与 egui 构建的轻量桌面看图与编辑工具。它专注于快速浏览文件夹、显示大图、轻量裁剪/旋转/翻转，以及简洁的跨平台界面。

## 功能

- 快速浏览文件夹，底部带预览条。
- 以虚拟文件夹方式读取 CBZ/ZIP、CBR/RAR 和 CB7/7z，无需解压。
- 大图通过分块显示，保留原始细节。
- 缩放、平移、适应窗口、原始尺寸、90° 旋转和全屏。
- 轻量非破坏编辑：裁剪（含比例预设）、旋转、翻转与撤销；写盘前先预览结果。
- 覆盖原文件前必须显式确认；也可用“另存为”写出 PNG/JPEG/WebP/BMP。写入原子化，失败不会破坏原文件。
- 将当前图片复制到剪贴板。
- 从右到左的双页书本模式，支持自动宽页检测和 `W` 手动覆盖。
- 在压缩包中记录并恢复阅读进度。
- 预载相邻图片，切换更顺畅。
- 简洁清爽的界面。

## 编辑与保存

- 可编辑单张静态 PNG、JPEG、WebP、BMP，包括压缩包成员（只能另存为）。GIF、TIFF、ICO 保持只读；Oxeye 会直接拒绝编辑，而不是把动画或多页文件静默压成单帧。
- 按 `C` 进入编辑；当前显示旋转会在进入时烘焙一次，退出编辑会恢复原图与方向。裁剪比例预设：自由、1:1、4:3、3:2、16:9、原图。
- `Ctrl`/`Cmd`+`S` 在确认框（显示完整路径与目标格式）之后覆盖原文件；`Ctrl`/`Cmd`+`Shift`+`S` 另存为。未知扩展名会报错，不会猜测格式。
- 覆盖写盘使用同目录临时文件 + `fsync` + 原子 rename；取消或失败时原文件字节与权限不变。
- EXIF、ICC/XMP 与动画不会保留。JPEG 会有损重编码（默认质量 90）；WebP 只能无损写出，文件可能变大。

## 路线图

0.3.0 已发布轻量编辑、原子覆盖与另存为。仍在计划中：模板拼图与动画 GIF 编辑。详见 [docs/roadmap.md](docs/roadmap.md)。

## 下载

安装包见 [GitHub Releases](https://github.com/wang1zhen/oxeye/releases)。
Windows MSI 未签名，可能触发 SmartScreen。macOS DMG 仅作 ad-hoc 签名，未经公证；
若被 Gatekeeper 拦截，请右键选择“打开”，或在系统设置中允许。
Linux x86_64 以 Ubuntu 22.04 / glibc 2.35 为最低基线；AppImage 仍需主机提供
OpenGL/EGL、X11/Wayland 库及桌面 portal。
第三方声明：[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

## 快速开始

```sh
cargo run --release --locked -- path/to/image.png
```

不带路径时会打开空窗口。可以用 Open 按钮、`Ctrl`/`Cmd`+`O`，或把文件拖进窗口。

## 格式

- 编辑后可写出：PNG、JPEG、WebP、BMP。
- 只读：GIF（动画）、TIFF、ICO。本版不能编辑或覆盖这些格式；动画 GIF 编辑不会静默降级为单帧。
- 压缩包：CBZ/ZIP、CBR/RAR 和 CB7/7z 按虚拟文件夹读取，不释放到磁盘。成员可通过“另存为”导出编辑结果，但不能原地覆盖。
- 固实 RAR/7z 页面可能需要先解码前面的页面。阅读进度按压缩包路径、长度和修改时间生成指纹；若替换后二者不变，可能无法识别。

## 操作

| 操作 | 按键 |
| --- | --- |
| 打开 | `Ctrl`+`O` / `Cmd`+`O` |
| 上一张 / 下一张 | `PgUp` / `PgDn` |
| 前后跳 10 个文件 | `Up` / `Down` |
| 切换书本模式 | `M` |
| 切换宽页 | `W` |
| 适应窗口 / 原始尺寸 | `1` 或 `9` / `0` |
| 缩放 | `+`、`-` 或滚轮 |
| 复制图片 | `Ctrl`+`C` / `Cmd`+`C` |
| 旋转 | `Ctrl`+`R` / `Ctrl`+`L`（macOS 用 `Cmd`）；查看时为显示旋转，编辑时为图像旋转 |
| 进入编辑并裁剪 | `C` |
| 应用 / 取消裁剪 | `Enter` / `Esc` |
| 水平 / 垂直翻转 | `X` / `Y` |
| 撤销编辑 | `Ctrl`+`Z` / `Cmd`+`Z` |
| 保存（覆盖，需确认） | `Ctrl`+`S` / `Cmd`+`S` |
| 另存为 | `Ctrl`+`Shift`+`S` / `Cmd`+`Shift`+`S` |
| 全屏 | `F11` 或 `Alt`+`Enter` |
| 设置 | `Ctrl`+`,` / `Cmd`+`,` |
| 退出 | `Ctrl`+`W` / `Cmd`+`W` |

`Esc` 依次关闭设置、取消裁剪选区、退出编辑、取消排队导航、退出全屏。完整快捷键见 [docs/development.md](docs/development.md#keyboard-map)。

## 构建

需要 Rust 1.95 或更新版本；已在 Rust 1.99 下测试。Oxeye 支持 Linux、macOS 和 Windows，CI 已在三个平台通过。平台依赖与构建详情见 [docs/development.md](docs/development.md#platform-prerequisites)。
