#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
#
# Rootless tests for the ADR 0101 update fallback: ec-abyss-keep (the
# pre-upgrade copy), ec-abyss-launch (the greeter's startup-crash fallback) and
# abyss-session (execs directly; --previous picks the kept build). Each case
# builds a fake system root in a temp dir; nothing outside it is touched.
#
#   packaging/test/update-rollback.sh
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
keep="$here/../bin/ec-abyss-keep"
launch="$here/../bin/ec-abyss-launch"
session="$here/../abyss-session"

tmp=$(mktemp -d)
trap 'chmod -R u+w "$tmp" 2>/dev/null || true; rm -rf "$tmp"' EXIT

pass=0
fail=0
ok() { pass=$((pass + 1)); printf 'ok   %s\n' "$1"; }
no() { fail=$((fail + 1)); printf 'FAIL %s\n' "$1"; }
check() { name=$1; shift; if "$@"; then ok "$name"; else no "$name"; fi; }

# A fake root with an installed abyss of content $1 and pacman version $2.
mkroot() {
    r="$tmp/$3"
    mkdir -p "$r/usr/bin" "$r/usr/lib/eclipse" "$r/proc" "$r/var/lib/pacman/local/eclipseos-abyss-$2"
    printf '#!/bin/sh\n# %s\n' "$1" >"$r/usr/bin/ec-abyss"
    printf '#!/bin/sh\n# ctl %s\n' "$1" >"$r/usr/bin/ec-ctl"
    chmod 0755 "$r/usr/bin/ec-abyss" "$r/usr/bin/ec-ctl"
    printf '%%NAME%%\neclipseos-abyss\n\n%%VERSION%%\n%s\n\n' "$2" \
        >"$r/var/lib/pacman/local/eclipseos-abyss-$2/desc"
    echo "$r"
}
# Pretend pid $2 runs /usr/bin/ec-abyss with argv $3... under root $1.
fakeproc() {
    r=$1 pid=$2
    shift 2
    mkdir -p "$r/proc/$pid"
    ln -s /usr/bin/ec-abyss "$r/proc/$pid/exe"
    printf '%s\000' /usr/bin/ec-abyss "$@" >"$r/proc/$pid/cmdline"
}
kept() { cat "$1/usr/lib/eclipse/previous/ec-abyss" 2>/dev/null | sed -n 2p; }

# --- ec-abyss-keep -----------------------------------------------------------

r=$(mkroot v1 0.1.1-1 fresh)
"$keep" "$r" 2>/dev/null
check "keep: first run copies the current build" [ "$(kept "$r")" = "# v1" ]
check "keep: ec-ctl is kept beside it" grep -q 'ctl v1' "$r/usr/lib/eclipse/previous/ec-ctl"
check "keep: VERSION comes from pacman's local db" [ "$(cat "$r/usr/lib/eclipse/previous/VERSION")" = 0.1.1-1 ]
check "keep: kept binary is executable" [ -x "$r/usr/lib/eclipse/previous/ec-abyss" ]
check "keep: no staging dirs left" [ ! -e "$r/usr/lib/eclipse/previous.new" ] && [ ! -e "$r/usr/lib/eclipse/previous.old" ]

"$keep" "$r" 2>/dev/null
check "keep: same build again is a no-op" [ "$(kept "$r")" = "# v1" ]

# v2 installed, nothing running: v2 is unproven, v1 stays.
printf '#!/bin/sh\n# v2\n' >"$r/usr/bin/ec-abyss"
"$keep" "$r" 2>/dev/null
check "keep: unproven build never replaces the fallback" [ "$(kept "$r")" = "# v1" ]

fakeproc "$r" 300 --backend drm --kiosk -- /usr/lib/eclipse/ec-greeter-run
"$keep" "$r" 2>/dev/null
check "keep: a running greeter does not prove the build" [ "$(kept "$r")" = "# v1" ]

fakeproc "$r" 400 --backend drm --session
"$keep" "$r" 2>/dev/null
check "keep: a running session proves it; fallback replaced" [ "$(kept "$r")" = "# v2" ]
check "keep: no staging dirs left after a swap" [ ! -e "$r/usr/lib/eclipse/previous.new" ] && [ ! -e "$r/usr/lib/eclipse/previous.old" ]

# Interrupted swap: only previous.old survives. It is restored, never lost.
r=$(mkroot v3 0.1.3-1 interrupted)
mkdir -p "$r/usr/lib/eclipse/previous.old"
printf '#!/bin/sh\n# v2\n' >"$r/usr/lib/eclipse/previous.old/ec-abyss"
"$keep" "$r" 2>/dev/null
check "keep: interrupted swap is restored, and unproven v3 does not replace it" [ "$(kept "$r")" = "# v2" ]

# Fail closed: the copy cannot be written, so the hook must fail.
r=$(mkroot v1 0.1.1-1 readonly)
chmod 0555 "$r/usr/lib/eclipse"
if "$keep" "$r" 2>/dev/null; then st=0; else st=$?; fi
chmod 0755 "$r/usr/lib/eclipse"
check "keep: a failed copy exits non-zero (pacman aborts)" [ "$st" -ne 0 ]

r=$(mkroot v2 0.1.2-1 readonly-swap)
mkdir -p "$r/usr/lib/eclipse/previous"
printf '#!/bin/sh\n# v1\n' >"$r/usr/lib/eclipse/previous/ec-abyss"
fakeproc "$r" 500 --session
chmod 0555 "$r/usr/lib/eclipse"
if "$keep" "$r" 2>/dev/null; then st=0; else st=$?; fi
chmod 0755 "$r/usr/lib/eclipse"
check "keep: failed replace exits non-zero" [ "$st" -ne 0 ]
check "keep: failed replace leaves the old fallback intact" [ "$(kept "$r")" = "# v1" ]

