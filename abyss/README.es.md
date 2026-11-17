[English](README.md) | **Español**

# abyss — el compositor de EclipseOS

`abyss` es un compositor Wayland escrito en Rust sobre
[Smithay](https://github.com/Smithay/smithay) (fijado en `=0.7.0`). Es el
gestor de ventanas de EclipseOS y la mitad de su base de cómputo confiable: los agentes
manejan el escritorio mediante un protocolo nativo del compositor, bajo una verificación
de política que se ejecuta antes de que cambie cualquier estado.

**Estado: Fase 1, previa a la salida.** Tres backends (winit, DRM/udev, headless), el
conjunto estándar de protocolos, espacios de trabajo, reglas de ventanas y efectos, configuración KDL con
recarga en caliente y el socket IPC COMP-13. El 2026-09-10 el backend DRM arrancó sobre
KMS real en un equipo NVIDIA, encendió dos paneles en sus modos nativos y ejecutó un cliente
en vivo. Todavía no es apto para el uso diario — consulta [docs/STATUS.md](docs/STATUS.md) para ver qué
está verificado, qué es un stub y qué aún bloquea la salida de la Fase 1.

## Qué lo hace diferente

- **Los agentes tienen sus propios seats de Wayland.** La entrada de un agente nunca compite ni le
  roba el foco a la persona, y cada evento es atribuible a un principal (ADR 0007).
- **Sin autoridad ambiental.** Cada operación de un agente pasa por una verificación de capacidad
  contra una tabla de políticas compilada, dentro del proceso, con falla cerrada (ADR 0008).
- **La interfaz confiable la dibuja el compositor**, por encima de todo cliente, de modo que los avisos
  de consentimiento y el indicador de actividad no pueden suplantarse (ADR 0009).
- **La entrada humana nunca se registra por contenido.** Ni en ningún nivel de log, ni en el
  registro de auditoría.

## Inicio rápido

```
cargo build --workspace
cargo run -- --backend winit     # nested in your current session
cargo run -- --backend drm       # from a bare VT, real KMS
cargo run -- --backend headless  # no display; what wlcs drives
journalctl --user -t ec-abyss -f    # logs
```

`Super+Shift+Q` sale. Instrucciones completas y dependencias del sistema:
[docs/BUILDING.md](docs/BUILDING.md).

## Documentación

| | |
|---|---|
| [docs/STATUS.md](docs/STATUS.md) | qué está construido, qué es un stub, qué está verificado |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | disposición de crates, mapa de módulos, límites de confianza |
| [docs/BUILDING.md](docs/BUILDING.md) | dependencias, compilación, ejecución, logs |
| [docs/internal/HANDOFF.md](docs/internal/HANDOFF.md) | registro continuo de sesiones: qué cambió, qué se rompió, trampas |
| [decisions/](decisions/) | registros de decisiones de arquitectura |
| [CLAUDE.md](CLAUDE.md) | invariantes (tanto para humanos como para asistentes de IA) |
| `ECLIPSEOS_SPECS_v2_VOL1.md` | el paquete de especificación; autoritativo |

## Licencia

El código del sistema es **AGPL-3.0-only** ([LICENSE](LICENSE)). Las definiciones de protocolos y
los SDK de agentes son **Apache-2.0** ([LICENSE-APACHE](LICENSE-APACHE)) para que los agentes
puedan escribirse bajo cualquier licencia. Hay una licencia comercial disponible para
organizaciones que no pueden cumplir con la AGPL — consulta
[CONTRIBUTING.md](CONTRIBUTING.md) y [NOTICE](NOTICE). El nombre y el logotipo no
están licenciados por la AGPL: [TRADEMARK.md](TRADEMARK.md).
