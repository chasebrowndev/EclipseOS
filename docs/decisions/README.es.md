[English](README.md) | **Español**

# Registro de decisiones (F-08)

Una decisión por archivo, `NNNN-slug.md`, formato en `TEMPLATE.md`. **Nunca se edita
tras su aceptación salvo para cambiar el estado** — reemplazar una decisión significa
escribir un nuevo ADR que lo indique.

| # | Decisión | Estado |
|---|---|---|
| 0001 | Rust como lenguaje de implementación | aceptada (aún sin redactar) |
| 0002 | [Compositor propio sobre Smithay, fijado en `=0.7.0`](0002-smithay-not-a-fork.md) | aceptada |
| 0003 | Protocolo Wayland propio para el control de agentes, no libei/portal | aceptada (aún sin redactar) |
| 0004 | Superficie de agentes compatible con MCP desde el primer día | aceptada (aún sin redactar) |
| 0005 | [AGPLv3 + SDK Apache-2.0, CLA, licencia comercial dual](0005-agpl-plus-commercial.md) | aceptada |
| 0006 | Sin ISO primero; primero el compositor | aceptada (aún sin redactar) |
| 0007 | [Seats de agentes por defecto, compat lock como respaldo](0007-agent-seats-default.md) | aceptada |
| 0008 | [Política en el proceso a partir de una tabla compilada; defer solo restringe](0008-policy-in-process-table.md) | aceptada |
| 0009 | [Interfaz confiable dibujada por el compositor, nunca layer-shell](0009-trusted-ui-compositor-drawn.md) | aceptada |
| 0010 | Solo modo WM, sin DE en v1 | reemplazada por 0038 |
| 0011 | UX humana al estilo Hyprland; espacios de trabajo de agentes sin mosaico | aceptada (aún sin redactar) |
| 0012 | Sensibilidad privada por defecto; la clase declarada por la app solo puede subir | aceptada (aún sin redactar) |
| 0013 | Las pulsaciones humanas nunca se registran por contenido | aceptada (aún sin redactar) |
| 0014 | Colaboración entre agentes mediante canales con procedencia no falsificable | aceptada (aún sin redactar) |
| 0015 | `registryd` fuera de la TCB; nunca recibe superficies secretas | aceptada (aún sin redactar) |
| 0016 | [El formato de configuración es KDL](0016-kdl-config.md) | aceptada |
| 0017 | [Base de GPU con NVIDIA primero; sincronización explícita obligatoria](0017-nvidia-first-gpu-baseline.md) | aceptada |
| 0018 | [Núcleo de un solo hilo que posee `AbyssState`](0018-single-threaded-core.md) | aceptada |
| 0019 | [Foco que sigue al mouse por defecto](0019-focus-follows-mouse-default.md) | aceptada |
| 0020 | [Atajo de salida provisional `Super+Shift+Q`](0020-quit-binding-super-shift-q.md) | aceptada |
| 0021 | [Árbol binario en arena por espacio de trabajo para el mosaico](0021-layout-tree.md) | aceptada |
| 0022 | [`wlr_data_control` detrás de una lista de permitidos por nombre de proceso](0022-data-control-allowlist.md) | aceptada |
| 0023 | [Identidad de salida por EDID, disposiciones persistidas por conjunto de salidas](0023-output-identity-and-persistence.md) | aceptada |
| 0024 | [La caída de un bloqueador deja la sesión bloqueada tras un respaldo del compositor](0024-session-lock-fallback.md) | aceptada |
| 0025 | [El scanout directo se desactiva en cualquier fotograma que contenga una superficie sensible](0025-direct-scanout-and-redaction.md) | aceptada |
| 0026 | [XWayland es un único dominio de confianza no confiable, su portapapeles una concesión limitada al foco](0026-xwayland-trust-domain.md) | aceptada |
| 0027 | [La captura de pantalla se deniega salvo que el cliente esté en una lista de permitidos por nombre de proceso](0027-screencopy-fail-closed.md) | aceptada |
| 0028 | [El socket de control es solo del propietario, y los títulos de ventana lo cruzan](0028-control-socket-authority.md) | aceptada |
| 0029 | [`xdg-desktop-portal-wlr` lo permite la persona usuaria, nunca por defecto](0029-portal-capture-allowlist.md) | aceptada |
| 0030 | [Dos protocolos de captura coexisten, detrás de una compuerta compartida](0030-ext-image-copy-capture.md) | aceptada |
| 0031 | [Redimensionar una ventana mediante el socket de control](0031-resize-over-the-control-socket.md) | aceptada |
| 0032 | [El traspaso de sesión de systemd vive en el compositor, detrás de `--session`](0032-session-handoff-in-compositor.md) | aceptada |
| 0033 | [Un render-device explícito que no se resuelve se niega a iniciar](0033-render-device-override-refuses-on-miss.md) | aceptada |
| 0034 | [Emulador de terminal: conservar el fork de foot, extraer el publicador como `cataclysm-pub`](0034-terminal-foot-fork-publisher-crate.md) | reemplazada por 0109 |
| 0035 | [CI desde el primer día: compuerta solo en la nube, `cargo-deny` provisional, reglas de revisión mecanizadas](0035-ci-harness-day-one.md) | aceptada |
| 0036 | [Las escrituras de configuración empalman bytes en el archivo; el modelo de documento KDL no](0036-config-writes-byte-splice.md) | aceptada |
| 0037 | [La superficie de seguridad pasa a su propio archivo, `policy.kdl`](0037-abyss-kdl-policy-kdl-split.md) | aceptada |
| 0038 | [El userland del DE es Rust nativo, no Quickshell](0038-de-userland-is-native-rust.md) | aceptada |
| 0039 | [Excepciones de cadena de suministro para el toolkit iced](0039-iced-supply-chain-exceptions.md) | aceptada |
| 0040 | [Las superposiciones de anotaciones son un pase propio del compositor, no interfaz confiable](0040-annotation-overlay-pass.md) | aceptada |
| 0041 | [Oracle-Eyes se ejecuta fuera del proceso, con dos capacidades revocables](0041-oracle-eyes-out-of-process.md) | aceptada |
| 0042 | [La salida enfocada es de primera clase; el foco del teclado se deriva de ella](0042-focused-output-and-pointer-focus.md) | aceptada |
| 0043 | [El arnés de benchmarks es artesanal, no criterion](0043-hand-rolled-bench-harness.md) | aceptada |
| 0044 | [Un perfil CBOR canónico, artesanal, para concesiones, auditoría y sockets](0044-canonical-cbor-profile.md) | aceptada |
| 0045 | [Las concesiones son COSE_Sign1 sobre CBOR canónico, Ed25519, carga útil embebida](0045-grant-signing-cose-sign1.md) | aceptada |
| 0046 | [El almacén de auditoría S-04 §4 es el journal de tareas/contadores, construido en el hito 10](0046-shared-audit-store-at-m10.md) | aceptada |
| 0047 | [La superficie de API de `ec-policy-eval`: lo que los dos procesos pueden compartir](0047-policy-eval-api-surface.md) | aceptada |
| 0048 | [El enunciado de una tarea se registra como un hash y no sobrevive a un reinicio](0048-task-statement-is-hashed-not-journalled.md) | aceptada |
| 0049 | [Las salidas obtienen un número de pantalla estable; acción de mover ventana al espacio de trabajo de la salida](0049-output-number-and-move-to-output.md) | aceptada |
| 0050 | [Oracle-Eyes puede llamar a `get_outputs`](0050-oracle-eyes-reads-get-outputs.md) | aceptada |
| 0051 | [Un servicio `org.freedesktop.ScreenSaver` conecta los bloqueos de inactividad de D-Bus con el compositor](0051-screensaver-service-bridges-idle-inhibit.md) | aceptada |
| 0052 | [Un crate y un paquete por cada componente intercambiable del DE](0052-one-crate-per-swappable-de-component.md) | aceptada |
| 0053 | [El servicio de estado gana acciones; los secretos pasan por su propio proceso de aviso](0053-status-actions-and-secret-prompt.md) | aceptada |
| 0054 | [Las anotaciones ganan un título y un marcador de selección](0054-annotation-pick-marker.md) | aceptada |
| 0055 | [Oracle-Eyes publica una baliza de estado para el ojo de la barra de tareas](0055-oracle-eyes-status-beacon.md) | aceptada |
| 0056 | [Las superficies de capa pequeñas listadas se omiten de la captura](0056-capture-hide-layer.md) | aceptada |
| 0057 | [`mousebind`: mover/redimensionar ventanas con modificador + botón del mouse](0057-mousebind-alt-drag.md) | aceptada |
| 0058 | [Radiant: mosaico n-ario ponderado con zonas de soltado](0058-radiant-layout.md) | aceptada |
| 0059 | [`gesture "drag"`: arrastrar ventanas con modificador + touchpad](0059-touchpad-drag-gesture.md) | aceptada |
| 0060 | [Perfiles de instalación y el instalador ISO gráfico](0060-setup-profiles.md) | aceptada |
| 0061 | [Confirmación de acciones destructivas y su canal de solicitud](0061-trusted-destructive-confirm.md) | aceptada |
| 0062 | [Modo de interacción híbrido](0062-hybrid-interaction-mode.md) | aceptada |
| 0063 | [`agentd` es el único emisor confiable de eventos de actividad de archivos de agentes](0063-agent-file-activity.md) | propuesta |
| 0064 | [Una configuración inválida al iniciar descarta los nodos incorrectos y arranca](0064-start-on-invalid-config.md) | aceptada |
| 0065 | [Widgets de la barra de tareas, movimiento fluido de la barra y la toma del monitor de audio](0065-taskbar-widgets.md) | aceptada |
| 0066 | [Complementos: un solo tipo, un manifiesto y hooks que permanecen apagados](0066-addons.md) | aceptada |
| 0067 | [Los widgets de comandos solo se ejecutan cuando la persona propietaria los aprueba en la interfaz confiable](0067-command-approval.md) | aceptada |
| 0068 | [El fondo de pantalla es un daemon central en la capa Background](0068-wallpaper-daemon.md) | aceptada |
| 0069 | [La pila de agentes es un solo complemento](0069-agent-stack-addon.md) | aceptada |
| 0070 | [Un repositorio paraguas, un workspace por producto, nombres `ec-`](0070-umbrella-repo-and-ec-naming.md) | aceptada |
| 0071 | [HUD de anotaciones: presentación de vidrio líquido, colores por propietario, tipo de error](0071-annotation-hud-presentation.md) | aceptada |
| 0074 | [Aplicaciones fijadas en la barra de tareas](0074-taskbar-pinned-apps.md) | aceptada |
| 0072 | [Los ajustes de Oracle Eyes viven en abyss.kdl; un comando de modelo modificado necesita aprobación](0072-oracle-eyes-settings-in-abyss-kdl.md) | aceptada |
| 0073 | [Sistema de animación del compositor: eventos, preajustes, fantasmas, shaders de transición](0073-compositor-animation-system.md) | aceptada |
| 0075 | [brokerd se ejecuta como servicio de usuario con un almacén por usuario](0075-brokerd-user-service.md) | aceptada |
| 0076 | [Enrutador de inferencia con un backend de API y un backend de Claude Code](0076-inference-router.md) | aceptada |
| 0077 | [Cuentas por tarea, un inicio de sesión de Claude Code capturado y el aviso del almacén](0077-per-task-accounts.md) | aceptada |
| 0078 | [XWayland usa por defecto el escalado del cliente](0078-xwayland-client-scaling-default.md) | aceptada |
| 0079 | [Ventanas de marco de smithay de profundidad 32 para clientes X11 ARGB](0079-xwayland-argb-frame.md) | aceptada |
| 0079 | [Glass for Firefox](0079-firefox-glass-addon.md) | aceptada (tres ADRs llegaron como 0079 el mismo día; se conservan los números para que las citas sigan siendo válidas) |
| 0079 | [El calendario de la barra lee eventos de un proveedor en el bus de sesión](0079-calendar-provider.md) | aceptada |
| 0080 | [Vidrio por clave para clientes opacos](0080-keyed-glass.md) | aceptada |
| 0081 | [Paneles de vidrio dibujados por el compositor para las celdas del DE](0081-glass-panes.md) | propuesta |
| 0082 | [Oracle Eyes: pantallas de varias preguntas, contexto continuo y modos de modelo e imagen](0082-oracle-eyes-multi-question-context.md) | propuesta |
| 0083 | [El HUD de anotaciones dibuja latín y griego](0083-annotation-extended-glyphs.md) | propuesta |
| 0084 | [Temas: paletas derivadas, oscuro y claro como preajustes integrados](0084-themes.md) | aceptada |
| 0084 | [GPU híbrida: cada GPU compone sus propias salidas, con un modo conmutable](0084-hybrid-gpu-per-gpu-compositing.md) | aceptada (cláusula por GPU reemplazada por 0086); 0084 se comparte con Temas, se conservan los números para que las citas sigan siendo válidas |
| 0085 | [Actualización y energía: la actualización completa del widget de energía](0085-update-and-power.md) | propuesta |
| 0086 | [Las salidas en una GPU secundaria obtienen una copia por defecto; la composición nativa es opcional](0086-secondary-gpu-copy-default.md) | aceptada |
| 0087 | [Abyss aloja el greeter](0087-abyss-hosts-the-greeter.md) | aceptada |
| 0088 | [El compositor recoge las contraseñas de polkit](0088-compositor-polkit-prompt.md) | aceptada |
| 0089 | [La máquina elige su pantalla de inicio de sesión](0089-login-screen-settings.md) | aceptada |
| 0090 | [Modo DE: un escritorio apilado con cromo dibujado por el compositor](0090-de-mode-compositor-chrome.md) | aceptada |
| 0091 | [Las notificaciones tienen modos, un historial y un widget](0091-notification-modes-and-history.md) | aceptada |
| 0092 | [Vidrio adaptativo: el tinte sigue al fondo](0092-adaptive-glass.md) | aceptada |
| 0093 | [El compositor toma sus propias capturas de pantalla](0093-built-in-screenshot.md) | aceptada |
| 0094 | [Los globales de seguridad admiten por ruta del ejecutable](0094-global-admission-by-executable-path.md) (enmienda 0022, 0027) | propuesta |
| 0095 | [ec-lock: el bloqueador de pantalla es un cliente, con PAM dentro](0095-ec-lock-client-locker.md) | aceptada |
| 0096 | [`ecl-desktop-portal`: un portal de escritorio propio, de reemplazo directo](0096-ecl-desktop-portal.md) | propuesta |
| 0097 | [Concesiones de captura con alcance y una confirmación de captura dibujada por el compositor](0097-scoped-capture-grant-and-trusted-confirm.md) | aceptada (enmendada 2026-11-12) |
| 0098 | [La TCB de políticas sale del uid del propietario](0098-policy-tcb-out-of-the-owner-uid.md) | propuesta |
| 0099 | [Un bloqueador mantiene la sesión, uno caído se relanza y la inactividad puede suspender](0099-session-lock-hardening-and-idle-suspend.md) | propuesta |
| 0100 | [El selector de archivos del portal es ec-portal manejando el selector de Fog](0100-portal-file-chooser.md) | propuesta |
| 0101 | [Una mala actualización de Abyss vuelve a la compilación anterior](0101-update-rollback.md) | propuesta |
| 0102 | [Contención de pánicos para funciones no esenciales](0102-panic-containment.md) | propuesta |
| 0103 | [Un workspace y un lockfile para el SO](0103-one-workspace-one-lockfile.md) | propuesta |
| 0104 | [Memoria de los paneles del shell: medir, recortar, luego decidir](0104-shell-process-consolidation.md) | propuesta |
| 0105 | [Hilo de render: primero medir, luego una regla numérica](0105-render-thread-and-frame-baselines.md) | propuesta |
| 0106 | [Pantalla compartida redactada como función del escritorio](0106-redacted-screen-sharing.md) | propuesta |
| 0107 | [La presentación de una clave de configuración vive junto a la fila de su esquema](0107-config-schema-single-source.md) | propuesta |
| 0108 | [El compositor es dueño de la tapa; logind solo suspende](0108-compositor-owns-the-lid.md) | aceptada |
| 0109 | [Apotheosis: un terminal propio reemplaza al fork de foot](0109-apotheosis-supersedes-foot-fork.md) | aceptada |
| 0110 | [Regla de lenguaje de Apotheosis y lista de permitidos de bibliotecas del sistema](0110-apotheosis-language-rule.md) | aceptada |
| 0111 | [Apotheosis escribe su propio analizador y su propia cuadrícula](0111-apotheosis-own-parser-and-grid.md) | propuesta |
| 0112 | [El estado del arte se estudia, nunca se copia](0112-apotheosis-prior-art-procedure.md) | aceptada |
| 0113 | [`ec-config-schema`: un segundo producto usa los tipos del esquema; ec-settings edita el archivo del terminal directamente](0113-config-schema-second-source.md) | propuesta |
| 0114 | [`ec-cataclysm-pub` pasa a ser `ec-term-pub`; se elimina la ABI de C](0114-ec-term-pub-rename.md) | propuesta |
| 0115 | [La señal de credencial es ECHO apagado e ICANON encendido](0115-credential-signal-echo-off-icanon-on.md) | aceptada |
| 0116 | [El socket de control del terminal nunca elude la política de seats](0116-apotheosis-control-socket-vs-seat-policy.md) | propuesta |
| 0117 | [`apotheosis` se une a la clase de aplicación terminal en la política por defecto](0117-terminal-app-class-in-policy.md) | propuesta |
| 0118 | [Medios: tres niveles, audio sobre el protocolo PulseAudio, nasm en tiempo de compilación](0118-apotheosis-media-scope.md) | aceptada |
| 0119 | [Presentador: solo la superficie wgpu](0119-apotheosis-presenter-wgpu-surface.md) | propuesta |
| 0120 | [Un proceso por instancia por defecto; el daemon es opcional](0120-apotheosis-single-process.md) | propuesta |
| 0121 | [La persistencia del historial y el volcado a disco están apagados por defecto, cifrados y atentos a la sensibilidad](0121-apotheosis-scrollback-persistence.md) | propuesta |
| 0122 | [Sin scripting embebido ni entorno de plugins](0122-apotheosis-no-embedded-scripting.md) | propuesta |
| 0123 | [Sincronización del mux por instantánea más réplica del flujo de bytes](0123-apotheosis-mux-sync.md) | propuesta |
| 0124 | [`TERM=xterm-apotheosis`](0124-apotheosis-term-name.md) | propuesta |
| 0125 | [Backend XKB en Rust puro con libxkbcommon como función opcional y oráculo de pruebas](0125-apotheosis-xkb-backend.md) | propuesta |
| 0126 | [Un greeter en iced, una disposición en mosaico y Glances](0126-glance-greeter.md) | propuesta |

Los números 0001–0015 son la lista inicial de F-08 §"Seeded ADRs"; los marcados
como "aún sin redactar" están decididos en la especificación y reservados aquí para que
la numeración se mantenga estable.
