#!/usr/bin/env bash
# Agent end-to-end smoke run: headless abyss + policyd + agentd + a scripted
# agent on ec-agent.sock. Test-only; touches nothing outside $R.
#
# Build first (from abyss/):
#   export CARGO_TARGET_DIR=/home/chase/.cache/ab15 CARGO_BUILD_JOBS=2
#   nice -n 19 cargo build -p ec-abyss -p ec-policyd -p ec-agentd -p ec-ctl \
#     -p ec-audit -p ec-smoke -p ec-ref-agent \
#     --features ec-abyss/smoke-input,ec-abyss/dev-peers,ec-policyd/dev-peers,ec-agentd/dev-unsandboxed
#
# Bypasses (printed as BYPASS lines): the grant hand-off to the agent is done by
# a stand-in named `ec-agentd`, because the real ec-agentd drops the grant.
set -u
T=${CARGO_TARGET_DIR:-/home/chase/.cache/ab15}/debug
R=/home/chase/.cache/smoke-run
rm -rf "$R"; mkdir -p "$R"/{run/eclipse,home,cfg/eclipse,data,state,sd,policy,empty,bin,data/eclipse/agents/smoke-agent/0.1.0/bin}
chmod 700 "$R/run"
export HOME=$R/home XDG_RUNTIME_DIR=$R/run XDG_CONFIG_HOME=$R/cfg XDG_DATA_HOME=$R/data XDG_STATE_HOME=$R/state
export ECLIPSE_STATE_DIR=$R/state ECLIPSE_POLICY_DIR=$R/policy ECLIPSE_AGENTS_DIR=$R/data/eclipse/agents
export ECLIPSE_POLICYD_SOCKET=$R/run/eclipse/policyd.sock ECLIPSE_HEADLESS_INPUT=$R/input.sock
export ECLIPSE_AGENT_SOCKET=$R/run/eclipse/ec-agent.sock SMOKE_SYNC=$R/sd
unset WAYLAND_DISPLAY DISPLAY ECLIPSE_MCP_SOCKET
# Every eclipse binary goes through E: env -i plus the private tree, nothing inherited.
E=(env -i PATH=/usr/bin:/bin HOME="$HOME" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" XDG_CONFIG_HOME="$XDG_CONFIG_HOME"
   XDG_DATA_HOME="$XDG_DATA_HOME" XDG_STATE_HOME="$XDG_STATE_HOME" ECLIPSE_STATE_DIR="$ECLIPSE_STATE_DIR"
   ECLIPSE_POLICY_DIR="$ECLIPSE_POLICY_DIR" ECLIPSE_AGENTS_DIR="$ECLIPSE_AGENTS_DIR"
   ECLIPSE_POLICYD_SOCKET="$ECLIPSE_POLICYD_SOCKET" ECLIPSE_HEADLESS_INPUT="$ECLIPSE_HEADLESS_INPUT"
   ECLIPSE_AGENT_SOCKET="$ECLIPSE_AGENT_SOCKET" SMOKE_SYNC="$SMOKE_SYNC" SMOKE_OUT="${SMOKE_OUT:-}" WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-}" WAYLAND_DEBUG="${SMOKE_WLDEBUG:-}")
