<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ISO test harness (headless QEMU)

Drives the EclipseOS live ISO with scripted input so the graphical installer (D-07)
can be tested without a display. Python 3 stdlib only. It **never builds an ISO** and
**never touches host processes**: it records the QEMU pid it spawned in `<work>/qemu.pid`
and only ever signals that pid, after re-checking `/proc/<pid>/cmdline` names its own QMP
socket. No `pkill`/`killall`/`pkill -f` anywhere.

## Prerequisites (Arch)

- `qemu-system-x86_64`, `qemu-img` (package `qemu-base`/`qemu-full`)
- OVMF: `edk2-ovmf` (`/usr/share/edk2/x64/OVMF_CODE.4m.fd` + `OVMF_VARS.4m.fd`; other common
  paths are probed; override with `OVMF_CODE=` and `OVMF_VARS=`)
- `/dev/kvm` read/write for speed; otherwise it falls back to TCG (about 4x slower, default
  timeouts scale accordingly). `--tcg` forces it.
- `python3`. Pillow, `magick`, `pnmtopng` are not needed (PPM to PNG is done in pure Python).
- Optional: `shellcheck` for `run.sh`.

## Files

| file | role |
|---|---|
| `run.sh` | wrapper: `run.sh [--work DIR] <cmd>`, plus `unit` and `compare` shortcuts |
| `vm.py` | VM lifecycle, QMP client, input, screendump, waits, `assert-no-keystroke`, `selftest` |
| `compare.py` | `compare.py A.png B.png [--tolerance N] [--max-frac F] [--max-mean M] [--ignore x,y,w,h]` |
| `imgtools.py` | PPM/PNG codec, diff, screen heuristics |
| `test_unit.py` | QEMU-free tests (fake QMP socket): `run.sh unit` |

## Work dir

`--work DIR`, else `$ECLIPSEOS_VM_WORK`, else `$TMPDIR/eclipseos-vm-<uid>`. Holds `disk.qcow2`
(20G), `OVMF_VARS.fd` (copied per work dir), `serial.log`, `qemu.log`, `qemu.pid`, `shots/`.
A work dir inside a git checkout is refused. The QMP socket lives there too, or under
`$XDG_RUNTIME_DIR/evm-<hash>/` when the path would exceed the 108-byte unix socket limit.

The disk and vars persist between `up` runs (installed system survives a reboot test);
`up --fresh` resets them. `up` without `--iso` boots the disk.

## Usage

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

`compare`/`wait-image` semantics: `--tolerance` is the per-channel delta (0..255) under which a
pixel counts as equal; `--max-frac` is the allowed fraction of differing pixels; `--max-mean`
optionally caps the mean absolute channel difference. Output reports `mean_abs` and `frac_diff`.
`--ignore x,y,w,h` (repeatable) masks regions (clock, animation, spinner). Size mismatch is
always a difference. Exit codes: 0 match, 1 differ, 2 error.

Do not type real secrets: `type` text appears in argv. Use throwaway test passwords. The
harness never logs typed text.

## Zero-keystroke gate

```
# record a reference once (from a known-good boot), then assert against it
./run.sh --work $W assert-no-keystroke path.iso --record ref/welcome.png
./run.sh --work $W assert-no-keystroke path.iso --ref ref/welcome.png [--ignore x,y,w,h] [--timeout 300]
./run.sh --work $W assert-no-keystroke path.iso                # no ref: heuristic only
```

It boots fresh, locks the input path in code (any key/type/click raises), polls the screen and
passes after `--settle` consecutive good polls. Good means: matches the reference within
tolerance, or (no reference) non-blank, not a text console, and not thrashing. On timeout it
fails and saves `shots/failure.png`; exit 0 pass, 1 fail, 2 error. The VM is stopped afterwards
unless `--keep`. The bootloader countdown must expire on its own, which is the point.

## What it can verify

- The ISO boots under UEFI and the screen ends up looking like the reference (pixel-level).
- Scripted keyboard and absolute-pointer input reaches the guest (virtio-tablet, PS/2 keyboard).
- A run reaches "not a text console" without any input; serial console output is captured.
- Regression of screens against recorded references, with tolerance and masks.

## What it cannot verify

- Screen contents semantically: there is no OCR. The `login:` check is a heuristic (sparse,
  uncoloured glyphs on black) plus reference matching; a reference is the reliable signal. A
  `login:` on the serial log is reported as a note only (archiso does not put a getty there).
- Anything that needs a real network, real disks of other sizes, GPU acceleration, Secure Boot
  (plain non-secboot OVMF is used), audio, or BIOS boot.
- Animated screens match only within tolerance or with `--ignore` masks.
- TCG runs are slow enough that the timeouts may need raising.
- Pointer events only reach a compositor that reads the virtio-tablet (kernel `virtio_input`).

## Self-test

`./run.sh selftest` boots OVMF with no disk and no ISO (512 MB), checks QMP, that the screendump
is a valid non-empty PNG, waits for stability, sends `esc`, typed text and a pointer click, then
stops QEMU by pid and confirms it is gone. Screens are left in `<work>/shots/`.
