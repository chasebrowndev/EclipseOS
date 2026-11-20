# EclipseOS welcome screen: fonts

Onest is the one face (SIL OFL). The native screen embeds `assets/fonts/Onest-{Light,Regular}.ttf`; the
HTML reference takes `fonts/Onest-{Light,Regular}.woff2` (convert the same files) or a system copy.
Onest covers Latin, Latin Extended and Cyrillic only, so the three CJK greetings use Noto Sans CJK
(Arch `noto-fonts-cjk`).

| File | Family / weight | Covers | Used for |
| --- | --- | --- | --- |
| `fonts/Onest-Light.woff2` | Onest 300 | Latin, Latin Extended, Cyrillic | Welcome, Bienvenue, Willkommen, Bienvenido, Benvenuto, Bem-vindo, Добро пожаловать |
| `fonts/Onest-Regular.woff2` | Onest 400 | Latin, Cyrillic | Version label |
| `fonts/NotoSansCJKJP-Light.woff2` | Noto Sans CJK JP 300 | Japanese | ようこそ |
| `fonts/NotoSansCJKSC-Light.woff2` | Noto Sans CJK SC 300 | Simplified Chinese | 欢迎 |
| `fonts/NotoSansCJKKR-Light.woff2` | Noto Sans CJK KR 300 | Korean | 환영합니다 |

## Notes
- Chinese and Japanese need separate fonts: the same characters take different shapes in each, and 欢 exists only in Simplified Chinese.
- CJK fonts are 10-25 MB each. To save ISO space, subset them to the glyphs used (e.g. `pyftsubset NotoSansCJKJP-Light.otf --text="ようこそ" --flavor=woff2`).
- Greek, Arabic, Hebrew, Devanagari and Thai are not in Onest: if you add such a language, add the matching Noto Sans family and a line in `FONT` inside `index.html`.
