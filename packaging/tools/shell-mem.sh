#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Read-only memory table for every running ec-* process (ADR 0104 stage 0).
# RSS/PSS/anon in MiB from /proc/<pid>/smaps_rollup; VRAM from nvidia-smi
# compute/graphics apps or DRM fdinfo (drm-memory-*/drm-total-*). Never signals.
# Own-uid processes only unless run as root (smaps_rollup is not readable otherwise).
set -u
mib() { awk -v k="$1" 'BEGIN{printf "%.0f", k/1024}'; }

declare -A vram
if command -v nvidia-smi >/dev/null 2>&1; then
  while IFS=, read -r pid used; do
    pid=${pid// /}; used=${used// /}
    [[ $pid =~ ^[0-9]+$ && $used =~ ^[0-9]+$ ]] && vram[$pid]=$used
  done < <(nvidia-smi --query-compute-apps=pid,used_memory --format=csv,noheader,nounits 2>/dev/null)
fi

fdinfo_vram() { # pid -> MiB from DRM fdinfo, empty if none
  local f total=0 seen=0 v
  declare -A done_ids=()
  for f in /proc/"$1"/fdinfo/*; do
    [[ -r $f ]] || continue
    grep -q '^drm-driver:' "$f" 2>/dev/null || continue
    id=$(awk '/^drm-client-id:/{print $2}' "$f")
    [[ -n $id && -n ${done_ids[$id]:-} ]] && continue
    done_ids[$id]=1
    v=$(awk '/^drm-(memory-vram|total-vram[0-9]*|total-local[0-9]*):/{
         n=$2; u=$3; if(u=="KiB")n/=1024; else if(u=="MiB")n=n; else if(u=="GiB")n*=1024; else n/=1048576; s+=n}
         END{printf "%d", s}' "$f")
    total=$((total + v)); seen=1
  done
  ((seen)) && echo "$total"
}

printf '%-8s %-22s %7s %7s %7s %7s\n' PID COMM RSS PSS ANON VRAM
sr=0 sp=0 sa=0 sv=0
for d in /proc/[0-9]*; do
  pid=${d#/proc/}
  comm=$(<"$d/comm") 2>/dev/null || continue
  [[ $comm == ec-* ]] || continue
  roll="$d/smaps_rollup"
  [[ -r $roll ]] || { printf '%-8s %-22s %7s\n' "$pid" "$comm" "(unreadable)"; continue; }
  read -r rss pss anon < <(awk '/^Rss:/{r=$2}/^Pss:/{p=$2}/^Anonymous:/{a=$2}END{print r+0,p+0,a+0}' "$roll")
  v=${vram[$pid]:-}
  [[ -z $v ]] && v=$(fdinfo_vram "$pid")
  printf '%-8s %-22s %7s %7s %7s %7s\n' "$pid" "$comm" "$(mib "$rss")" "$(mib "$pss")" "$(mib "$anon")" "${v:--}"
  sr=$((sr + rss)); sp=$((sp + pss)); sa=$((sa + anon)); sv=$((sv + ${v:-0}))
done
printf '%-8s %-22s %7s %7s %7s %7s\n' - 'TOTAL ec-*' "$(mib "$sr")" "$(mib "$sp")" "$(mib "$sa")" "$sv"
echo "(MiB; RSS from smaps_rollup. '-' = no VRAM figure available. ec-abyss VRAM and others may be absent from nvidia-smi on some drivers.)"
awk '/^MemTotal:/{t=$2}/^MemAvailable:/{a=$2}END{printf "system: MemTotal %d MiB, used (total-available) %d MiB\n", t/1024, (t-a)/1024}' /proc/meminfo
if command -v nvidia-smi >/dev/null 2>&1; then
  nvidia-smi --query-gpu=memory.used,memory.total --format=csv,noheader 2>/dev/null | sed 's/^/gpu vram used\/total: /'
fi
