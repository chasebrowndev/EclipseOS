<!-- SPDX-License-Identifier: AGPL-3.0-only -->
[English](../../browser/firefox-glass/README.md) | **Español**

> Traducción solo para GitHub. Vive aquí, fuera de `browser/firefox-glass/`, porque esa carpeta debe coincidir exactamente con el XPI firmado (ADR 0079).

# Glass for Firefox

Un complemento de EclipseOS (ADR 0066) que hace que Firefox combine con el escritorio: casi negro cálido, acento dorado, vidrio líquido. Firefox en Wayland siempre envía un toplevel opaco y aplana la transparencia del tema, así que la translucidez la hace abyss, basada en el color: la extensión pinta cada superficie de vidrio con un color clave reservado, y abyss vuelve translúcidos exactamente los píxeles de esos colores (con desenfoque detrás). El texto, los iconos y el contenido de la página son de otros colores y permanecen opacos.

No hay reemplazo de la nueva pestaña: la propia página de Inicio de Firefox (páginas recientes, accesos directos, ajustes) está de vuelta. Inicio se tematiza mediante `ntp_background` y `ntp_card_background`, que también son colores clave.

## Vidrio por clave

Cuatro claves, derivadas del tinte en `shared/glass.js` (`keyColors`): vidrio base (marco, tira de pestañas, barra de herramientas, fondo de Inicio), campo de URL y pestaña seleccionada, popups y menús, barra lateral y tarjetas de Inicio. Cada clave es el color deseado desplazado exactamente una unidad en un canal, así que es invisible al ojo pero no es un color que una página pintaría. Las claves se separan al menos 2 unidades para que la tolerancia de redondeo de +-1 de abyss no pueda confundirlas. La clave de popups está marcada como `everywhere` para que abyss también la aplique sobre toda la ventana en sitios web (el menú desplegable de resultados de la barra de URL se dibuja dentro de la ventana); el campo de URL enfocado y ese menú se pintan con ella. Cada clave tiene su propia opacidad: Strength escalado por el control deslizante de la región correspondiente. El Strength por defecto da cerca de 35% en el vidrio base.

## Qué sale del navegador

Mediante native messaging (`os.eclipse.glass`, un puerto persistente; se reconecta con retroceso): los colores clave y las opacidades, y por ventana el tamaño, la altura del área de página de la pestaña activa y un booleano `body` que es verdadero mientras la pestaña activa es about:home, about:newtab, about:blank o about:privatebrowsing (abyss entonces aplica la clave a toda la ventana). El permiso `tabs` se necesita solo para leer la URL en esa prueba; la URL nunca se envía, solo el booleano. Sin títulos. Las actualizaciones se envían en eventos de ventana, pestaña, URL y zoom y con un sondeo cada 1 s para detectar cambios de tamaño, y solo cuando el contenido cambió. La página de opciones muestra si el ayudante está conectado.

Límite: lo que se mide es el área de página, así que una barra de búsqueda inferior o las herramientas de desarrollo acopladas la acortan y la franja crece mientras están abiertas.

Una WebExtension solo puede establecer colores del tema, así que no puede cambiar la geometría, el radio ni la densidad, dibujar detrás de la página ni desenfocar nada. Cárgala con `npx web-ext run -s browser/firefox-glass` (perfil desechable) y revísala con `npx web-ext lint -s browser/firefox-glass`.
