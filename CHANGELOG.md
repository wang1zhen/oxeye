# Changelog

## 0.1.0 — 2026-11-17

- Initial read-only image viewer: folder navigation, preview strip, tiled
  large-image rendering, zoom/pan, display rotation, fullscreen, and clipboard copy.
- Right-to-left Book mode, animated GIF playback, bounded neighbor preloading,
  persistent settings, and English/Simplified Chinese/Japanese UI.
- Installer definitions for Windows x64 MSI, macOS universal DMG, and Linux
  x86_64 deb/AppImage; MIT license.

### Limits

- No editing, saved rotation, automatic updates, or OS file-association setup.
- Windows is unsigned (SmartScreen may warn); macOS is ad-hoc signed, without
  Developer ID or notarization (Gatekeeper may block opening).
- Linux baseline: Ubuntu 22.04 / glibc 2.35. AppImage needs host OpenGL/EGL,
  X11/Wayland libraries, and a desktop portal; it is not a fully bundled runtime.
- Native installer smoke tests passed in the three-platform release dry-run
  (`workflow_dispatch` run 44086841142); artifacts are not publicly released.
