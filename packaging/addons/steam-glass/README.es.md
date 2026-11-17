<!-- SPDX-License-Identifier: AGPL-3.0-only -->
[English](README.md) | **Español**

# steam-glass

El tema de vidrio de Eclipse para la interfaz de escritorio de Steam. Un complemento (ADR 0066) sin
hook del host: un manifiesto, un proceso inyector y dos recursos.

| Pieza | Dónde |
|---|---|
| Manifiesto | `packaging/addons/steam-glass.kdl` |
| Inyector | `shell/crates/ec-steam-glass` (`ec-steam-glass`, unidad de usuario `ec-steam-glass.service`) |
| Capa base | `steam-glass.css`: variables estables, barras de desplazamiento, sin fondo opaco |
| Recoloreado | `steam-glass.js`: reescribe las superficies con colores fijos de Steam según el color |

## Cómo funciona

La interfaz de cliente de Steam es una aplicación de Chromium Embedded Framework. Steam abre
un puerto de DevTools en `127.0.0.1:8080` al iniciar cuando
existe `~/.steam/steam/.cef-enable-remote-debugging` (el inyector lo crea;
surte efecto la próxima vez que se inicie Steam). El inyector mantiene una conexión
por cada página de Steam, registra el bundle para cada documento nuevo y lo ejecuta en el
actual. Editar los recursos bajo `--assets DIR` los vuelve a aplicar en vivo.

Solo se tocan las páginas propias de Steam: las ventanas `about:blank?createflags=` y
`steamloopback.host`. Las páginas de la tienda y de la comunidad son sitios de otras personas.

Las clases de Steam están hasheadas y cambian en cada versión, así que `steam-glass.js`
se basa en **valores de color** (`#0e141b`, `#171d25`, `#23262e`, `#1a9fff`, ...): recorre
cada hoja de estilos y, por cada regla que pinta uno, añade una regla con el mismo selector
con el equivalente en vidrio. Los grises de superficie se vuelven tintes de vidrio ahumado y
pasos de blanco con alfa de `docs/STYLE.md`; el acento azul de Steam se vuelve dorado,
usado solo para el estado.

## La ventana necesita desenfoque de abyss

El tema deja píxeles transparentes; el compositor dibuja el vidrio detrás de ellos.
Steam es un cliente XWayland, así que se necesita una regla de ventana si tu configuración desactiva
el desenfoque para las ventanas X11:

```kdl
windowrule "blur glass" { app-id "^steam$"; title "^Steam$"; }
```

Haz coincidir la ventana principal por título: los popups de Steam (Lista de amigos, Ajustes) son
ventanas separadas y flotan.

## Desarrollo

```
cargo build -p ec-steam-glass          # in shell/
target/debug/ec-steam-glass --assets packaging/addons/steam-glass
```

Ejecútalo contra un Steam en una sesión anidada headless oculta, nunca la sesión en vivo.