r=$(mkroot v1 0.1.1-1 skip)
EC_ABYSS_KEEP=skip "$keep" "$r" 2>/dev/null
check "keep: EC_ABYSS_KEEP=skip takes no copy and succeeds" [ ! -e "$r/usr/lib/eclipse/previous" ]

if "$keep" a b 2>/dev/null; then st=0; else st=$?; fi
check "keep: extra arguments exit 2" [ "$st" -eq 2 ]

# --- ec-abyss-launch ---------------------------------------------------------

L="$tmp/launch"
mkdir -p "$L/usr/lib/eclipse/previous" "$L/bin"
log="$L/log"
export LOG="$log"
mkbin() { printf '#!/bin/sh\necho "%s $*" >>"$LOG"\n%s\n' "$2" "$3" >"$1"; chmod 0755 "$1"; }
mkbin "$L/usr/lib/eclipse/previous/ec-abyss" previous 'exit 0'
echo 0.1.1-1 >"$L/usr/lib/eclipse/previous/VERSION"
mkbin "$L/bin/crash" current 'exit 1'
mkbin "$L/bin/clean" current 'exit 0'
mkbin "$L/bin/slow" current 'sleep 2; exit 1'
mkbin "$L/bin/sleeper" current 'sleep 5; exit 0'

run() { : >"$log"; ECLIPSE_ROOT="$L" "$launch" "$@" 2>/dev/null; }
logis() { [ "$(cat "$log")" = "$1" ]; }
nl='
'

run "$L/bin/crash" --backend drm --kiosk -- /usr/lib/eclipse/ec-greeter-run || true
check "launch: startup crash falls back with the same args" \
    logis "current --backend drm --kiosk -- /usr/lib/eclipse/ec-greeter-run${nl}previous --backend drm --kiosk -- /usr/lib/eclipse/ec-greeter-run"

run "$L/bin/clean" --session
check "launch: clean exit does not fall back" logis "current --session"

if EC_ABYSS_FALLBACK_SECS=1 run "$L/bin/slow" --session; then st=0; else st=$?; fi
check "launch: a crash after the window passes its status through" [ "$st" -eq 1 ]
check "launch: ...and does not fall back" logis "current --session"

mv "$L/usr/lib/eclipse/previous/ec-abyss" "$L/usr/lib/eclipse/previous/ec-abyss.keep"
cp "$L/bin/crash" "$L/usr/lib/eclipse/previous/ec-abyss"
if run "$L/bin/crash" --session; then st=0; else st=$?; fi
check "launch: kept copy identical to the crashing build is not re-run" logis "current --session"
mv "$L/usr/lib/eclipse/previous/ec-abyss.keep" "$L/usr/lib/eclipse/previous/ec-abyss"

: >"$log"
ECLIPSE_ROOT="$L" "$launch" "$L/bin/sleeper" --session 2>/dev/null &
lpid=$!
sleep 1
kill -TERM "$lpid"
wait "$lpid" 2>/dev/null || true
sleep 0.2
check "launch: a stop sent to the wrapper never starts the previous build" logis "current --session"

mv "$L/usr/lib/eclipse/previous" "$L/usr/lib/eclipse/gone"
if run "$L/bin/crash" --session; then st=0; else st=$?; fi
check "launch: no kept build: crash status passes through" [ "$st" -eq 1 ]
mv "$L/usr/lib/eclipse/gone" "$L/usr/lib/eclipse/previous"

if ECLIPSE_ROOT="$L" "$launch" 2>/dev/null; then st=0; else st=$?; fi
check "launch: no binary exits 2" [ "$st" -eq 2 ]

# --- abyss-session -----------------------------------------------------------
# Fake systemd-cat and ec-abyss on PATH; ec-ctl absent so the migration is skipped.
S="$tmp/session"
mkdir -p "$S/bin" "$S/usr/lib/eclipse/previous"
printf '#!/bin/sh\nshift 2\nexec "$@"\n' >"$S/bin/systemd-cat"
printf '#!/bin/sh\necho "current $*" >>"$LOG"\n' >"$S/bin/ec-abyss"
printf '#!/bin/sh\necho "previous $*" >>"$LOG"\n' >"$S/usr/lib/eclipse/previous/ec-abyss"
chmod 0755 "$S/bin/systemd-cat" "$S/bin/ec-abyss" "$S/usr/lib/eclipse/previous/ec-abyss"
# A launcher that records any use: the session must never go through it.
printf '#!/bin/sh\necho launcher >>"$LOG"\n' >"$S/usr/lib/eclipse/ec-abyss-launch"
chmod 0755 "$S/usr/lib/eclipse/ec-abyss-launch"
sess() { : >"$log"; PATH="$S/bin:/usr/bin:/bin" ECLIPSE_ROOT="$S" sh "$session" "$@" 2>/dev/null; }

if sess; then st=0; else st=$?; fi
check "session: execs the compositor directly, no re-exec, no launcher" logis "current --backend drm --session"
sess --previous
check "session: --previous execs the kept build" logis "previous --backend drm --session"
rm "$S/usr/lib/eclipse/previous/ec-abyss"
sess --previous
check "session: --previous without a kept build runs the current one" logis "current --backend drm --session"
printf '#!/bin/sh\necho "current $*" >>"$LOG"\nexit 1\n' >"$S/bin/ec-abyss"
if sess; then st=0; else st=$?; fi
check "session: a crashing compositor exits non-zero, nothing else starts" [ "$st" -eq 1 ] && logis "current --backend drm --session"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
