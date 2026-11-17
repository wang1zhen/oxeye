# Oxeye icon

An original, minimal eye mark using the app's blue palette, with a rounded-square
background and transparent outer corners. No text or external artwork.

Regenerate locally with Python 3 and Pillow:

```sh
python3 assets/icons/generate.py
```

`oxeye-1024.png` is the canonical RGBA image. The 512 and 256 PNGs and the
multi-size ICO are derived from the same supersampled drawing. ICO sizes:
16, 32, 48, 64, 128, and 256 pixels. macOS ICNS conversion is left to CI.

The script checks encoded dimensions, RGBA modes, transparent PNG corners,
embedded ICO sizes, and the directory's total size (under 1 MB).