case $HOME in /home/chase/.cache/smoke-run/*) ;; *) echo "refusing: HOME not private"; exit 2;; esac
PIDS=()
trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; echo "killed: ${PIDS[*]}"' EXIT
spawn() { local out=$1; shift; "$@" >"$R/$out.log" 2>&1 & PIDS+=($!); LAST=$!; }
inp() { python3 - "$@" <<'PY'
import socket,sys,os
s=socket.socket(socket.AF_UNIX); s.connect(os.environ["ECLIPSE_HEADLESS_INPUT"])
s.sendall(("\n".join(sys.argv[1:])+"\n").encode()); s.close()
PY
}
tap() { inp "key $1 1"; sleep 0.05; inp "key $1 0"; }
say() { echo "== $*"; }

cat >"$R/cfg/eclipse/policy.kdl" <<'K'
windowrule "seat-compat lock" { app-id "ec-smoke-win" }
K
cat >"$ECLIPSE_AGENTS_DIR/smoke-agent/0.1.0/manifest.kdl" <<'K'
agent {
  id "smoke-agent"
  name "Smoke Agent"
  version "0.1.0"
  publisher "local"
  entrypoint "bin/ec-ref-agent"
  task { default_deadline "2h"
         max_depth 0 }
  capabilities {
    scene.list scope { app_id "ec-smoke-win"; class "private" }
    because "smoke"
    scene.read scope { app_id "ec-smoke-win"; class "private" }
    because "smoke"
    seat.focus scope { app_id "ec-smoke-win"; class "private" }
    because "smoke"
    seat.text scope { app_id "ec-smoke-win"; class "private" }
    because "smoke"
    click scope { app_id "ec-smoke-win"; class "private" }
    because "smoke"
    seat.compat_lock scope { app_id "ec-smoke-win"; class "private" }
    because "smoke"
  }
  sandbox {
  }
  resumable #false
}
K
cp "$T/ec-ref-agent" "$R/data/eclipse/agents/smoke-agent/0.1.0/bin/"
cp "$T/ec-smoke" "$R/bin/ec-agentd"   # the stand-in; policyd keys the role off exe basename

say "STEP1 start abyss (headless), policyd, agentd"
spawn abyss "${E[@]}" PATH="$R/empty" "$T/ec-abyss" --backend headless --size 1280x800; ABY=$LAST
for i in $(seq 100); do [ -S "$R/input.sock" ] && [ -S "$R/run/eclipse/abyss.sock" ] && break; sleep 0.1; done
WL=$(ls "$R/run" | grep -E '^wayland-[0-9]+$' | head -1); export WAYLAND_DISPLAY=$WL; E+=(WAYLAND_DISPLAY="$WL")
echo "wayland=$WL agent_sock=$([ -S "$ECLIPSE_AGENT_SOCKET" ] && echo yes || echo NO)"
spawn policyd "${E[@]}" "$T/ec-policyd"
for i in $(seq 50); do [ -S "$ECLIPSE_POLICYD_SOCKET" ] && break; sleep 0.1; done
spawn agentd "${E[@]}" "$T/ec-agentd"
sleep 2
"${E[@]}" "$T/ec-ctl" agents | head -5

say "STEP3 install the package via the compositor modal"
spawn install "${E[@]}" "$T/ec-ctl" agent install "$ECLIPSE_AGENTS_DIR/smoke-agent/0.1.0"; INS=$LAST
sleep 1.5; inp "dump $R/install-modal.pam"
tap 15; sleep 0.2; tap 57   # Tab to Grant, Space
sleep 1.5
wait "$INS" 2>/dev/null; cat "$R/install.log"
ls "$ECLIPSE_AGENTS_DIR"/smoke-agent/* 2>&1 | head -3

say "STEP2 ordinary window client"
spawn window "${E[@]}" SMOKE_OUT=$R/window.out "$T/ec-smoke" window; WIN=$LAST
sleep 2; "${E[@]}" "$T/ec-ctl" windows | head

say "STEP3 stand-in grant holder (BYPASS: real agentd drops the grant)"
spawn agent "${E[@]}" "$R/bin/ec-agentd" agent
for i in $(seq 50); do [ -e "$R/sd/standin-ready" ] && break; sleep 0.1; done

say "STEP3 commit slot (human Enter)"
spawn slot "${E[@]}" SMOKE_OUT=$R/slot.out "$T/ec-smoke" slot smoke-agent "Smoke: type hello into the window"; SLOT=$LAST
sleep 1.5; inp "dump $R/slot-card.pam"; sleep 0.3
tap 28
sleep 1; inp "dump $R/commit-modal.pam"
tap 15; sleep 0.2; tap 57   # overflow: Enter opens the modal card; Tab, Space commits
sleep 3
cat "$R/slot.out" 2>/dev/null | tail -5
echo "--- agent so far"; cat "$R/agent.log"

# the agent prints LOCKED when the lock is held; drive the human then
for i in $(seq 200); do [ -e "$R/sd/locked" ] && break; sleep 0.1; done
if [ -e "$R/sd/locked" ]; then
  say "STEP4 human input during compat lock"
  sleep 0.5; inp "dump $R/lock-indicator.pam"; sleep 0.5
  tap 30; tap 48
  sleep 0.5; touch "$R/sd/human-sent"
  for i in $(seq 100); do [ -e "$R/sd/unlocked" ] && break; sleep 0.1; done
fi
for i in $(seq 100); do [ -e "$R/sd/agent-done" ] && break; sleep 0.1; done

echo "lock marks (mono ms): locked=$(cat $R/sd/locked) human-sent-file=$(date +%s) unlocking=$(cat $R/sd/unlocking 2>/dev/null) unlocked=$(cat $R/sd/unlocked 2>/dev/null)"
say "STEP5 SEAT-01: a late client must not see agent-N"
"${E[@]}" SMOKE_OUT=$R/seats.out "$T/ec-smoke" seats 2>&1 | tee "$R/seats.log"
touch "$R/sd/stop"; sleep 1

say "window log"; cat "$R/window.out"
say "agent log"; cat "$R/agent.log"
say "STEP6 audit"
"${E[@]}" "$T/ec-audit" --dir "$R/state/policyd" verify; echo "verify rc=$?"
"${E[@]}" "$T/ec-audit" --dir "$R/state/policyd" query --since 1h >"$R/audit.jsonl"
python3 - "$R/audit.jsonl" <<'PY'
import json,sys,collections
c=collections.Counter()
for l in open(sys.argv[1]):
    r=json.loads(l); c[(r["kind"],r["principal"].split("-")[0])]+=1
    if r["kind"]=="decision" and r["body"].get("outcome")!="allow": print("non-allow decision",r.get("req_id"),r["body"])
    if r["kind"]=="result" and r["body"].get("status")!=0: print("non-ok result",r.get("req_id"),r["body"])
print(dict(c))
PY
for rid in 105 108 112; do echo "-- trace req $rid"; "${E[@]}" "$T/ec-audit" --dir "$R/state/policyd" trace --req-id $rid | cut -c1-230; done
say "ec-ctl agents"; "${E[@]}" "$T/ec-ctl" agents | head
say "abyss journal"; journalctl --user -t ec-abyss --since "10 min ago" _PID=$ABY 2>/dev/null | tail -40
for f in install-modal slot-card commit-modal lock-indicator; do
  [ -f "$R/$f.pam" ] && nice -n 19 ffmpeg -loglevel error -y -i "$R/$f.pam" "$R/$f.png" && echo "png $R/$f.png"
done
