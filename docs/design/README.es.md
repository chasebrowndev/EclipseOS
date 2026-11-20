[English](README.md) | **Español**

# Documentos de diseño

Documentos de nivel 6 (índice de planificación del Vol 1):

| ID | Documento | Estado |
|---|---|---|
| D-01 | [Sistema base](D-01-base-system.md) | escrito 2026-09-18 |
| D-02 | [Repositorio de paquetes y firma](D-02-package-repository.md) | escrito 2026-09-18, reducido |
| D-03 | [Medio de instalación e instalador](D-03-installation-media.md) | escrito 2026-09-18 |
| D-05 | [Userland por defecto](D-05-userland.md) | escrito 2026-09-23, parcial (sin terminal, sin portal) |
| D-08 | [Portal de escritorio (`ecl-desktop-portal`)](D-08-portal.md) | escrito 2026-11-02, solo especificación |
| D-07 | [Primer arranque: instalador gráfico y configuración](D-07-first-run.md) | escrito 2026-09-24, solo especificación |
| DP-4 | [Perfiles de escritorio (modo WM y modo DE)](DP-4-desktop-profiles.md) | revisado 2026-09-11, plan de trabajo |

## Referencia de diseño incluida

`eclipse-panes.html` es una copia del lienzo de Claude Design para los paneles de
Eclipse. Trae su propio `./support.js`, que **no** está incluido y no es
necesario: el archivo es una referencia de *lectura*, y el marcado más los estilos en línea
son la carga útil. Abrirlo en un navegador renderizará los tableros de forma estática
y registrará un error de script faltante; eso es lo esperado.

Cinco tableros, referenciados en todo `docs/COMPOSITION.md`:
`4a` Display, `4b` Sound, `4c` Notifications, `4d` Power,
`4e` Privacy & security.

La especificación de estilo que lo acompaña **no** se incluyó aquí — es idéntica byte a byte
a `docs/STYLE.md`, que es la copia que el código y `tokens.rs` citan.
