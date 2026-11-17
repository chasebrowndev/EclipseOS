<!-- SPDX-License-Identifier: AGPL-3.0-only -->
[English](README.md) | **Español**

# Arnés de pruebas de la ISO (QEMU headless)

Maneja la ISO live de EclipseOS con entrada con guion para poder probar el instalador gráfico (D-07)
sin una pantalla. Solo la biblioteca estándar de Python 3. **Nunca construye una ISO** y
**nunca toca procesos del host**: registra el pid de QEMU que lanzó en `<work>/qemu.pid`
y solo envía señales a ese pid, tras volver a comprobar que `/proc/<pid>/cmdline` nombra su propio socket
QMP. Ningún `pkill`/`killall`/`pkill -f` en ninguna parte.

## Requisitos previos (Arch)

- `qemu-system-x86_64`, `qemu-img` (paquete `qemu-base`/`qemu-full`)
- OVMF: `edk2-ovmf` (`/usr/share/edk2/x64/OVMF_CODE.4m.fd` + `OVMF_VARS.4m.fd`; se prueban otras
  rutas comunes; se puede sobrescribir con `OVMF_CODE=` y `OVMF_VARS=`)
- `/dev/kvm` con lectura/escritura para mayor velocidad; de lo contrario recurre a TCG (unas 4 veces más lento, los
  tiempos de espera por defecto se escalan en consecuencia). `--tcg` lo fuerza.
- `python3`. Pillow, `magick` y `pnmtopng` no se necesitan (PPM a PNG se hace en Python puro).
- Opcional: `shellcheck` para `run.sh`.

## Archivos

| archivo | función |
|---|---|
| `run.sh` | envoltorio: `run.sh [--work DIR] <cmd>`, más los atajos `unit` y `compare` |
| `vm.py` | ciclo de vida de la VM, cliente QMP, entrada, screendump, esperas, `assert-no-keystroke`, `selftest` |
| `scenario.py` | escenario de instalación de extremo a extremo (`run.sh scenario --iso ...`), ver abajo |
| `compare.py` | `compare.py A.png B.png [--tolerance N] [--max-frac F] [--max-mean M] [--ignore x,y,w,h]` |
| `imgtools.py` | códec PPM/PNG, diff, heurísticas de pantalla |
| `test_unit.py` | pruebas sin QEMU (socket QMP falso): `run.sh unit` |

## Directorio de trabajo

`--work DIR`, o si no `$ECLIPSEOS_VM_WORK`, o si no `$TMPDIR/eclipseos-vm-<uid>`. Contiene `disk.qcow2`
(20G), `OVMF_VARS.fd` (copiado por directorio de trabajo), `serial.log`, `qemu.log`, `qemu.pid`, `shots/`.
Se rechaza un directorio de trabajo dentro de un checkout de git. El socket QMP vive allí también, o bajo
`$XDG_RUNTIME_DIR/evm-<hash>/` cuando la ruta excedería el límite de 108 bytes de los sockets unix.

El disco y las vars persisten entre ejecuciones de `up` (el sistema instalado sobrevive a una prueba de
reinicio); `up --fresh` los restablece. `up` sin `--iso` arranca el disco.

## Uso

```
./run.sh selftest                                  # no ISO needed
./run.sh unit
W=$TMPDIR/vm1
./run.sh --work $W up --iso /var/tmp/eclipseos-out/eclipseos-*.iso --fresh [--res 1280x800]
./run.sh --work $W wait-screen stable --timeout 300 --save $W/s.png
./run.sh --work $W shot out.png
./run.sh --work $W key ret          # names are QEMU qcodes: ret esc tab spc shift+tab ctrl+alt+f2 meta_l
./run.sh --work $W type 'Hello, World!'   # US layout, shift handled; '-' reads stdin
./run.sh --work $W move 640 400 ; ./run.sh --work $W click 640 400 [--button right] [--double]
./run.sh --work $W wait-image ref/welcome.png --timeout 120 --tolerance 12 --max-frac 0.03
./run.sh --work $W record-ref ref/welcome.png       # wait until stable, save reference
./run.sh --work $W down                              # by recorded pid only
./run.sh compare a.png b.png --tolerance 8 --max-frac 0.01
```

Semántica de `compare`/`wait-image`: `--tolerance` es el delta por canal (0..255) por debajo del cual un
píxel cuenta como igual; `--max-frac` es la fracción permitida de píxeles distintos; `--max-mean`
opcionalmente limita la diferencia absoluta media por canal. La salida reporta `mean_abs` y `frac_diff`.
`--ignore x,y,w,h` (repetible) enmascara regiones (reloj, animación, spinner). Una diferencia de tamaño es
siempre una diferencia. Códigos de salida: 0 coincide, 1 difiere, 2 error.

No escribas secretos reales: el texto de `type` aparece en argv. Usa contraseñas desechables de prueba. El
arnés nunca registra el texto escrito.

## Compuerta de cero pulsaciones

```
# record a reference once (from a known-good boot), then assert against it
./run.sh --work $W assert-no-keystroke path.iso --record ref/welcome.png
./run.sh --work $W assert-no-keystroke path.iso --ref ref/welcome.png [--ignore x,y,w,h] [--timeout 300]
./run.sh --work $W assert-no-keystroke path.iso                # no ref: heuristic only
```

Arranca desde cero, bloquea la ruta de entrada en el código (cualquier tecla/type/clic lanza una excepción), sondea la pantalla y
pasa tras `--settle` sondeos buenos consecutivos. Bueno significa: coincide con la referencia dentro de la
tolerancia, o (sin referencia) no está en blanco, no es una consola de texto y no está oscilando. Al agotar el tiempo
falla y guarda `shots/failure.png`; salida 0 pasa, 1 falla, 2 error. La VM se detiene después
salvo con `--keep`. La cuenta regresiva del bootloader debe expirar por sí sola, ese es el punto.

