#!/usr/bin/env bash
set -euo pipefail

# Measure combined Physical footprint (vmmap --summary) of Alacritty + velm server + velm client
# across scenarios E1..E3 (and optional E4) in 3 rounds and print the median per scenario.

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: scripts/mem.sh requires macOS (vmmap)" >&2
  exit 2
fi

for cmd in alacritty velm vmmap awk sort pgrep; do
  command -v "$cmd" >/dev/null || { echo "error: required command not found: $cmd" >&2; exit 1; }
done

script_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$script_dir/.." && pwd)
config_file=${VELM_ALACRITTY_CONFIG:-"$repo_root/extras/alacritty.toml"}
include_e4=${VELM_MEM_INCLUDE_E4:-0}
runs=3

[[ -f "$config_file" ]] || { echo "error: config not found: $config_file" >&2; exit 1; }

tmp_root=$(mktemp -d /tmp/velm-mem.XXXXXX)
alacritty_pid=""
session_name="velm-mem-$$"

cleanup() {
  if [[ -n "$alacritty_pid" ]] && kill -0 "$alacritty_pid" 2>/dev/null; then
    kill "$alacritty_pid" 2>/dev/null || true
    wait "$alacritty_pid" 2>/dev/null || true
  fi
  XDG_CONFIG_HOME="$tmp_root/config" "$velm_bin" session stop "$session_name" >/dev/null 2>&1 || true
  rm -rf "$tmp_root"
}
trap cleanup EXIT

velm_bin=$(command -v velm)
mkdir -p "$tmp_root/config/velm"
printf 'onboarding = false\n[terminal]\nshell_mode = "non_login"\n' > "$tmp_root/config/velm/config.toml"

footprint_mb() {
  local pid=$1
  vmmap --summary "$pid" 2>/dev/null | awk '
    /Physical footprint:/ {
      val = $3
      unit = substr(val, length(val))
      num = substr(val, 1, length(val) - 1) + 0
      if (unit == "G") num *= 1024
      else if (unit == "K") num /= 1024
      printf "%.2f", num
      found = 1
      exit
    }
    END { if (!found) print "0.00" }
  '
}

wait_for_session_socket() {
  local sock="$tmp_root/config/velm/sessions/$session_name/velm.sock"
  local i=0
  while [[ ! -S "$sock" ]] && (( i < 100 )); do
    sleep 0.1
    i=$((i + 1))
  done
  [[ -S "$sock" ]] || { echo "error: velm socket did not appear at $sock" >&2; exit 1; }
}

sum_process_footprints() {
  local sock="$tmp_root/config/velm/sessions/$session_name/velm.sock"
  local server_pid client_pids total
  server_pid=$(lsof -t "$sock" 2>/dev/null | head -n 1 || true)
  client_pids=$(pgrep -P "$alacritty_pid" -x velm 2>/dev/null || true)

  total=0
  for pid in $alacritty_pid $server_pid $client_pids; do
    [[ -n "$pid" ]] || continue
    if kill -0 "$pid" 2>/dev/null; then
      local mb
      mb=$(footprint_mb "$pid")
      total=$(awk -v a="$total" -v b="$mb" 'BEGIN { printf "%.2f", a + b }')
    fi
  done
  echo "$total"
}

median3() {
  printf '%s\n%s\n%s\n' "$1" "$2" "$3" | sort -n | sed -n '2p'
}

run_scenario() {
  local scenario=$1
  alacritty_pid=""
  rm -rf "$tmp_root/config/velm/sessions"

  XDG_CONFIG_HOME="$tmp_root/config" \
  HERDR_SESSION="$session_name" \
    alacritty --config-file "$config_file" -e "$velm_bin" --session "$session_name" >/dev/null 2>&1 &
  alacritty_pid=$!

  wait_for_session_socket
  sleep 2

  case "$scenario" in
    E1)
      sleep 30
      ;;
    E2)
      local pane_id
      pane_id=$(XDG_CONFIG_HOME="$tmp_root/config" "$velm_bin" --session "$session_name" pane current --json 2>/dev/null | awk -F'"' '/"pane_id"/{print $4; exit}')
      if [[ -n "$pane_id" ]]; then
        XDG_CONFIG_HOME="$tmp_root/config" "$velm_bin" --session "$session_name" pane run "$pane_id" "seq 1 100000" >/dev/null 2>&1 || true
      fi
      sleep 5
      ;;
    E3)
      for _ in 1 2 3 4; do
        XDG_CONFIG_HOME="$tmp_root/config" "$velm_bin" --session "$session_name" tab create >/dev/null 2>&1 || true
      done
      sleep 5
      ;;
    E4)
      local pane_id
      pane_id=$(XDG_CONFIG_HOME="$tmp_root/config" "$velm_bin" --session "$session_name" pane split --direction right --json 2>/dev/null | awk -F'"' '/"pane_id"/{print $4; exit}' || true)
      sleep 2
      if [[ -n "$pane_id" ]]; then
        XDG_CONFIG_HOME="$tmp_root/config" "$velm_bin" --session "$session_name" pane close "$pane_id" >/dev/null 2>&1 || true
      fi
      sleep 30
      ;;
  esac

  local total
  total=$(sum_process_footprints)

  if [[ -n "$alacritty_pid" ]] && kill -0 "$alacritty_pid" 2>/dev/null; then
    kill "$alacritty_pid" 2>/dev/null || true
    wait "$alacritty_pid" 2>/dev/null || true
  fi
  alacritty_pid=""
  XDG_CONFIG_HOME="$tmp_root/config" "$velm_bin" session stop "$session_name" >/dev/null 2>&1 || true
  echo "$total"
}

scenarios=(E1 E2 E3)
if [[ "$include_e4" == "1" ]]; then
  scenarios+=(E4)
fi

for scenario in "${scenarios[@]}"; do
  vals=()
  for r in $(seq 1 "$runs"); do
    vals+=("$(run_scenario "$scenario")")
  done
  med=$(median3 "${vals[0]}" "${vals[1]}" "${vals[2]}")
  printf '%s median=%s MiB runs=(%s, %s, %s)\n' "$scenario" "$med" "${vals[0]}" "${vals[1]}" "${vals[2]}"
done
