# Oxeye

[English](README.md) | [简体中文](README.zh-CN.md)

Oxeye 是一个轻量的只读桌面看图工具，使用 Rust 与 egui/eframe 构建。它从文件夹中打开图片，绝不会修改原文件。

## 功能

- 单页阅读，或从右到左、成对显示的双页书本布局。
- 大图分块显示，保留原始细节。
- 单页和书本双页都支持 GIF 动画，两页时钟相互独立。
- 按 `P` 播放/暂停 GIF，按 `M` 切换单页/书本模式。
- 缩放、平移、适应窗口、原始尺寸、90° 旋转和全屏。
- 预载相邻图片，底部预览条可点击缩略图跳转。
- 支持英文、简体中文和日文，自动模式跟随系统语言。

## 快速开始

```sh
cargo run --release --locked -- path/to/image.png
```

不带路径时会打开空窗口。可以用 Open 按钮、`Ctrl`/`Cmd`+`O`，或把文件拖进窗口。

## 操作

| 操作 | 按键 |
| --- | --- |
| 打开 | `Ctrl`+`O` / `Cmd`+`O` |
| 上一张 / 下一张 | `PgUp` / `PgDn`（书本模式为上一跨页/下一跨页） |
| 前后跳 10 个文件 | `Up` / `Down`（书本模式保持跨页对齐） |
| 左右键 | 单图模式：可水平平移时平移，否则切换图片。书本模式：始终按设置方向切换跨页。 |
| 空格 | 可向下平移时平移；否则单图下一张/书本下一跨页 |
| 切换单页 / 书本 | `M` |
| 适应窗口 / 原始尺寸 | `1` 或 `9` / `0` |
| 缩放 | `+`、`-`、滚轮，或指针处的 `Ctrl`+滚轮 |
| 顺时针 / 逆时针旋转 | `Ctrl`+`R` / `Ctrl`+`L`（macOS 用 `Cmd`） |
| 全屏 | `F11` 或 `Alt`+`Enter` |
| 播放 / 暂停 GIF | `P` |
| 设置 | `Ctrl`+`,` / `Cmd`+`,` |
| 返回 | `Escape`：先关闭设置，再取消排队导航，最后退出全屏 |
| 退出 | `Ctrl`+`W` / `Cmd`+`W` |
| 平移图片 | 拖拽，或 `Alt`+拖拽 |

浏览会在首尾停止。更多快捷键与别名见 [docs/development.md](docs/development.md#keyboard-map)。

## 书本模式

在设置中选择阅读模式：“Single image”或“Book · right to left”。书本模式下，“Book: next spread”可选择“Right arrow →”（默认）或“← Left arrow”；Apply 保存设置；只有切换阅读模式时才重新加载所属视图。连续页面按从右到左配对：以 6 个文件为例，第 1 页单独显示，然后右侧 2 / 左侧 3，右侧 4 / 左侧 5，最后第 6 页单独显示。从任意入口打开页面都会显示其所属跨页。适应窗口、原始尺寸、缩放、平移和旋转同时作用于两页。GIF 页面在左右两侧独立播放动画。

## 预览条

底部预览条默认显示附近图片的缩略图。点击缩略图即可跳转；当前页或跨页会高亮并自动滚动到可见位置。可用工具栏或设置中的 Hide previews / Show previews 切换。缩略图使用独立的 32 MiB 缓存，尺寸最大 160×120。

## 设置与预载

- 默认向前 5 张、向后 5 张，预算 256 MiB。设置中可调整张数和预算。Apply 立即生效并保存到用户配置目录；Cancel 保持当前设置，保存出错会提示。
- 设置还控制语言（Automatic/English/简体中文/日本語）、阅读模式、书本方向和预览条显示。Automatic 在启动时跟随系统语言。Restore defaults 只修改草稿，Apply 后才生效。
- 邻图按距离由近到远加载。张数和预算会同时限制保留内容。某侧张数为 0 时不预载该侧；预算为 0 时关闭全部预载。
- 预算只计算解码后的像素。当前图片、GPU 纹理和临时解码缓冲不计入。GIF 播放期间会暂停新的预载。

保存成功提示约两秒后自动消失；错误提示会保留到手动关闭。新鲜度判断与完整缓存规则见 [docs/development.md](docs/development.md)。

## 格式

PNG、JPEG、WebP、BMP、ICO 和 TIFF 按静态图片显示。GIF 是唯一支持的动画格式；动画 WebP 只显示第一帧。格式按文件内容判断，但所选路径仍需受支持的扩展名。视频、压缩包和智能跨页识别尚未实现。

## 限制

- 每张图片的编码输入、解码数据和最终输出各最多 256 MiB；源图每边最多 65,535 像素，并受这些字节上限约束。
- 单个 GIF 最多保留 256 MiB 解码帧和 2,048 帧；书本模式同时显示两个 GIF 时，理论上可接近 512 MiB。
- GPU 分块每边不超过 4,096 像素，也不超过渲染器的最大纹理尺寸。
- 预览缩略图使用独立的 32 MiB 缓存，尺寸最大 160×120。
- 预载、预览和单图限制彼此独立，合计仍不是整个进程的内存或 RSS 上限。

## 只读

Oxeye 只读取图片，不保存、重命名、删除或改写文件；旋转不会写入磁盘。

## 运行要求

- Rust 1.95 或更新版本；已在 Rust 1.99 下测试。
- Linux：OpenGL/EGL、Wayland 或 X11，以及桌面门户。
- Windows：MSVC Rust 工具链。
- macOS：Xcode 命令行工具，以及 Apple 已弃用的 OpenGL。

Oxeye 的目标平台是 Linux、macOS 和 Windows。Linux 是主要测试平台；macOS 和 Windows 的 CI 已配置但尚未验证。详情见 [docs/development.md](docs/development.md#platform-prerequisites)。

## 开发

```sh
cargo test --locked --all-targets
```

格式检查、编译检查、测试、Clippy 和发布构建由 [.github/workflows/ci.yml](.github/workflows/ci.yml) 执行。架构和尚未验证的项目见 [docs/development.md](docs/development.md)。

当前验证：200 个测试通过（2 个忽略）；格式检查、编译检查和严格 Clippy 通过；Linux 发布版冒烟测试覆盖了 6720×4480 大图、已填充的预览条，以及书本模式双无限 GIF 播放。Windows 和 macOS CI 尚未运行。
