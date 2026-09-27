# EclipseOS welcome screen: fonts

Put these files in `fonts/` next to `index.html`. If a font is already installed system-wide on the ISO, the page uses that copy and the file is optional.

All are free under the SIL Open Font License (download from Google Fonts or the Noto project).

| File | Family / weight | Covers | Used for |
| --- | --- | --- | --- |
| `fonts/CormorantGaramond-Light.woff2` | Cormorant Garamond 300 | Latin, Latin Extended, Cyrillic | Welcome, Bienvenue, Willkommen, Bienvenido, Benvenuto, Bem-vindo, Добро пожаловать |
| `fonts/Lora-Regular.woff2` | Lora 400 | Latin, Cyrillic | Version label |
| `fonts/NotoSerifJP-Light.woff2` | Noto Serif JP 300 | Japanese | ようこそ |
| `fonts/NotoSerifSC-Light.woff2` | Noto Serif SC 300 | Simplified Chinese | 欢迎 |
| `fonts/NotoSerifKR-Light.woff2` | Noto Serif KR 300 | Korean | 환영합니다 |

## Notes
- Chinese and Japanese need separate fonts: the same characters take different shapes in each, and 欢 exists only in Simplified Chinese.
- CJK fonts are 10–25 MB each. To save ISO space, subset them to the glyphs used (e.g. `pyftsubset NotoSerifJP-Light.otf --text="ようこそ" --flavor=woff2`).
- If you add languages, add the matching Noto Serif family (Arabic, Devanagari, Thai, Hebrew…) and a line in `FONT` inside `index.html`.

## Folder layout
```
eclipseos-welcome/
  index.html
  assets/eclipseos-logo.png
  fonts/  (the five .woff2 files above)
```