## Qué puede verificar

- La ISO arranca bajo UEFI y la pantalla termina viéndose como la referencia (a nivel de píxel).
- La entrada de teclado con guion y de puntero absoluto llega al invitado (virtio-tablet, teclado PS/2).
- Una ejecución llega a "no es una consola de texto" sin ninguna entrada; se captura la salida de la consola serial.
- Regresión de pantallas contra referencias grabadas, con tolerancia y máscaras.

## Qué no puede verificar

- El contenido de la pantalla de forma semántica: no hay OCR. La comprobación de `login:` es una heurística (glifos
  dispersos y sin color sobre negro) más la coincidencia con la referencia; la referencia es la señal confiable. Un
  `login:` en el log serial se reporta solo como nota (archiso no pone un getty ahí).
- Cualquier cosa que necesite una red real, discos reales de otros tamaños, aceleración por GPU, Secure Boot
  (se usa OVMF normal sin secboot), audio o arranque BIOS.
- Las pantallas animadas solo coinciden dentro de la tolerancia o con máscaras `--ignore`.
- Las ejecuciones con TCG son tan lentas que puede ser necesario aumentar los tiempos de espera.
- Los eventos de puntero solo llegan a un compositor que lea el virtio-tablet (kernel `virtio_input`).

## Autoprueba

`./run.sh selftest` arranca OVMF sin disco ni ISO (512 MB), comprueba QMP, que el screendump
sea un PNG válido y no vacío, espera la estabilidad, envía `esc`, texto escrito y un clic de puntero, luego
detiene QEMU por pid y confirma que ya no está. Las pantallas se dejan en `<work>/shots/`.

## Escenario de instalación de extremo a extremo

```
./run.sh scenario --iso /var/tmp/eclipseos-out/eclipseos-*.iso --work $TMPDIR/e2e \
    [--profile standard] [--disk virtio|nvme] [--disk-size 20G] [--second-disk 8G] [--res 1280x800] \
    [--no-net] [--bios] [--wifi] [--reboot-count N] [--kernels linux,linux-lts] [--keep] [--tcg]
```

Arranca la ISO desde cero, comprueba que la pantalla de bienvenida aparece con cero pulsaciones, luego maneja ec-setup
con el teclado (Space, Enter, Down, Tab; dos clics donde la interfaz no tiene ruta por teclado), confirma
el aviso de borrado del compositor (Tab hasta Erase, Space), espera a que `ec-setup-helper` termine, reinicia
al disco con la ISO expulsada, inicia sesión en el greeter y verifica la salud por SSH. Cada paso se
captura en `<work>/shots/NN-name.png`; el veredicto es `<work>/result.json` (pasa/falla por comprobación) y el
estado de salida (0 pasa, 1 falló una comprobación, 2 error del arnés/de uso).

Límites honestos:
- Sin OCR. "Advanced" significa que la pantalla cambió y se estabilizó. El nombre del disco escrito en Review se
  predice (`virtio-eclipse0`, `nvme-QEMU_NVMe_Ctrl_eclipse0`; se sobrescribe con `--disk-by-id`).
- **ec-setup no tiene paso de acceso remoto**, así que SSH se planta en la instalación terminada desde la shell root de tty2
  de la sesión live (solo con clave, como la opción en `eclipseos-postinstall.sh`). La salida de los comandos
  del lado live regresa por el puerto serial (`/dev/ttyS0` hacia `serial.log`).
- Solo se puede elegir el perfil Standard en ec-setup (`data::selectable`); los demás valores de
  `--profile` se aceptan y se reportan como una comprobación `profile_selectable` fallida sin arrancar.
- `--wifi`: mac80211_hwsim y un AP WPA2 con hostapd/dnsmasq (SSID `eclipse-test`) en un netns, instalado
  desde el mirror por el enlace ethernet SLIRP (que sigue siendo la salida a internet). Prueba la unión del
  asistente y el traspaso del perfil de conexión, no una radio real.
- `--no-net` espera que la instalación se detenga en su primera descarga; una detención limpia se registra, no se afirma
  contra un banner en particular. `--bios` espera que el ayudante se niegue antes de cualquier aviso de borrado.
- Los objetivos de clic son fracciones de pantalla (`CLICK_*` en `scenario.py`), calibrados a 1280x800.
- Las credenciales desechables (`tester` / `eclipse-test-pw`) se escriben mediante `type`, así que aparecen en argv.
- Opciones: `--stop-at SCREEN` termina el recorrido en una pantalla del asistente (se registra como éxito), `--force-profile`
  omite la comprobación de que el perfil sea seleccionable en el asistente, `--boot-entry lts` reinicia por el menú
  de Limine hacia la entrada LTS y comprueba `uname -r`.
- El ritmo de los pasos usa la barra de progreso del asistente como verdad para el índice del paso; BIOS y 2560x1600 reciben
  un ritmo extra porque la pantalla va rezagada respecto de las teclas (unos 3 minutos por paso a 2560x1600).
- La instalación necesita un disco de al menos 16 GiB; se comprueba que los discos más pequeños sean rechazados.
- Se evitan los viajes de ida y vuelta de VT antes de la instalación (la entrada murió tras uno en ISOs sin la corrección
  de reanudación de libinput), así que la fila de Wi-Fi necesita la ISO corregida.
- El repositorio `[eclipseos]` instalado es `chase-pc:8088` (D-03), inalcanzable desde la VM; `pacman -Sy` lo anota.
