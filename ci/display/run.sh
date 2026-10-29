#!/usr/bin/env bash
# Live display scenarios for the visor and the companion.
#
#   bash ci/display/run.sh x11
#   bash ci/display/run.sh x11 inject-lands
#   bash ci/display/run.sh wayland
#
# X11 physical input is a kernel uinput pointer and keyboard. Xorg's dummy
# video driver plus libinput (or the evdev driver on those same nodes) exposes
# them as slave devices. They are not the XTEST pointer. Injected mouse and
# type still use XTEST. This script does not fake a person at the machine
# with XTEST.
#
# Each scenario runs in its own process group with a hard timeout. Logs and
# failure captures go to display-logs/. Caller-provided HYPERMESH_* filesystem
# paths are ignored. Runtime, config, state, and data directories are ones
# this script creates with mktemp. remove, chmod, and writes go through
# refuse_outside, which rejects a path outside those directories and
# display-logs/. Process ids that have to survive a scenario subshell live
# in the private state directory. A pid file is removed only after that
# process is gone. Xorg's display comes from -displayfd. The harness does
# not read or remove /tmp/.X*-lock, and it does not chmod /dev/uinput or
# the event nodes. The uinput helper and the companion-socket helper read
# commands on stdin. Coproc ends are duplicated onto ordinary fds before any
# scenario starts, because bash closes coproc fds in a subshell. Commands
# take a flock in the private state directory. A write error, a read error,
# or a missing ack fails the scenario. Neither helper listens. Around every
# scenario, ss -ltnp must show no listening TCP socket for a process this
# run started. The visor is the exception only when its command line
# contains --tcp.
set -euo pipefail
set -m

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"

DISPLAY_LOG_DIR=$root/display-logs
mkdir -p "$DISPLAY_LOG_DIR"
HARNESS_ROOTS=("$DISPLAY_LOG_DIR")
# Request bodies for code that runs outside a scenario. run_named points
# each scenario at its own directory.
REQ_DIR=$DISPLAY_LOG_DIR
HTTP_BODY=$REQ_DIR/http.body
SCENARIO_BG=

ignore_caller_hypermesh_path() {
  local name=$1
  local value=${!name-}
  [[ -n $value ]] || return 0
  echo "ignoring $name ($value): the display harness uses a private temporary directory and does not remove files from a directory it did not create" >&2
  unset "$name"
}

hypermesh_path_name() {
  case $1 in
    HYPERMESH_*_DIR|HYPERMESH_*_PATH|HYPERMESH_*_FILE|HYPERMESH_*_HOME|HYPERMESH_*_ROOT|HYPERMESH_*_PROJECT|HYPERMESH_*STATE*|HYPERMESH_*DATA*|HYPERMESH_*CONFIG*)
      return 0
      ;;
  esac
  return 1
}

while IFS= read -r name; do
  hypermesh_path_name "$name" || continue
  ignore_caller_hypermesh_path "$name"
done < <(compgen -e || true)

RUNTIME=$(mktemp -d "${TMPDIR:-/tmp}/hm-display.XXXXXX")
CONFIG=$(mktemp -d "${TMPDIR:-/tmp}/hm-display-config.XXXXXX")
STATE=$(mktemp -d "${TMPDIR:-/tmp}/hm-display-state.XXXXXX")
DATA=$(mktemp -d "${TMPDIR:-/tmp}/hm-display-data.XXXXXX")
MCP_PROJECT=$(mktemp -d "${TMPDIR:-/tmp}/hm-display-mcp.XXXXXX")
HARNESS_RUNTIME_DIR=$RUNTIME
HARNESS_CONFIG_DIR=$CONFIG
HARNESS_STATE_DIR=$STATE
HARNESS_DATA_DIR=$DATA
HARNESS_MCP_DIR=$MCP_PROJECT
HARNESS_ROOTS+=("$RUNTIME" "$CONFIG" "$STATE" "$DATA" "$MCP_PROJECT")
if [[ -z ${XDG_RUNTIME_DIR:-} ]]; then
  HARNESS_XDG=$(mktemp -d "${TMPDIR:-/tmp}/hm-display-xdg.XXXXXX")
  export XDG_RUNTIME_DIR=$HARNESS_XDG
  HARNESS_ROOTS+=("$HARNESS_XDG")
fi
VISOR_BIN=$root/target/debug/hypermesh-visor
COMPANION_BIN=$root/target/debug/hypermesh-companion
# Cross-scenario process state. Scenario bodies run in subshells, so a shell
# variable set there never reaches the parent or the next scenario.
PROC=$STATE/procs
mkdir -p "$PROC"
COMPANION_PID_FILE=$PROC/companion.pid
XEV_LOG=$DISPLAY_LOG_DIR/xev.log
XEV_AT=0
FAILED=()
PHYSICAL_DRIVER=
BASE=
CONTROL_SOCK=
TCP_BASE=
SID=
UINPUT_IN=
UINPUT_OUT=
UINPUT_PID=
SOCK_IN=
SOCK_OUT=
SOCK_PID=
TCP_WATCH_PID=
XORG_PID=
XEV_PID=
VISOR_PID=
VISOR_PID_FILE=$PROC/visor.pid
COMPOSITOR_PID=
COMPANION_WAS_UP=
CORE_POINTER_ID=
CORE_KEYBOARD_ID=
XTEST_POINTER='Virtual core XTEST pointer'
XTEST_KEYBOARD='Virtual core XTEST keyboard'
ROOT_W=
ROOT_H=
WAYLAND_SOCK_BEFORE=

export HYPERMESH_RUNTIME_DIR=$RUNTIME
export HYPERMESH_CONFIG_DIR=$CONFIG
export HYPERMESH_STATE_DIR=$STATE
export HYPERMESH_DATA_DIR=$DATA
export HYPERMESH_MCP_PROJECT=$MCP_PROJECT
unset HYPERMESH_AUTOMATION_RESUME_SECS || true

die() {
  echo "FAIL: $*" >&2
  exit 1
}

# Canonical path. Missing components are kept so a file we are about to
# create is judged the same way as one that already exists.
owned_path() {
  realpath -m -- "$1"
}

# True when `path` is a harness root or a file inside one.
path_inside_roots() {
  local path=$1 root
  [[ -n $path && $path != / ]] || return 1
  for root in "${HARNESS_ROOTS[@]}"; do
    [[ -n $root && $root != / ]] || continue
    root=$(owned_path "$root")
    [[ $path == "$root" || $path == "$root"/* ]] && return 0
  done
  return 1
}

# Refuse rm, chmod, or a write outside the harness temp dirs and display-logs.
refuse_outside() {
  local path
  [[ -n ${1:-} ]] || die "refusing an empty path"
  path=$(owned_path "$1")
  if path_inside_roots "$path"; then
    return 0
  fi
  die "refusing to remove, chmod, or write $path: outside the harness temp dirs and display-logs"
}

allow_write() {
  local path
  for path in "$@"; do
    refuse_outside "$path"
  done
}

remove_inside() {
  local path
  [[ $# -gt 0 ]] || return 0
  for path in "$@"; do
    refuse_outside "$path"
  done
  rm -f -- "$@"
}

remove_tree() {
  local path
  for path in "$@"; do
    [[ -n $path && -d $path ]] || continue
    refuse_outside "$path"
    rm -rf -- "$path"
  done
}

chmod_inside() {
  local mode=$1
  shift
  local path
  for path in "$@"; do
    refuse_outside "$path"
  done
  chmod "$mode" -- "$@"
}

read_state() {
  local file=$PROC/$1
  [[ -f $file ]] || return 0
  tr -d '\n' <"$file"
}

write_state() {
  local file=$PROC/$1
  allow_write "$file"
  printf '%s\n' "$2" >"$file"
}

clear_state() {
  local file=$PROC/$1
  [[ -e $file ]] || return 0
  remove_inside "$file"
}

# Every process this run starts. Cleanup signals the list; the self-check
# fails if one is still the same executable afterwards.
# `marker` is a cmdline substring unique to this launch. Cleanup may sudo
# kill only when that substring is still there.
note_started() {
  local pid=$1 exe=$2 marker=${3:-}
  [[ $pid =~ ^[0-9]+$ && -n $exe ]] || return 0
  allow_write "$PROC/started"
  printf '%s\t%s\t%s\n' "$pid" "$exe" "$marker" >>"$PROC/started"
  disown "$pid" 2>/dev/null || true
}

started_by_this_run() {
  local pid=$1 line_pid line_exe
  [[ -f $PROC/started ]] || return 1
  while IFS=$'\t' read -r line_pid line_exe; do
    [[ $line_pid == "$pid" ]] && return 0
  done <"$PROC/started"
  return 1
}

load_proc_state() {
  CONTROL_SOCK=$(read_state control.sock)
  TCP_BASE=$(read_state tcp.base)
  SID=$(read_state session.id)
  VISOR_PID=$(read_state visor.pid)
  COMPANION_WAS_UP=$(read_state companion.was-up)
  UINPUT_POINTER=$(read_state uinput.pointer)
  UINPUT_KEYBOARD=$(read_state uinput.keyboard)
  local disp
  disp=$(read_state xorg.display)
  if [[ $disp =~ ^[0-9]+$ ]]; then
    export DISPLAY=:$disp
  fi
}

# Root-owned Xorg is not ptrace-readable by the unprivileged CI runner.
# Keep executable identity checks exact; only the readlink needs privilege.
proc_executable() {
  local pid=$1 exe
  [[ $pid =~ ^[0-9]+$ ]] || return 1
  if exe=$(readlink "/proc/$pid/exe" 2>/dev/null); then
    printf '%s\n' "$exe"
  else
    sudo -n readlink "/proc/$pid/exe" 2>/dev/null
  fi
}

exe_is_xorg() {
  local pid=$1 exe base
  [[ $pid =~ ^[0-9]+$ ]] || return 1
  exe=$(proc_executable "$pid" || true)
  base=$(basename "$exe")
  [[ $base == Xorg || $base == Xorg.bin ]]
}

# The only place that may change a device node. The harness does not call it.
# A caller that opts in gets the original mode, owner, group, and ACL restored
# from the exit trap.
change_device_perms() {
  local path=$1 new_mode=$2 mode owner group
  [[ -e $path ]] || die "no such device $path"
  read -r mode owner group < <(stat -c '%a %U %G' "$path")
  echo "device permission change: $path mode $mode owner ${owner}:${group} -> mode $new_mode" >&2
  if [[ ${HYPERMESH_DISPLAY_DEVICE_PERMS:-} != 1 ]]; then
    die "refusing to change $path; set HYPERMESH_DISPLAY_DEVICE_PERMS=1 to opt in"
  fi
  allow_write "$PROC/device-perms.log"
  printf '%s\t%s\t%s\t%s\n' "$path" "$mode" "$owner" "$group" >>"$PROC/device-perms.log"
  if command -v getfacl >/dev/null 2>&1; then
    local acl=$PROC/acl-$(basename "$path")
    allow_write "$acl"
    getfacl -p "$path" >"$acl"
  fi
  sudo -n chmod "$new_mode" -- "$path"
}

restore_device_perms() {
  local log=$PROC/device-perms.log
  [[ -f $log ]] || return 0
  local path mode owner group acl
  while IFS=$'\t' read -r path mode owner group; do
    [[ -n $path && -e $path ]] || continue
    echo "restoring $path to mode $mode owner ${owner}:${group}" >&2
    sudo -n chown "$owner:$group" -- "$path" 2>/dev/null || true
    sudo -n chmod "$mode" -- "$path" 2>/dev/null || true
    acl=$PROC/acl-$(basename "$path")
    if [[ -f $acl ]] && command -v setfacl >/dev/null 2>&1; then
      sudo -n setfacl --restore=- <"$acl" 2>/dev/null || true
    fi
  done <"$log"
  remove_inside "$log"
}

remember_uinput() {
  [[ -e /dev/uinput ]] || {
    if [[ ! -f $PROC/uinput.stamp ]]; then
      allow_write "$PROC/uinput.stamp"
      printf 'absent\n' >"$PROC/uinput.stamp"
    fi
    return 0
  }
  if [[ -f $PROC/uinput.stamp ]] && ! grep -qx 'absent' "$PROC/uinput.stamp"; then
    return 0
  fi
  allow_write "$PROC/uinput.stamp"
  stat -c '%a %U %G' /dev/uinput >"$PROC/uinput.stamp"
}

uinput_is_world_writable() {
  local digit=${1: -1}
  [[ $digit =~ ^[0-9]$ ]] || return 1
  (( digit & 2 ))
}

# Fail when /dev/uinput is world-writable, or when its mode and owner
# differ from the stamp taken before this run changed anything.
assert_uinput_mode() {
  local now stamp mode
  if [[ ! -e /dev/uinput ]]; then
    if [[ -f $PROC/uinput.stamp ]] && ! grep -qx 'absent' "$PROC/uinput.stamp"; then
      echo "self-check: /dev/uinput disappeared (was $(cat "$PROC/uinput.stamp"))" >&2
      return 1
    fi
    return 0
  fi
  now=$(stat -c '%a %U %G' /dev/uinput)
  mode=${now%% *}
  if uinput_is_world_writable "$mode"; then
    echo "self-check: /dev/uinput is world-writable ($now)" >&2
    return 1
  fi
  if [[ ! -f $PROC/uinput.stamp ]]; then
    echo "self-check: /dev/uinput mode was not recorded" >&2
    return 1
  fi
  stamp=$(cat "$PROC/uinput.stamp")
  if [[ $stamp == absent ]]; then
    echo "self-check: /dev/uinput exists and its original mode was not recorded" >&2
    return 1
  fi
  if [[ $now != "$stamp" ]]; then
    echo "self-check: /dev/uinput is $now, originally $stamp" >&2
    return 1
  fi
}

# True when /proc/$pid still belongs to this executable. A recycled pid must
# not be signalled. `exe` is the inode the process started; cmdline covers a
# replaced binary whose symlink reads "(deleted)".
pid_matches_exe() {
  local pid=$1 expected=$2 exe raw cmd
  [[ $pid =~ ^[0-9]+$ ]] || return 1
  [[ -r /proc/$pid/cmdline ]] || return 1
  exe=$(proc_executable "$pid" || true)
  if [[ -n $exe && $exe == "$expected" ]]; then
    return 0
  fi
  raw=$exe
  if [[ $raw == "$expected" || $raw == "$expected (deleted)" ]]; then
    return 0
  fi
  cmd=$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null || true)
  [[ $cmd == "$expected" || $cmd == "$expected "* ]]
}

# Signal `pid` only when it is still `expected`. A recycled pid is left alone.
signal_if_exe() {
  local pid=$1 expected=$2 sig=${3:-TERM}
  if [[ -z $pid ]]; then
    return 1
  fi
  if pid_matches_exe "$pid" "$expected"; then
    if kill -s "$sig" "$pid" 2>/dev/null; then
      return 0
    fi
    return 2
  fi
  echo "not signalling pid $pid; it is not $expected" >&2
  return 1
}

# sudo kill only for a pid this run recorded, whose cmdline still contains
# the marker from that launch (the Xorg logfile, or the uinput helper path).
signal_root_child() {
  local pid=$1 expected=$2 marker=${3:-} sig=${4:-TERM} cmd
  [[ $pid =~ ^[0-9]+$ && -n $expected && -n $marker ]] || return 1
  pid_matches_exe "$pid" "$expected" || return 1
  started_by_this_run "$pid" || return 1
  cmd=$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null || true)
  if [[ $cmd != *"$marker"* ]]; then
    echo "not signalling pid $pid; cmdline does not contain $marker" >&2
    return 1
  fi
  sudo -n kill -s "$sig" "$pid" 2>/dev/null || true
}

# /proc still belongs to this launch. A recycled pid does not.
proc_still_ours() {
  local pid=$1 expected=$2
  [[ -d /proc/$pid ]] && pid_matches_exe "$pid" "$expected"
}

# TERM, wait out the visor's 5s drain plus margin, then KILL. The caller
# removes the pid file only after this returns 0.
wait_for_exit() {
  local pid=$1 expected=$2 marker=${3:-} i
  if ! proc_still_ours "$pid" "$expected"; then
    return 0
  fi
  if ! signal_if_exe "$pid" "$expected" TERM; then
    signal_root_child "$pid" "$expected" "$marker" TERM || true
  fi
  for i in $(seq 1 80); do
    proc_still_ours "$pid" "$expected" || return 0
    sleep 0.1
  done
  if proc_still_ours "$pid" "$expected"; then
    if ! signal_if_exe "$pid" "$expected" KILL; then
      signal_root_child "$pid" "$expected" "$marker" KILL || true
    fi
  fi
  for i in $(seq 1 20); do
    proc_still_ours "$pid" "$expected" || return 0
    sleep 0.1
  done
  if proc_still_ours "$pid" "$expected"; then
    echo "pid $pid ($expected) is still running after SIGKILL" >&2
    return 1
  fi
}

# Signal the recorded pid and an optional in-memory pid, wait until neither
# is still that executable, and only then remove the pid file.
signal_recorded_pid() {
  local file=$1 expected=$2 live=${3:-}
  local pid=""
  if [[ -f $file ]]; then
    pid=$(tr -d '[:space:]' <"$file" 2>/dev/null || true)
  fi
  if [[ $pid =~ ^[0-9]+$ ]]; then
    wait_for_exit "$pid" "$expected" || return 1
  fi
  if [[ -n $live && $live != "$pid" ]]; then
    wait_for_exit "$live" "$expected" || return 1
  fi
  if [[ -n $pid ]] && proc_still_ours "$pid" "$expected"; then
    return 1
  fi
  if [[ -n $live && $live != "$pid" ]] && proc_still_ours "$live" "$expected"; then
    return 1
  fi
  if [[ -f $file ]]; then
    remove_inside "$file"
  fi
}

# `set -m` gives each background request its own process group, so killing
# the scenario group leaves the curl running. The pid list is how a timeout
# reaches those jobs. The companion is not recorded here.
kill_tracked_requests() {
  local file=$1 bg
  [[ -f $file ]] || return 0
  while read -r bg; do
    [[ $bg =~ ^[0-9]+$ ]] || continue
    kill -TERM -- -"$bg" 2>/dev/null || kill -TERM "$bg" 2>/dev/null || true
  done <"$file" || true
}

MAIN_BASHPID=$BASHPID

stop_recorded() {
  local name=$1 exe=$2 marker=${3:-} pid
  pid=$(read_state "$name")
  [[ $pid =~ ^[0-9]+$ ]] || return 0
  if ! wait_for_exit "$pid" "$exe" "$marker"; then
    echo "leaving $name in place; pid $pid is still running" >&2
    return 1
  fi
  clear_state "$name"
}

# One pass over every process this run started. The pid list stays until
# each process is gone, so a later start can still see a visor that has
# not finished exiting.
stop_started() {
  local pid exe marker i alive
  [[ -f $PROC/started ]] || return 0
  while IFS=$'\t' read -r pid exe marker; do
    [[ $pid =~ ^[0-9]+$ && -n $exe ]] || continue
    proc_still_ours "$pid" "$exe" || continue
    if ! signal_if_exe "$pid" "$exe" TERM; then
      signal_root_child "$pid" "$exe" "$marker" TERM || true
    fi
  done <"$PROC/started"
  for i in $(seq 1 80); do
    alive=0
    while IFS=$'\t' read -r pid exe marker; do
      [[ $pid =~ ^[0-9]+$ && -n $exe ]] || continue
      if proc_still_ours "$pid" "$exe"; then
        alive=1
        break
      fi
    done <"$PROC/started"
    if (( alive == 0 )); then
      return 0
    fi
    sleep 0.1
  done
  while IFS=$'\t' read -r pid exe marker; do
    [[ $pid =~ ^[0-9]+$ && -n $exe ]] || continue
    proc_still_ours "$pid" "$exe" || continue
    if ! signal_if_exe "$pid" "$exe" KILL; then
      signal_root_child "$pid" "$exe" "$marker" KILL || true
    fi
  done <"$PROC/started"
  for i in $(seq 1 20); do
    alive=0
    while IFS=$'\t' read -r pid exe marker; do
      [[ $pid =~ ^[0-9]+$ && -n $exe ]] || continue
      if proc_still_ours "$pid" "$exe"; then
        alive=1
        break
      fi
    done <"$PROC/started"
    if (( alive == 0 )); then
      return 0
    fi
    sleep 0.1
  done
}

self_check() {
  local bad=0 pid exe mode other
  if ! assert_uinput_mode; then
    bad=1
  fi
  local node key node_stamp
  for key in uinput.pointer uinput.keyboard; do
    node=$(read_state "$key")
    [[ -n $node && -e $node ]] || continue
    node_stamp=$(read_state "$key.mode")
    mode=$(stat -c '%a %U %G' "$node")
    if [[ -z $node_stamp ]]; then
      echo "self-check: $node mode was not recorded" >&2
      bad=1
    elif [[ $mode != "$node_stamp" ]]; then
      echo "self-check: $node is $mode, originally $node_stamp" >&2
      bad=1
    fi
    other=${mode%% *}
    other=${other: -1}
    local recorded=${node_stamp%% *}
    # A node udev created world-writable is unchanged when the stamp matches.
    # Becoming world-writable after that stamp is a permission change.
    if [[ $other =~ ^[0-9]$ && $recorded =~ [0-9]$ ]]; then
      local recorded_other=${recorded: -1}
      if (( (other & 2) != 0 && (recorded_other & 2) == 0 )); then
        echo "self-check: $node is world-writable (mode $mode)" >&2
        bad=1
      fi
    elif [[ $other =~ ^[0-9]$ ]] && (( other & 2 )); then
      echo "self-check: $node is world-writable (mode $mode)" >&2
      bad=1
    fi
  done
  if [[ -f $PROC/started ]]; then
    while IFS=$'\t' read -r pid exe marker; do
      [[ $pid =~ ^[0-9]+$ && -n $exe ]] || continue
      if [[ -d /proc/$pid ]] && pid_matches_exe "$pid" "$exe"; then
        echo "self-check: $exe pid $pid is still running" >&2
        bad=1
      fi
    done <"$PROC/started"
  fi
  if (( bad )); then
    return 1
  fi
  if [[ -e /dev/uinput ]]; then
    echo "self-check: /dev/uinput $(stat -c '%a %U:%G' /dev/uinput) unchanged and not world-writable; no harness process left"
  else
    echo "self-check: /dev/uinput absent; no harness process left"
  fi
}

cleanup() {
  local status=$?
  # Subshells inherit this trap. Only the top-level shell tears the displays down.
  if [[ ${BASHPID:-} != "$MAIN_BASHPID" ]]; then
    return 0
  fi
  if [[ -n ${UINPUT_IN:-} ]]; then
    # printf is a builtin. A helper that has already exited delivers SIGPIPE
    # to this shell, and that would skip the rest of cleanup.
    trap '' PIPE
    printf 'quit\n' >&"$UINPUT_IN" 2>/dev/null || true
    trap - PIPE
    exec {UINPUT_IN}>&- || true
    unset UINPUT_IN
  fi
  if [[ -n ${UINPUT_OUT:-} ]]; then
    exec {UINPUT_OUT}<&- || true
    unset UINPUT_OUT
  fi
  if ! assert_harness_no_tcp_listen cleanup; then
    status=1
  fi
  if ! stop_tcp_watch; then
    status=1
  fi
  if [[ -n ${SOCK_IN:-} ]]; then
    trap '' PIPE
    printf 'quit\n' >&"$SOCK_IN" 2>/dev/null || true
    trap - PIPE
    exec {SOCK_IN}>&- || true
    unset SOCK_IN
  fi
  if [[ -n ${SOCK_OUT:-} ]]; then
    exec {SOCK_OUT}<&- || true
    unset SOCK_OUT
  fi
  # The visor pid file stays until stop_visor has seen the process exit.
  stop_visor || true
  signal_recorded_pid "$COMPANION_PID_FILE" "$COMPANION_BIN" || true
  stop_recorded xev.pid "$(command -v xev || true)"
  stop_recorded uinput.launcher "$(command -v sudo || true)"
  stop_recorded sock.pid "$(command -v python3 || true)"
  local comp_exe
  comp_exe=$(read_state compositor.exe)
  if [[ -n $comp_exe ]]; then
    stop_recorded compositor.pid "$comp_exe"
  fi
  kill_recorded_xorg || true
  stop_started
  restore_device_perms || true
  if ! self_check; then
    status=1
  fi
  remove_tree \
    "${HARNESS_RUNTIME_DIR:-}" \
    "${HARNESS_CONFIG_DIR:-}" \
    "${HARNESS_STATE_DIR:-}" \
    "${HARNESS_DATA_DIR:-}" \
    "${HARNESS_MCP_DIR:-}" \
    "${HARNESS_XDG:-}"
  exit "$status"
}
trap cleanup EXIT

# Control API. The companion socket stays on socket_action.
curl_visor() {
  command curl --unix-socket "$CONTROL_SOCK" "$@"
}

http_code() {
  local out=$1 method=$2 url=$3 body=${4:-}
  allow_write "$out" "$HTTP_BODY"
  if [[ -n $body ]]; then
    curl_visor --max-time 20 -sS -o "$HTTP_BODY" -w '%{http_code}' -X "$method" "$url" \
      -H 'content-type: application/json' --data-binary "$body" >"$out"
  else
    curl_visor --max-time 20 -sS -o "$HTTP_BODY" -w '%{http_code}' -X "$method" "$url" >"$out"
  fi
}

# Background a request and remember its pid for the scenario timeout.
spawn_request() {
  "$@" &
  SPAWN_PID=$!
  if [[ -n ${SCENARIO_BG:-} ]]; then
    allow_write "$SCENARIO_BG"
    echo "$SPAWN_PID" >>"$SCENARIO_BG"
  fi
}

phase() {
  curl_visor --max-time 5 -fsS "$BASE/companion" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["automation"]["phase"])'
}

injecting() {
  curl_visor --max-time 5 -fsS "$BASE/companion" \
    | python3 -c 'import json,sys; print(str(json.load(sys.stdin)["automation"]["injecting"]).lower())'
}

wait_phase() {
  local want=$1 tries=${2:-50} i got
  for i in $(seq 1 "$tries"); do
    got=$(phase)
    if [[ $got == "$want" ]]; then
      return 0
    fi
    sleep 0.1
  done
  echo "phase stayed $(phase), wanted $want" >&2
  return 1
}

socket_action_direct() {
  python3 - "$RUNTIME/companion.sock" "$1" <<'PY'
import socket, sys
path, action = sys.argv[1], sys.argv[2]
body = '{"action":"%s"}' % action
req = (
    "POST /companion/automation HTTP/1.1\r\n"
    "host: hypermesh\r\n"
    "content-type: application/json\r\n"
    "content-length: %d\r\n"
    "connection: close\r\n\r\n%s" % (len(body), body)
)
stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
stream.settimeout(2)
stream.connect(path)
stream.sendall(req.encode())
data = b""
while True:
    chunk = stream.recv(4096)
    if not chunk:
        break
    data += chunk
text = data.decode(errors="replace")
line = text.split("\r\n", 1)[0]
print(line)
if not line.startswith("HTTP/1.1 204"):
    sys.stderr.write(text)
    sys.exit(1)
PY
}

# The helper is a long-lived stdin client so a drag can be paused between
# pointer batches. Its pipe ends are ordinary fds, inherited by scenario
# subshells. A broken channel fails the scenario. An explicit err ack is the
# companion refusing the action, and callers may retry that.
#
# Every action is written before any ack is read. The helper posts the next
# one as soon as the previous response is in hand. Waiting for stop's ack in
# this shell, then writing resume, lets the in-flight type finish first.
# Resume on off is a successful no-op.
socket_actions() {
  local action
  if [[ -z ${SOCK_IN:-} && -z ${SOCK_OUT:-} ]]; then
    for action in "$@"; do
      socket_action_direct "$action"
    done
    return
  fi
  [[ $# -ge 1 ]] || die "socket helper needs an action"
  local ack lockfd failed=0
  [[ ${SOCK_IN:-} =~ ^[0-9]+$ && ${SOCK_OUT:-} =~ ^[0-9]+$ ]] || die "socket helper command channel is not open"
  # Builtin test. A forked readlink here is another gap before the write.
  if [[ ! -p /proc/${BASHPID:-$$}/fd/$SOCK_IN || ! -p /proc/${BASHPID:-$$}/fd/$SOCK_OUT ]]; then
    die "socket helper command channel is not open"
  fi
  exec {lockfd}>>"$PROC/sock.cmd.lock" || die "socket helper lock open failed"
  if ! flock -x "$lockfd"; then
    exec {lockfd}>&- || true
    die "socket helper lock failed"
  fi
  for action in "$@"; do
    if ! printf '%s\n' "$action" >&"$SOCK_IN"; then
      exec {lockfd}>&- || true
      die "socket helper write failed: $action"
    fi
  done
  for action in "$@"; do
    if ! IFS= read -r -t 2 ack <&"$SOCK_OUT"; then
      exec {lockfd}>&- || true
      die "socket helper did not answer: $action"
    fi
    if [[ -z ${ack:-} ]]; then
      exec {lockfd}>&- || true
      die "socket helper $action -> missing ack"
    fi
    if [[ $ack != ok* ]]; then
      echo "socket $action -> $ack" >&2
      failed=1
    fi
  done
  exec {lockfd}>&- || true
  return "$failed"
}

socket_action() {
  socket_actions "$1"
}

# HTTP body "error" field. Empty and non-JSON bodies yield an empty string.
error_text() {
  python3 - "$1" <<'PY'
import json, sys
raw = open(sys.argv[1], encoding="utf-8", errors="replace").read().strip()
if not raw:
    print("")
    raise SystemExit(0)
try:
    body = json.loads(raw)
except json.JSONDecodeError:
    print("")
    raise SystemExit(0)
if isinstance(body, dict):
    print(body.get("error", ""))
else:
    print("")
PY
}

sent_after() {
  local unit=$1
  python3 - "$HTTP_BODY" "$unit" <<'PY'
import json, re, sys
raw = open(sys.argv[1], encoding="utf-8", errors="replace").read().strip()
unit = sys.argv[2]
if not raw:
    print("")
    raise SystemExit(0)
try:
    body = json.loads(raw)
except json.JSONDecodeError:
    print("")
    raise SystemExit(0)
err = body.get("error", "") if isinstance(body, dict) else ""
print(err, file=sys.stderr)
match = re.search(r"after (\d+) " + re.escape(unit), err)
print(match.group(1) if match else "")
PY
}

open_session() {
  local body='{"purpose":"display harness","recipes":[],"agent":{"name":"display","instructions":""},"skills":[]}'
  SID=$(curl_visor --max-time 10 -fsS -X POST "$BASE/session" \
    -H 'content-type: application/json' -d "$body" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
  write_state session.id "$SID"
  echo "session $SID"
}

recorded_visor_alive() {
  local pid
  pid=$(read_state visor.pid)
  proc_still_ours "$pid" "$VISOR_BIN"
}

count_live_visors() {
  local pid exe marker n=0 seen=" "
  [[ -f $PROC/started ]] || {
    echo 0
    return 0
  }
  while IFS=$'\t' read -r pid exe marker; do
    [[ $exe == "$VISOR_BIN" ]] || continue
    [[ $seen == *" $pid "* ]] && continue
    if proc_still_ours "$pid" "$VISOR_BIN"; then
      n=$((n + 1))
      seen+="$pid "
    fi
  done <"$PROC/started"
  echo "$n"
}

# Signal every recorded visor and keep visor.pid until /proc shows it is gone.
# companion.lock is not touched here.
stop_visor() {
  local pid exe marker failed=0
  pid=$(read_state visor.pid)
  if [[ $pid =~ ^[0-9]+$ ]] && proc_still_ours "$pid" "$VISOR_BIN"; then
    wait_for_exit "$pid" "$VISOR_BIN" || failed=1
  fi
  if [[ -f $PROC/started ]]; then
    while IFS=$'\t' read -r pid exe marker; do
      [[ $exe == "$VISOR_BIN" ]] || continue
      if proc_still_ours "$pid" "$VISOR_BIN"; then
        wait_for_exit "$pid" "$VISOR_BIN" || failed=1
      fi
    done <"$PROC/started"
  fi
  if (( failed )) || recorded_visor_alive || (( $(count_live_visors) > 0 )); then
    echo "visor is still running; leaving visor.pid in place" >&2
    return 1
  fi
  clear_state visor.pid
  clear_state control.sock
  clear_state tcp.base
  VISOR_PID=
}

# `tcp` also opens the opt-in loopback port. The control socket is always used
# for the harness requests above. A live recorded visor is left in place unless
# this call is switching it to TCP. companion.lock is removed only after every
# recorded visor has exited.
start_visor() {
  local tcp=${1:-} n
  load_proc_state
  n=$(count_live_visors)
  if (( n > 1 )); then
    echo "self-check: $n harness visors are running" >&2
    return 1
  fi
  if [[ -z $tcp ]] && recorded_visor_alive; then
    if (( n != 1 )); then
      echo "self-check: recorded visor is alive but the started list has $n" >&2
      return 1
    fi
    return 0
  fi
  if recorded_visor_alive || (( n > 0 )); then
    if ! stop_visor; then
      echo "refusing to unlink companion.lock while a harness visor is still running" >&2
      return 1
    fi
  fi
  n=$(count_live_visors)
  if recorded_visor_alive || (( n > 0 )); then
    echo "refusing to unlink companion.lock while a harness visor is still running" >&2
    return 1
  fi
  mkdir -p "$RUNTIME" "$CONFIG"
  chmod_inside 0700 "$RUNTIME" "$CONFIG"
  remove_inside \
    "$RUNTIME/control.sock" \
    "$RUNTIME/companion.sock" \
    "$RUNTIME/companion.lock" \
    "$RUNTIME/api.token"
  allow_write "$DISPLAY_LOG_DIR/visor.out" "$DISPLAY_LOG_DIR/visor.err" "$VISOR_PID_FILE"
  local -a args=()
  if [[ $tcp == tcp ]]; then
    args=(--tcp --listen 127.0.0.1:0)
  fi
  "$VISOR_BIN" "${args[@]}" \
    >"$DISPLAY_LOG_DIR/visor.out" 2>"$DISPLAY_LOG_DIR/visor.err" &
  VISOR_PID=$!
  note_started "$VISOR_PID" "$VISOR_BIN"
  write_state visor.pid "$VISOR_PID"
  local i line
  for i in $(seq 1 50); do
    if [[ -s $DISPLAY_LOG_DIR/visor.out ]]; then
      line=$(awk '/^control socket / { print substr($0, 16); exit }' "$DISPLAY_LOG_DIR/visor.out")
      if [[ -n $line && -S $line ]]; then
        CONTROL_SOCK=$line
        BASE=http://localhost
        write_state control.sock "$CONTROL_SOCK"
        if [[ $tcp == tcp ]]; then
          TCP_BASE=$(awk '/^listening / { print "http://"$2; exit }' "$DISPLAY_LOG_DIR/visor.out")
          if [[ -z $TCP_BASE ]]; then
            sleep 0.1
            continue
          fi
          write_state tcp.base "$TCP_BASE"
        elif grep -q '^listening ' "$DISPLAY_LOG_DIR/visor.out"; then
          echo "TCP opened without --tcp" >&2
          return 1
        fi
        echo "visor $CONTROL_SOCK"
        n=$(count_live_visors)
        if (( n != 1 )); then
          echo "self-check: $n harness visors are running after start" >&2
          return 1
        fi
        return 0
      fi
    fi
    if ! kill -0 "$VISOR_PID" 2>/dev/null; then
      echo "visor exited" >&2
      cat "$DISPLAY_LOG_DIR/visor.err" >&2 || true
      return 1
    fi
    sleep 0.1
  done
  echo "visor did not listen" >&2
  return 1
}

stop_companion() {
  if signal_recorded_pid "$COMPANION_PID_FILE" "$COMPANION_BIN"; then
    sleep 0.2
  fi
}

start_companion() {
  local resume=${1:-}
  local mode=${2:-x11}
  stop_companion
  allow_write "$DISPLAY_LOG_DIR/companion.log" "$COMPANION_PID_FILE"
  # The script already cleared HYPERMESH_AUTOMATION_RESUME_SECS. A direct
  # exec keeps that. `env -u` reported the binary missing on the runner
  # even when this same path executed.
  if [[ -n $resume ]]; then
    HYPERMESH_AUTOMATION_RESUME_SECS=$resume "$COMPANION_BIN" \
      >"$DISPLAY_LOG_DIR/companion.log" 2>&1 &
  else
    "$COMPANION_BIN" >"$DISPLAY_LOG_DIR/companion.log" 2>&1 &
  fi
  local pid=$!
  # The companion treats SIGHUP as a stop. Disown it so a scenario
  # subshell can exit without taking the process down.
  disown "$pid" 2>/dev/null || true
  note_started "$pid" "$COMPANION_BIN"
  write_state companion.pid "$pid"
  write_state companion.was-up 1
  COMPANION_WAS_UP=1
  local i
  for i in $(seq 1 50); do
    if xwininfo -name Hypermesh >/dev/null 2>&1; then
      break
    fi
    if ! kill -0 "$(cat "$COMPANION_PID_FILE")" 2>/dev/null; then
      echo "companion exited" >&2
      cat "$DISPLAY_LOG_DIR/companion.log" >&2 || true
      return 1
    fi
    sleep 0.1
  done
  xwininfo -name Hypermesh >/dev/null 2>&1 || {
    echo "companion window did not appear" >&2
    cat "$DISPLAY_LOG_DIR/companion.log" >&2 || true
    return 1
  }
  # Wayland refuses start. The window and a live process are the ready check.
  if [[ $mode == wayland ]]; then
    return 0
  fi
  local code
  # A leftover user pause rejects TCP start with 403. The socket may clear
  # it. TCP start is the ready signal only once the phase is not running,
  # because that is when a missing poll is a 503.
  socket_action stop || true
  for i in $(seq 1 50); do
    http_code "$REQ_DIR/ready.code" POST "$BASE/companion/automation" '{"action":"start"}'
    code=$(cat "$REQ_DIR/ready.code")
    if [[ $code == 204 ]]; then
      socket_action stop
      wait_phase off
      return 0
    fi
    if [[ $code == 403 ]]; then
      socket_action stop || true
      sleep 0.1
      continue
    fi
    if [[ $code == 501 ]]; then
      echo "companion poll returned $code" >&2
      cat "$HTTP_BODY" >&2 || true
      return 1
    fi
    sleep 0.1
  done
  echo "companion did not complete a poll" >&2
  cat "$HTTP_BODY" >&2 || true
  return 1
}

ensure_companion() {
  local pid=
  if [[ -f $COMPANION_PID_FILE ]]; then
    pid=$(cat "$COMPANION_PID_FILE")
  fi
  if pid_matches_exe "$pid" "$COMPANION_BIN" && kill -0 "$pid" 2>/dev/null; then
    return 0
  fi
  start_companion
}

ensure_running() {
  ensure_companion
  local got
  got=$(phase)
  if [[ $got == running ]]; then
    return 0
  fi
  if [[ $got == paused ]]; then
    socket_action resume
  else
    socket_action start
  fi
  wait_phase running
}

ensure_off() {
  socket_action stop
  wait_phase off
}

window_height() {
  xwininfo -name Hypermesh | awk '/^  Height:/ { print $2; exit }'
}

window_origin() {
  xwininfo -name Hypermesh | awk '
    /Absolute upper-left X:/ { x = $NF }
    /Absolute upper-left Y:/ { y = $NF }
    END { print x, y }
  '
}

uinput_cmd() {
  local ack lockfd kind
  [[ ${UINPUT_IN:-} =~ ^[0-9]+$ && ${UINPUT_OUT:-} =~ ^[0-9]+$ ]] || die "uinput command channel is not open"
  # Check before opening the lock file. A closed descriptor number can be
  # reused by that open, and the command would then land in the lock file.
  for kind in "$UINPUT_IN" "$UINPUT_OUT"; do
    if [[ ! -e /proc/${BASHPID:-$$}/fd/$kind ]] || [[ $(readlink "/proc/${BASHPID:-$$}/fd/$kind" 2>/dev/null || true) != pipe:* ]]; then
      die "uinput command channel is not open"
    fi
  done
  allow_write "$PROC/uinput.cmd.lock"
  exec {lockfd}>>"$PROC/uinput.cmd.lock" || die "uinput lock open failed"
  if ! flock -x "$lockfd"; then
    exec {lockfd}>&- || true
    die "uinput lock failed"
  fi
  if ! printf '%s\n' "$*" >&"$UINPUT_IN"; then
    exec {lockfd}>&- || true
    die "uinput write failed: $*"
  fi
  if ! IFS= read -r -t 2 ack <&"$UINPUT_OUT"; then
    exec {lockfd}>&- || true
    die "uinput did not answer: $*"
  fi
  exec {lockfd}>&- || true
  if [[ -z ${ack:-} || $ack != ok ]]; then
    die "uinput $* -> ${ack:-missing ack}"
  fi
}

warp() {
  local target_x=$1 target_y=$2 i loc cx cy dx dy
  for i in $(seq 1 40); do
    loc=$(xdotool getmouselocation --shell)
    cx=$(awk -F= '/^X=/{print $2}' <<<"$loc")
    cy=$(awk -F= '/^Y=/{print $2}' <<<"$loc")
    dx=$((target_x - cx))
    dy=$((target_y - cy))
    if (( dx > -4 && dx < 4 && dy > -4 && dy < 4 )); then
      return 0
    fi
    # Flat acceleration is 1:1, so a long move across the wide root
    # does not need a one-pixel step.
    (( dx > 800 )) && dx=800
    (( dx < -800 )) && dx=-800
    (( dy > 800 )) && dy=800
    (( dy < -800 )) && dy=-800
    uinput_cmd "rel $dx $dy"
    sleep 0.03
  done
  echo "pointer stayed at $cx,$cy, wanted $target_x,$target_y" >&2
  return 1
}

xev_mark() {
  XEV_AT=$(wc -c < "$XEV_LOG")
}

xev_since() {
  tail -c +$((XEV_AT + 1)) "$XEV_LOG"
}

focus_xev() {
  local id
  id=$(xdotool search --name "Event Tester" | head -n 1)
  [[ -n $id ]] || die "xev window missing"
  # No window manager, so _NET_ACTIVE_WINDOW is absent. Focus is enough.
  xdotool windowfocus --sync "$id" >/dev/null || true
}

query_state() {
  local id=$1 out
  allow_write "$DISPLAY_LOG_DIR/xinput.err"
  if ! out=$(xinput query-state "$id" 2>"$DISPLAY_LOG_DIR/xinput.err"); then
    echo "xinput query-state $id failed" >&2
    cat "$DISPLAY_LOG_DIR/xinput.err" >&2 || true
    xinput list >&2 || true
    exit 1
  fi
  printf '%s\n' "$out"
}

button_is_down() {
  # query-state only opens extension devices. XTEST is where injected
  # buttons land; the virtual core pointer is not one of those devices.
  query_state "$XTEST_POINTER" | grep -q 'button\[1\]=down'
}

key_is_down() {
  local code=$1
  # The whole token. A shorter pattern would treat key[1]=down as key 1
  # inside key[10]=down, or stop at the first bracket.
  query_state "$XTEST_KEYBOARD" | grep -q "key\\[$code\\]=down"
}

# KeyPress records only. KeyRelease and a later event's keysym do not count.
count_keypress() {
  local sym=$1
  awk -v sym="$sym" '
    /^KeyPress / { kind = "press"; next }
    /^KeyRelease / { kind = "release"; next }
    /^[A-Za-z]/ { kind = ""; next }
    kind == "press" && index($0, sym) { n++ }
    END { print n + 0 }
  '
}

keycode_for() {
  xmodmap -pke | awk -v name="$1" '$4 == name { print $2; exit }'
}

capture_shot() {
  local name=$1
  allow_write \
    "$DISPLAY_LOG_DIR/${name}-root.xwd" \
    "$DISPLAY_LOG_DIR/${name}-companion.xwd" \
    "$DISPLAY_LOG_DIR/${name}-xinput.txt" \
    "$DISPLAY_LOG_DIR/${name}.png"
  if [[ -n ${DISPLAY:-} ]] && command -v xwd >/dev/null 2>&1; then
    xwd -root -silent -out "$DISPLAY_LOG_DIR/${name}-root.xwd" 2>/dev/null || true
    xwd -name Hypermesh -silent -out "$DISPLAY_LOG_DIR/${name}-companion.xwd" 2>/dev/null || true
    xinput list >"$DISPLAY_LOG_DIR/${name}-xinput.txt" 2>/dev/null || true
  fi
  if [[ -n ${WAYLAND_DISPLAY:-} ]] && command -v grim >/dev/null 2>&1; then
    grim "$DISPLAY_LOG_DIR/${name}.png" 2>/dev/null || true
  fi
}

# ss -ltnp, limited to processes this run started. A visor launched with
# --tcp is the opt-in token listener and is the only allowed match.
# command -v python3 is often a symlink to python3.N. Resolve both sides.
harness_same_exe() {
  local pid=$1 expected=$2 exe resolved
  pid_matches_exe "$pid" "$expected" && return 0
  exe=$(proc_executable "$pid" || true)
  resolved=$(readlink -f "$expected" 2>/dev/null || true)
  [[ -n $exe && -n $resolved && $exe == "$resolved" ]]
}

harness_tcp_allowed() {
  local pid=$1 cmd
  harness_same_exe "$pid" "$VISOR_BIN" || return 1
  cmd=$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null || true)
  [[ $cmd == *"--tcp"* ]]
}

assert_harness_no_tcp_listen() {
  local label=${1:-run}
  command -v ss >/dev/null || die "ss is required to check harness listeners"
  local -A exe_of=()
  local pid exe line bad=0 ss_out seen_pid
  if [[ -f ${PROC:-}/started ]]; then
    while IFS=$'\t' read -r pid exe _marker; do
      [[ $pid =~ ^[0-9]+$ && -n $exe ]] || continue
      exe_of[$pid]=$exe
    done <"$PROC/started"
  fi
  # The harness processes are this user. sudo ss walks every process on
  # the machine and is slow enough to disturb a stop/resume during a type.
  ss_out=$(ss -H -ltnp 2>/dev/null || true)
  while IFS= read -r line; do
    [[ $line == LISTEN* ]] || continue
    for seen_pid in $(grep -oE 'pid=[0-9]+' <<<"$line" | cut -d= -f2); do
      [[ -n ${exe_of[$seen_pid]:-} ]] || continue
      harness_same_exe "$seen_pid" "${exe_of[$seen_pid]}" || continue
      if harness_tcp_allowed "$seen_pid"; then
        continue
      fi
      echo "harness pid $seen_pid (${exe_of[$seen_pid]}) is listening: $line" >&2
      bad=1
    done
  done <<<"$ss_out"
  if (( bad )); then
    return 1
  fi
  if [[ $label != quiet ]]; then
    echo "harness listeners ($label): ss -ltnp has no harness TCP socket"
  fi
}

start_tcp_watch() {
  [[ -z ${TCP_WATCH_PID:-} ]] || return 0
  command -v ss >/dev/null || die "ss is required to check harness listeners"
  allow_write "$PROC/tcp-watch.fail" "$PROC/tcp-watch.stop" "$DISPLAY_LOG_DIR/tcp-watch.err"
  remove_inside "$PROC/tcp-watch.fail" "$PROC/tcp-watch.stop"
  (
    set -euo pipefail
    trap - EXIT
    while [[ ! -f $PROC/tcp-watch.stop ]]; do
      if ! assert_harness_no_tcp_listen quiet >"$DISPLAY_LOG_DIR/tcp-watch.err" 2>&1; then
        allow_write "$PROC/tcp-watch.fail"
        cp "$DISPLAY_LOG_DIR/tcp-watch.err" "$PROC/tcp-watch.fail"
        exit 1
      fi
      sleep 0.5
    done
  ) &
  TCP_WATCH_PID=$!
  note_started "$TCP_WATCH_PID" "$(command -v bash)" "tcp-watch"
}

stop_tcp_watch() {
  local status=0
  if [[ -n ${PROC:-} ]]; then
    allow_write "$PROC/tcp-watch.stop"
    : >"$PROC/tcp-watch.stop"
  fi
  if [[ -n ${TCP_WATCH_PID:-} ]]; then
    wait "$TCP_WATCH_PID" 2>/dev/null || true
    TCP_WATCH_PID=
  fi
  if [[ -n ${PROC:-} && -s $PROC/tcp-watch.fail ]]; then
    echo "harness process listened on TCP during the run" >&2
    cat "$PROC/tcp-watch.fail" >&2 || true
    status=1
  fi
  return "$status"
}

require_harness_no_tcp_listen() {
  local label=$1
  if ! assert_harness_no_tcp_listen "$label"; then
    die "harness process is listening on TCP"
  fi
  if [[ -s ${PROC:-}/tcp-watch.fail ]]; then
    cat "$PROC/tcp-watch.fail" >&2 || true
    die "harness process listened on TCP during the run"
  fi
}

run_named() {
  local name=$1 limit=$2
  shift 2
  require_harness_no_tcp_listen "$name"
  echo "=== $name ==="
  local flag="$DISPLAY_LOG_DIR/$name.running"
  local req=$DISPLAY_LOG_DIR/scenarios/$name
  mkdir -p "$req"
  allow_write "$req/bg.pids" "$flag" "$DISPLAY_LOG_DIR/$name.log" "$DISPLAY_LOG_DIR/$name.pid"
  : >"$req/bg.pids"
  touch "$flag"
  (
    sleep "$limit"
    if [[ -f $flag && -f $DISPLAY_LOG_DIR/$name.pid ]]; then
      allow_write "$DISPLAY_LOG_DIR/$name.log"
      echo "TIMEOUT $name after ${limit}s" >>"$DISPLAY_LOG_DIR/$name.log"
      # Not a function: `local` would abort this subshell before the kill.
      pid=$(cat "$DISPLAY_LOG_DIR/$name.pid")
      # The scenario shell is one process group. Background requests are
      # another, because monitor mode is on, so they are killed from bg.pids.
      kill -TERM -- -"$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
      kill_tracked_requests "$req/bg.pids"
    fi
  ) &
  local timer=$!
  set +e
  (
    # The parent cleared -e so `wait` can collect the status. Turn it back
    # on here, or a failed companion start still looks like a pass.
    # Monitor mode stays on so the companion can be disowned out of this
    # group. Request jobs are recorded in bg.pids instead.
    set -euo pipefail
    trap - EXIT
    REQ_DIR=$req
    HTTP_BODY=$req/http.body
    SCENARIO_BG=$req/bg.pids
    # Shell variables from the parent are a copy. Process ids live in $PROC.
    load_proc_state
    "$@"
  ) >"$DISPLAY_LOG_DIR/$name.log" 2>&1 &
  local test_pid=$!
  echo "$test_pid" >"$DISPLAY_LOG_DIR/$name.pid"
  wait "$test_pid"
  local status=$?
  set -e
  remove_inside "$flag" "$DISPLAY_LOG_DIR/$name.pid"
  # A request that outlived the scenario must not keep injecting, and its
  # files live under this scenario's directory either way.
  kill_tracked_requests "$req/bg.pids"
  kill "$timer" 2>/dev/null || true
  wait "$timer" 2>/dev/null || true
  require_harness_no_tcp_listen "$name"
  if [[ $status -eq 0 ]]; then
    echo "PASS $name"
    return 0
  fi
  echo "FAIL $name (exit $status)" >&2
  cat "$DISPLAY_LOG_DIR/$name.log" >&2 || true
  capture_shot "$name" || true
  FAILED+=("$name")
}

xorg_is_ours() {
  local pid=$1 cmd log ppid launcher
  exe_is_xorg "$pid" || return 1
  started_by_this_run "$pid" || return 1
  log=$(read_state xorg.log)
  cmd=$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null || true)
  if [[ -n $log && $cmd == *"$log"* ]]; then
    return 0
  fi
  launcher=$(read_state xorg.launcher)
  ppid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ' || true)
  [[ -n $launcher && $ppid == "$launcher" ]] && started_by_this_run "$launcher"
}

resolve_xorg_pid() {
  local launcher=$1 child grand
  if exe_is_xorg "$launcher"; then
    printf '%s\n' "$launcher"
    return 0
  fi
  for child in $(ps -o pid= --ppid "$launcher" 2>/dev/null || true); do
    if exe_is_xorg "$child"; then
      printf '%s\n' "$child"
      return 0
    fi
    for grand in $(ps -o pid= --ppid "$child" 2>/dev/null || true); do
      if exe_is_xorg "$grand"; then
        printf '%s\n' "$grand"
        return 0
      fi
    done
  done
  return 1
}

# sudo kill only after /proc says this pid is the Xorg this run launched.
kill_recorded_xorg() {
  local pid launcher
  pid=$(read_state xorg.pid)
  if [[ $pid =~ ^[0-9]+$ ]]; then
    if xorg_is_ours "$pid"; then
      sudo -n kill "$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
    else
      echo "not signalling pid $pid; it is not the Xorg this run started" >&2
    fi
  fi
  launcher=$(read_state xorg.launcher)
  if [[ $launcher =~ ^[0-9]+$ && $launcher != "$pid" ]]; then
    signal_if_exe "$launcher" "$(command -v sudo || true)" || \
      signal_if_exe "$launcher" "$(command -v Xorg || true)" || true
  fi
  if [[ -n ${XORG_DISP_FD:-} ]]; then
    exec {XORG_DISP_FD}>&- || true
    unset XORG_DISP_FD
  fi
  clear_state xorg.pid
  clear_state xorg.launcher
  clear_state xorg.display
  XORG_PID=
}

wait_for_display() {
  local i
  [[ ${DISPLAY:-} == :* ]] || return 1
  for i in $(seq 1 40); do
    if xdpyinfo >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.15
  done
  return 1
}

# -displayfd picks a free display. The lock file under /tmp is never read.
launch_xorg() {
  local conf=$1 log=$2 as_root=${3:-}
  local disp_file=$PROC/xorg.display.num number="" i xpid xexe
  allow_write "$log" "$disp_file"
  : >"$disp_file"
  write_state xorg.log "$log"
  if [[ $as_root == root ]]; then
    sudo -n bash -c 'exec 3>"$1"; shift; exec Xorg -displayfd 3 "$@"' \
      bash "$disp_file" \
      -ac -noreset -novtswitch -sharevts -nolisten tcp \
      -config "$conf" -logfile "$log" >/dev/null 2>&1 &
    local launcher=$!
    note_started "$launcher" "$(command -v sudo)"
    write_state xorg.launcher "$launcher"
  else
    if [[ -n ${XORG_DISP_FD:-} ]]; then
      exec {XORG_DISP_FD}>&- || true
      unset XORG_DISP_FD
    fi
    exec {XORG_DISP_FD}>"$disp_file"
    Xorg -displayfd "$XORG_DISP_FD" \
      -ac -noreset -novtswitch -sharevts -nolisten tcp \
      -config "$conf" -logfile "$log" >/dev/null 2>&1 &
    local launcher=$!
    note_started "$launcher" "$(command -v Xorg)"
    write_state xorg.launcher "$launcher"
  fi
  for i in $(seq 1 40); do
    number=$(tr -dc '0-9' <"$disp_file" || true)
    if [[ $number =~ ^[0-9]+$ ]]; then
      break
    fi
    kill -0 "$launcher" 2>/dev/null || return 1
    sleep 0.15
  done
  [[ $number =~ ^[0-9]+$ ]] || return 1
  write_state xorg.display "$number"
  export DISPLAY=:$number
  xpid=
  for i in $(seq 1 20); do
    xpid=$(resolve_xorg_pid "$launcher" || true)
    [[ -n $xpid ]] && break
    sleep 0.1
  done
  [[ -n $xpid ]] || return 1
  exe_is_xorg "$xpid" || return 1
  xexe=$(proc_executable "$xpid")
  note_started "$xpid" "$xexe" "$log"
  write_state xorg.pid "$xpid"
  XORG_PID=$xpid
}

start_xorg() {
  local conf=$1 log=$2 as_root=""
  allow_write "$log"
  kill_recorded_xorg
  if [[ -n ${UINPUT_POINTER:-} && ! -r $UINPUT_POINTER ]]; then
    echo "uinput event nodes are not readable by ${USER:-this user}; starting Xorg as root. Device modes are not changed." >&2
    as_root=root
  fi
  if launch_xorg "$conf" "$log" "$as_root" && wait_for_display; then
    return 0
  fi
  kill_recorded_xorg
  echo "retrying Xorg as root" >&2
  launch_xorg "$conf" "$log" root && wait_for_display
}

have_physical() {
  local list
  list=$(xinput list)
  grep -q 'hypermesh-display-pointer' <<<"$list" \
    && grep -q 'hypermesh-display-keyboard' <<<"$list"
}

write_evdev_conf() {
  allow_write "$DISPLAY_LOG_DIR/xorg-evdev.conf"
  cat >"$DISPLAY_LOG_DIR/xorg-evdev.conf" <<EOF
Section "ServerFlags"
    Option "DontVTSwitch" "true"
    Option "AllowEmptyInput" "false"
    Option "AutoAddDevices" "false"
    Option "AutoEnableDevices" "true"
    Option "BlankTime" "0"
    Option "StandbyTime" "0"
    Option "SuspendTime" "0"
    Option "OffTime" "0"
EndSection

Section "Device"
    Identifier "dummy"
    Driver "dummy"
    VideoRam 131072
    Option "IgnoreEDID" "true"
    Option "NoDDC" "true"
EndSection

Section "Monitor"
    Identifier "monitor"
    HorizSync 5.0-1000.0
    VertRefresh 5.0-200.0
    Modeline "16384x1024" 218.76 16384 16416 16448 16480 1024 1025 1028 1031 -hsync +vsync
EndSection

Section "Screen"
    Identifier "screen"
    Device "dummy"
    Monitor "monitor"
    DefaultDepth 24
    SubSection "Display"
        Depth 24
        Modes "16384x1024"
        Virtual 16384 1024
    EndSubSection
EndSection

Section "InputDevice"
    Identifier "hypermesh-display-pointer"
    Driver "evdev"
    Option "Device" "$UINPUT_POINTER"
    Option "Mode" "Relative"
    Option "AccelerationScheme" "none"
    Option "AccelerationProfile" "-1"
EndSection

Section "InputDevice"
    Identifier "hypermesh-display-keyboard"
    Driver "evdev"
    Option "Device" "$UINPUT_KEYBOARD"
EndSection

Section "ServerLayout"
    Identifier "layout"
    Screen "screen"
    InputDevice "hypermesh-display-pointer" "CorePointer"
    InputDevice "hypermesh-display-keyboard" "CoreKeyboard"
EndSection
EOF
}

# A descendant of `launcher` whose /proc identity matches `expected`.
resolve_child_pid() {
  local launcher=$1 expected=$2
  local -a current=("$launcher") next=()
  local depth=0 pid child
  while (( depth < 4 && ${#current[@]} )); do
    next=()
    for pid in "${current[@]}"; do
      if (( depth > 0 )) && pid_matches_exe "$pid" "$expected"; then
        printf '%s\n' "$pid"
        return 0
      fi
      for child in $(ps -o pid= --ppid "$pid" 2>/dev/null || true); do
        next+=("$child")
      done
    done
    current=()
    if ((${#next[@]})); then
      current=("${next[@]}")
    fi
    depth=$((depth + 1))
  done
  if pid_matches_exe "$launcher" "$expected"; then
    printf '%s\n' "$launcher"
    return 0
  fi
  return 1
}

start_uinput() {
  remember_uinput
  sudo -n modprobe uinput 2>/dev/null || true
  remember_uinput
  if [[ ! -e /dev/uinput ]]; then
    echo "uinput: /dev/uinput is missing after modprobe. Physical input is not faked with XTEST." >&2
    return 1
  fi
  assert_uinput_mode || return 1
  # The helper runs as root with stdin as the only command channel. It does
  # not listen, and it does not change device modes. Coproc fds are closed
  # in the scenario subshells, so the ends are duplicated onto ordinary fds
  # and the coproc originals are closed. Subshells inherit the duplicates.
  local helper=$root/ci/display/uinput.py
  allow_write "$DISPLAY_LOG_DIR/uinput.err" "$PROC/uinput.cmd.lock"
  coproc UINPUT_PROC {
    sudo -n python3 -u "$helper" 2>"$DISPLAY_LOG_DIR/uinput.err"
  }
  local launcher=$UINPUT_PROC_PID
  local coproc_out=${UINPUT_PROC[0]}
  local coproc_in=${UINPUT_PROC[1]}
  # Ordinary fds survive the scenario subshell. The {array[i]} close form
  # stores -1 and leaves the coproc descriptor open, so close the numbers.
  exec {UINPUT_IN}>&"${UINPUT_PROC[1]}" {UINPUT_OUT}<&"${UINPUT_PROC[0]}"
  exec {coproc_out}<&- {coproc_in}>&-
  local py_bin
  py_bin=$(command -v python3)
  note_started "$launcher" "$(command -v sudo)"
  write_state uinput.launcher "$launcher"
  local line="" py="" expected
  if ! IFS= read -r -t 8 line <&"$UINPUT_OUT"; then
    echo "uinput helper did not become ready" >&2
    cat "$DISPLAY_LOG_DIR/uinput.err" >&2 || true
    return 1
  fi
  if [[ $line == ready\ pointer=* ]]; then
    UINPUT_POINTER=$(sed -n 's/.*pointer=\([^ ]*\).*/\1/p' <<<"$line")
    UINPUT_KEYBOARD=$(sed -n 's/.*keyboard=\([^ ]*\).*/\1/p' <<<"$line")
  fi
  if [[ -z ${UINPUT_POINTER:-} || -z ${UINPUT_KEYBOARD:-} ]]; then
    echo "uinput helper did not become ready: $line" >&2
    cat "$DISPLAY_LOG_DIR/uinput.err" >&2 || true
    return 1
  fi
  for expected in "$py_bin" python3; do
    py=$(resolve_child_pid "$launcher" "$expected" || true)
    if [[ -n $py ]]; then
      note_started "$py" "$expected" "$helper"
      write_state uinput.pid "$py"
      UINPUT_PID=$py
      break
    fi
  done
  if [[ -z $py ]]; then
    echo "uinput helper is up but its python pid was not identified" >&2
    return 1
  fi
  write_state uinput.pointer "$UINPUT_POINTER"
  write_state uinput.keyboard "$UINPUT_KEYBOARD"
  sudo -n udevadm settle 2>/dev/null || true
  local node key mode i
  for key in uinput.pointer uinput.keyboard; do
    node=$(read_state "$key")
    for i in $(seq 1 30); do
      [[ -e $node ]] && break
      sleep 0.1
    done
    if [[ ! -e $node ]]; then
      echo "uinput: $node did not appear" >&2
      return 1
    fi
    mode=$(stat -c '%a %U %G' "$node")
    if [[ -z $(read_state "$key.mode") ]]; then
      write_state "$key.mode" "$mode"
    fi
    echo "uinput node $node mode $mode (not changed)"
  done
  echo "uinput pointer=$UINPUT_POINTER keyboard=$UINPUT_KEYBOARD"
}

probe_physical_move() {
  xset s off -dpms || true
  xset m 1 1 || true
  local id before after
  id=$(xinput list --id-only 'hypermesh-display-pointer' 2>/dev/null | head -n 1 || true)
  if [[ -n $id ]]; then
    xinput set-prop "$id" "libinput Accel Profile Enabled" 0 1 >/dev/null 2>&1 || true
  fi
  before=$(xdotool getmouselocation --shell | awk -F= '/^X=/{print $2}')
  uinput_cmd "rel 40 0"
  sleep 0.2
  after=$(xdotool getmouselocation --shell | awk -F= '/^X=/{print $2}')
  echo "PHYSICAL driver=$PHYSICAL_DRIVER pointer-x $before -> $after"
  if [[ $before == "$after" ]]; then
    echo "uinput relative move did not move the core pointer" >&2
    xinput list >&2 || true
    return 1
  fi
  uinput_cmd "rel -40 0" || true
}

build_bins() {
  cargo build --locked --workspace --bins
  [[ -x $VISOR_BIN ]] || die "visor binary missing after cargo build"
  [[ -x $COMPANION_BIN ]] || die "companion binary missing after cargo build"
}

start_socket_helper() {
  # Stdin is the only command channel. Coproc fds are closed in scenario
  # subshells, so the ends are duplicated onto ordinary fds and the coproc
  # originals are closed. There is no listener and no port file.
  allow_write "$DISPLAY_LOG_DIR/sock.err" "$PROC/sock.cmd.lock"
  local helper=$root/ci/display/sock.py
  coproc SOCK_PROC {
    python3 -u "$helper" "$RUNTIME/companion.sock" 2>"$DISPLAY_LOG_DIR/sock.err"
  }
  local launcher=$SOCK_PROC_PID
  local coproc_out=${SOCK_PROC[0]}
  local coproc_in=${SOCK_PROC[1]}
  exec {SOCK_IN}>&"${SOCK_PROC[1]}" {SOCK_OUT}<&"${SOCK_PROC[0]}"
  exec {coproc_out}<&- {coproc_in}>&-
  note_started "$launcher" "$(command -v python3)" "$helper"
  write_state sock.pid "$launcher"
  SOCK_PID=$launcher
  local line=""
  if ! IFS= read -r -t 2 line <&"$SOCK_OUT"; then
    echo "socket helper did not become ready" >&2
    cat "$DISPLAY_LOG_DIR/sock.err" >&2 || true
    return 1
  fi
  if [[ $line != ready ]]; then
    echo "socket helper did not become ready: $line" >&2
    cat "$DISPLAY_LOG_DIR/sock.err" >&2 || true
    return 1
  fi
  echo "socket helper ready"
  assert_harness_no_tcp_listen "socket-helper" || return 1
}

capture_core_devices() {
  CORE_POINTER_ID=$(xinput list --id-only 'Virtual core pointer' | head -n 1)
  CORE_KEYBOARD_ID=$(xinput list --id-only 'Virtual core keyboard' | head -n 1)
  [[ -n $CORE_POINTER_ID && -n $CORE_KEYBOARD_ID ]] || die "core pointer or keyboard id missing"
  query_state "$XTEST_POINTER" >/dev/null
  query_state "$XTEST_KEYBOARD" >/dev/null
  read -r ROOT_W ROOT_H < <(xwininfo -root | awk '/Width:/ { w = $2 } /Height:/ { h = $2 } END { print w, h }')
  echo "core pointer=$CORE_POINTER_ID keyboard=$CORE_KEYBOARD_ID xtest='$XTEST_POINTER' root=${ROOT_W}x${ROOT_H}"
  (( ROOT_W >= 8000 && ROOT_H >= 800 )) || die "root ${ROOT_W}x${ROOT_H} is too small for a cancellable drag"
}

setup_x11() {
  unset WAYLAND_DISPLAY || true
  export XDG_SESSION_TYPE=x11
  mkdir -p "$CONFIG"
  remove_inside "$CONFIG/companion.toml"
  build_bins
  start_uinput
  if ! start_xorg "$root/ci/display/xorg-libinput.conf" "$DISPLAY_LOG_DIR/xorg-libinput.log"; then
    echo "Xorg did not start with the libinput config" >&2
    cat "$DISPLAY_LOG_DIR/xorg-libinput.log" >&2 || true
  fi
  if have_physical; then
    PHYSICAL_DRIVER=libinput
  else
    echo "libinput did not attach the uinput nodes; retrying with evdev on the same nodes" >&2
    xinput list >&2 || true
    write_evdev_conf
    if ! start_xorg "$DISPLAY_LOG_DIR/xorg-evdev.conf" "$DISPLAY_LOG_DIR/xorg-evdev.log"; then
      echo "Xorg did not start" >&2
      cat "$DISPLAY_LOG_DIR/xorg-evdev.log" >&2 || true
      return 1
    fi
    if ! have_physical; then
      echo "uinput nodes are not slave devices. Physical input was not faked with XTEST." >&2
      xinput list >&2 || true
      cat "$DISPLAY_LOG_DIR/xorg-evdev.log" >&2 || true
      return 1
    fi
    PHYSICAL_DRIVER=evdev
  fi
  probe_physical_move
  allow_write "$XEV_LOG"
  xev -geometry 500x400+40+40 >"$XEV_LOG" 2>&1 &
  XEV_PID=$!
  note_started "$XEV_PID" "$(command -v xev)"
  write_state xev.pid "$XEV_PID"
  local i
  for i in $(seq 1 50); do
    if xdotool search --name "Event Tester" >/dev/null 2>&1; then
      break
    fi
    sleep 0.1
  done
  xdotool search --name "Event Tester" >/dev/null 2>&1 || die "xev window did not appear"
  capture_core_devices
  start_visor
  start_socket_helper
  open_session
  echo "xinput:"
  xinput list
}

scenario_key_down_detects() {
  local code
  code=$(keycode_for a)
  [[ -n $code ]] || die "keymap has no a"
  if key_is_down "$code"; then
    xdotool keyup a || true
    key_is_down "$code" && die "a was already down"
  fi
  # Leave the key down. key_is_down has to report that, or a later check
  # that expects a release would pass while the key was still held.
  xdotool keydown a
  sleep 0.15
  if ! key_is_down "$code"; then
    query_state "$XTEST_KEYBOARD" >&2 || true
    xdotool keyup a || true
    die "key_is_down missed a held XTEST a (keycode $code)"
  fi
  xdotool keyup a
  sleep 0.1
  if key_is_down "$code"; then
    query_state "$XTEST_KEYBOARD" >&2 || true
    die "a stayed down after keyup"
  fi
  echo "key_is_down saw key[$code]=down"
}

scenario_stop_resume_during_type() {
  ensure_companion
  allow_write "$REQ_DIR/stop-resume-type.json"
  python3 - <<'PY' >"$REQ_DIR/stop-resume-type.json"
import json
print(json.dumps({"text": "q" * 2000}))
PY
  local attempt code
  for attempt in 1 2 3; do
    ensure_running
    spawn_request http_code "$REQ_DIR/type.code" POST "$BASE/session/$SID/type" @"$REQ_DIR/stop-resume-type.json"
    local curl_pid=$SPAWN_PID
    local i saw=0
    for i in $(seq 1 50); do
      if ! kill -0 "$curl_pid" 2>/dev/null; then
        break
      fi
      if [[ $(injecting) == true ]]; then
        saw=1
        break
      fi
      sleep 0.02
    done
    if (( saw == 0 )); then
      wait "$curl_pid" || true
      echo "attempt $attempt type ended before injection was visible"
      continue
    fi
    # Both lines are in the pipe before stop's ack comes back, so resume is
    # queued while the type is still inside its chunk.
    socket_actions stop resume
    wait "$curl_pid" || true
    code=$(cat "$REQ_DIR/type.code")
    echo "attempt $attempt type status $code $(error_text "$HTTP_BODY")"
    if [[ $code != 409 ]]; then
      echo "stop did not cancel the type"
      continue
    fi
    if wait_phase running 40; then
      http_code "$REQ_DIR/last.code" POST "$BASE/session/$SID/mouse" '{"action":"move","x":40,"y":40}'
      [[ $(cat "$REQ_DIR/last.code") == 204 ]] \
        || die "move after resume returned $(cat "$REQ_DIR/last.code") $(cat "$HTTP_BODY")"
      [[ $(phase) == running ]] || die "move after resume left $(phase)"
      echo "stop then resume during injection left running"
      return 0
    fi
    echo "attempt $attempt ended at $(phase)"
  done
  die "stop then resume during injection did not leave running"
}

scenario_off_bug_toggles_menu() {
  ensure_companion
  ensure_off
  local ox oy h h_off h_menu h_back prev i
  # Stopping drops the continue prompt and the window moves down with it.
  # Wait until that height has settled, then aim at the bug that is on screen.
  prev=0
  for i in $(seq 1 20); do
    h=$(window_height)
    if [[ $h == "$prev" ]]; then
      break
    fi
    prev=$h
    sleep 0.2
  done
  read -r ox oy < <(window_origin)
  warp $((ox + 30)) $((oy + 30))
  read -r ox oy < <(window_origin)
  warp $((ox + 30)) $((oy + 30))
  h_off=$(window_height)
  echo "off window at $ox,$oy height $h_off"
  uinput_cmd click
  h_menu=$h_off
  for i in $(seq 1 30); do
    h_menu=$(window_height)
    if (( h_menu > h_off + 40 )); then
      break
    fi
    sleep 0.1
  done
  (( h_menu > h_off + 40 )) || die "off bug click did not open the menu ($h_off -> $h_menu)"
  [[ $(phase) == off ]] || die "menu click moved the phase to $(phase)"
  uinput_cmd click
  h_back=$h_menu
  for i in $(seq 1 30); do
    h_back=$(window_height)
    if (( h_back < h_off + 40 )); then
      echo "menu toggle $h_off -> $h_menu -> $h_back"
      return 0
    fi
    sleep 0.1
  done
  die "second off click did not close the menu ($h_menu -> $h_back)"
}

scenario_tcp_start_without_poll() {
  stop_companion
  if [[ -n $COMPANION_WAS_UP ]]; then
    sleep 2.2
  fi
  http_code "$REQ_DIR/tcp-start.code" POST "$BASE/companion/automation" '{"action":"start"}'
  local code
  code=$(cat "$REQ_DIR/tcp-start.code")
  [[ $code == 503 ]] || die "control-socket start returned $code ($(cat "$HTTP_BODY"))"
  grep -q 'no recent companion poll' "$HTTP_BODY" || die "body $(cat "$HTTP_BODY")"
  [[ $(phase) == off ]] || die "phase became $(phase)"
}

scenario_inject_lands() {
  ensure_companion
  ensure_running
  focus_xev
  xev_mark
  http_code "$REQ_DIR/last.code" POST "$BASE/session/$SID/mouse" '{"action":"move","x":80,"y":90}'
  [[ $(cat "$REQ_DIR/last.code") == 204 ]] || die "move $(cat "$REQ_DIR/last.code") $(cat "$HTTP_BODY")"
  http_code "$REQ_DIR/last.code" POST "$BASE/session/$SID/mouse" '{"action":"click","x":100,"y":120,"button":"left"}'
  [[ $(cat "$REQ_DIR/last.code") == 204 ]] || die "click $(cat "$REQ_DIR/last.code")"
  http_code "$REQ_DIR/last.code" POST "$BASE/session/$SID/mouse" '{"action":"drag","x":60,"y":70,"to_x":180,"to_y":200,"button":"left"}'
  [[ $(cat "$REQ_DIR/last.code") == 204 ]] || die "drag $(cat "$REQ_DIR/last.code")"
  http_code "$REQ_DIR/last.code" POST "$BASE/session/$SID/type" '{"text":"Hi","keys":[{"key":"return","action":"tap"}]}'
  [[ $(cat "$REQ_DIR/last.code") == 204 ]] || die "type $(cat "$REQ_DIR/last.code") $(cat "$HTTP_BODY")"
  sleep 0.4
  local log presses
  log=$(xev_since)
  presses=$(grep -c 'ButtonPress' <<<"$log" || true)
  grep -q 'MotionNotify' <<<"$log" || die "xev saw no motion"
  (( presses >= 2 )) || die "xev saw $presses button presses"
  grep -q 'keysym 0x48, H' <<<"$log" || die "xev missed H"
  grep -q 'keysym 0x69, i' <<<"$log" || die "xev missed i"
  grep -q 'keysym 0xff0d' <<<"$log" || die "xev missed Return"
  [[ $(phase) == running ]] || die "injected input paused automation ($(phase))"
}

scenario_scroll_lands() {
  ensure_companion
  ensure_running
  focus_xev
  xev_mark
  local delta code i log button
  for delta in '-2,-3' '2,3'; do
    http_code "$REQ_DIR/last.code" POST "$BASE/session/$SID/mouse" \
      "{\"action\":\"scroll\",\"x\":100,\"y\":120,\"delta_x\":${delta%,*},\"delta_y\":${delta#*,}}"
    [[ $(cat "$REQ_DIR/last.code") == 204 ]] || die "scroll $(cat "$HTTP_BODY")"
  done
  # Wait for the X event stream, then verify both press and release counts.
  for i in $(seq 1 40); do
    log=$(xev_since)
    if [[ $(grep -c 'ButtonRelease' <<<"$log" || true) -ge 10 ]]; then break; fi
    sleep 0.1
  done
  for button in 4 5 6 7; do
    local expected=2
    [[ $button == 4 || $button == 5 ]] && expected=3
    local kind count
    for kind in ButtonPress ButtonRelease; do
      count=$(awk -v kind="$kind" -v button="$button" '
        /^[A-Za-z]+ event/ { wanted = ($1 == kind) }
        wanted && $0 ~ "button " button "," { n++; wanted=0 }
        END { print n+0 }
      ' <<<"$log")
      [[ $count == "$expected" ]] || die "$kind button $button: $count, expected $expected"
    done
  done
  [[ $(phase) == running ]] || die "synthetic scroll paused automation"
}

scenario_scroll_editor() {
  ensure_companion
  ensure_running
  local python editor_pid i fraction log=$DISPLAY_LOG_DIR/scroll-editor.log
  python=$(readlink -f "$(command -v python3)")
  allow_write "$log"
  "$python" "$root/ci/display/scroll-editor.py" >"$log" 2>&1 &
  editor_pid=$!
  note_started "$editor_pid" "$python" "$root/ci/display/scroll-editor.py"
  for i in $(seq 1 40); do
    if xwininfo -name 'Visor scroll editor' >/dev/null 2>&1 && grep -q '^0.0$' "$log"; then break; fi
    sleep 0.1
  done
  xwininfo -name 'Visor scroll editor' >/dev/null 2>&1 || die "scroll editor did not open"
  http_code "$REQ_DIR/last.code" POST "$BASE/session/$SID/mouse" '{"action":"scroll","x":650,"y":120,"delta_x":0,"delta_y":3}'
  [[ $(cat "$REQ_DIR/last.code") == 204 ]] || die "editor scroll down $(cat "$HTTP_BODY")"
  local moved=0
  for i in $(seq 1 40); do
    fraction=$(tail -n 1 "$log")
    if awk -v value="$fraction" 'BEGIN { exit !(value+0 > 0) }'; then moved=1; break; fi
    sleep 0.1
  done
  [[ $moved == 1 ]] || die "editor did not scroll down: $(cat "$log")"
  http_code "$REQ_DIR/last.code" POST "$BASE/session/$SID/mouse" '{"action":"scroll","x":650,"y":120,"delta_x":0,"delta_y":-3}'
  [[ $(cat "$REQ_DIR/last.code") == 204 ]] || die "editor scroll up $(cat "$HTTP_BODY")"
  for i in $(seq 1 40); do
    [[ $(tail -n 1 "$log") == 0.0 ]] && break
    sleep 0.1
  done
  [[ $(tail -n 1 "$log") == 0.0 ]] || die "editor did not return to the top"
  wait_for_exit "$editor_pid" "$python" "$root/ci/display/scroll-editor.py"
}

scenario_uinput_channel() {
  ensure_companion
  ensure_running
  # Same poll gap as the other physical-move scenarios: the companion has to
  # observe running before the pointer move, or the move does not pause it.
  sleep 0.5
  local before after
  before=$(xdotool getmouselocation --shell | awk -F= '/^X=/{print $2}')
  uinput_cmd "rel 40 0"
  sleep 0.2
  after=$(xdotool getmouselocation --shell | awk -F= '/^X=/{print $2}')
  [[ $before != "$after" ]] || die "uinput move did not move the pointer ($before)"
  wait_phase paused 40
  echo "companion saw the uinput move ($before -> $after) and paused"
  # start while paused is 403, before the "no recent poll" check. Leave the
  # phase off, which is where the visor was before this scenario.
  ensure_off
}

scenario_uinput_key_pauses_type() {
  ensure_companion
  focus_xev
  allow_write "$REQ_DIR/long-type.json"
  python3 - <<'PY' >"$REQ_DIR/long-type.json"
import json
print(json.dumps({"text": "q" * 2000}))
PY
  local attempt code n
  for attempt in 1 2 3; do
    ensure_running
    xev_mark
    spawn_request http_code "$REQ_DIR/type.code" POST "$BASE/session/$SID/type" @"$REQ_DIR/long-type.json"
    local curl_pid=$SPAWN_PID
    sleep 0.05
    local spins=0
    while kill -0 "$curl_pid" 2>/dev/null; do
      uinput_cmd "key F12"
      spins=$((spins + 1))
      if (( spins > 100 )); then
        break
      fi
      sleep 0.02
    done
    wait "$curl_pid" || true
    code=$(cat "$REQ_DIR/type.code")
    n=$(sent_after keystrokes)
    echo "attempt $attempt status $code sent ${n:-none}"
    if [[ $code == 409 && -n $n && $n -gt 0 && $n -lt 4000 ]]; then
      sleep 0.3
      local qpress
      # One KeyPress per character down. Release lines and other events
      # are ignored. The server's sent count is the ceiling: a full 2000
      # q characters are 4000 keystrokes, and xev must not keep going.
      qpress=$(xev_since | count_keypress 'keysym 0x71, q')
      echo "xev q KeyPress $qpress sent $n"
      if (( qpress == 0 )); then
        echo "xev had not logged the q KeyPress yet"
        continue
      fi
      if (( qpress > n + 64 )); then
        die "xev KeyPress count $qpress exceeded the $n keystrokes the server sent"
      fi
      return 0
    fi
  done
  die "physical key did not cancel the rest of the type"
}

scenario_uinput_move_pauses() {
  ensure_companion
  ensure_running
  # The visor reports running as soon as resume lands. The companion learns
  # that on its next poll. A move before that poll is input while the bug
  # still shows paused, and it does not pause the new stretch.
  sleep 0.5
  uinput_cmd "rel 30 12"
  wait_phase paused 40
  http_code "$REQ_DIR/mouse.code" POST "$BASE/session/$SID/mouse" '{"action":"move","x":12,"y":12}'
  [[ $(cat "$REQ_DIR/mouse.code") == 409 ]] || die "mouse returned $(cat "$REQ_DIR/mouse.code")"
  grep -q 'automation paused' "$HTTP_BODY" || die "body $(cat "$HTTP_BODY")"
}

scenario_pausing_click_does_not_ask() {
  ensure_companion
  ensure_off
  local ox oy h_run h_paused h_ask i
  read -r ox oy < <(window_origin)
  warp $((ox + 30)) $((oy + 30))
  # Let that move finish while automation is off, so the click itself is the pause.
  sleep 0.4
  socket_action start
  wait_phase running
  sleep 0.3
  h_run=$(window_height)
  uinput_cmd click
  h_paused=$h_run
  for i in $(seq 1 40); do
    [[ $(phase) == paused ]] || { sleep 0.1; continue; }
    h_paused=$(window_height)
    if (( h_paused > h_run + 20 && h_paused < h_run + 90 )); then
      break
    fi
    sleep 0.1
  done
  [[ $(phase) == paused ]] || die "bug click did not pause"
  h_paused=$(window_height)
  (( h_paused > h_run + 20 && h_paused < h_run + 90 )) \
    || die "pausing click height $h_run -> $h_paused"
  sleep 0.5
  [[ $(window_height) == "$h_paused" ]] || die "continue opened on the pausing click"
  uinput_cmd click
  h_ask=$h_paused
  for i in $(seq 1 40); do
    h_ask=$(window_height)
    if (( h_ask >= h_paused + 100 )); then
      echo "continue prompt height $h_paused -> $h_ask"
      return 0
    fi
    sleep 0.1
  done
  die "later bug click did not open continue ($h_paused -> $h_ask)"
}

scenario_xtest_click_does_nothing() {
  ensure_companion
  ensure_running
  sleep 0.3
  local h ox oy
  h=$(window_height)
  read -r ox oy < <(window_origin)
  xdotool mousemove --sync $((ox + 30)) $((oy + 40)) click 1
  sleep 0.6
  [[ $(phase) == running ]] || die "XTEST click paused automation ($(phase))"
  [[ $(window_height) == "$h" ]] || die "XTEST click resized the companion ($h -> $(window_height))"
}

scenario_watchdog_pauses() {
  ensure_companion
  ensure_running
  local pid start now got
  pid=$(cat "$COMPANION_PID_FILE")
  pid_matches_exe "$pid" "$COMPANION_BIN" || die "companion pid $pid is not $COMPANION_BIN"
  kill "$pid"
  remove_inside "$COMPANION_PID_FILE"
  start=$(date +%s)
  got=running
  local i
  for i in $(seq 1 30); do
    got=$(phase || true)
    if [[ $got == paused ]]; then
      now=$(date +%s)
      echo "watchdog paused after $((now - start))s"
      (( now - start <= 5 )) || die "watchdog exceeded 5s"
      return 0
    fi
    sleep 0.2
  done
  die "watchdog left phase ${got:-unknown}"
}

scenario_held_release() {
  ensure_companion
  ensure_running
  http_code "$REQ_DIR/last.code" POST "$BASE/session/$SID/type" \
    '{"keys":[{"key":"control","action":"down"},{"key":"a","action":"down"}]}'
  [[ $(cat "$REQ_DIR/last.code") == 204 ]] || die "held type $(cat "$REQ_DIR/last.code") $(cat "$HTTP_BODY")"
  local control a
  control=$(keycode_for Control_L)
  a=$(keycode_for a)
  [[ -n $control && -n $a ]] || die "keymap has no Control_L or a"
  if key_is_down "$control" || key_is_down "$a"; then
    query_state "$XTEST_KEYBOARD" >&2 || true
    die "finished type left a key down"
  fi

  allow_write "$REQ_DIR/held-type.json"
  python3 - <<'PY' >"$REQ_DIR/held-type.json"
import json
keys = [{"key": "control", "action": "down"}]
keys += [{"key": "b", "action": "tap"} for _ in range(1500)]
print(json.dumps({"keys": keys}))
PY
  local attempt code n
  for attempt in 1 2 3; do
    ensure_running
    spawn_request http_code "$REQ_DIR/type.code" POST "$BASE/session/$SID/type" @"$REQ_DIR/held-type.json"
    local curl_pid=$SPAWN_PID
    local i
    sleep 0.02
    for i in $(seq 1 400); do
      if ! kill -0 "$curl_pid" 2>/dev/null; then
        break
      fi
      socket_action pause || true
    done
    wait "$curl_pid" || true
    code=$(cat "$REQ_DIR/type.code")
    n=$(sent_after keystrokes)
    echo "key cancel attempt $attempt status $code sent ${n:-none}"
    if [[ $code == 409 && -n $n && $n -gt 0 ]]; then
      if key_is_down "$control"; then
        query_state "$XTEST_KEYBOARD" >&2 || true
        die "cancel left control down"
      fi
      break
    fi
    if (( attempt == 3 )); then
      die "could not cancel a held key"
    fi
  done

  # A short drag finishes before a pause can land. The root is wide so
  # this is hundreds of pointer steps, and the socket helper pauses between
  # batches. sent must include the button-down and exclude the button-up.
  local to_x=$((ROOT_W - 1)) to_y=$((ROOT_H / 2))
  (( to_x <= 32767 )) || to_x=32767
  local points=$(( (to_x + 63) / 64 ))
  local total=$((points + 3))
  local drag_ok=0
  local delay
  for attempt in 1 2 3 4 5; do
    case $attempt in
      1) delay=0.02 ;;
      2) delay=0.05 ;;
      3) delay=0.01 ;;
      4) delay=0.08 ;;
      *) delay=0 ;;
    esac
    ensure_running
    spawn_request http_code "$REQ_DIR/drag.code" POST "$BASE/session/$SID/mouse" \
      "{\"action\":\"drag\",\"x\":0,\"y\":0,\"to_x\":$to_x,\"to_y\":$to_y,\"button\":\"left\"}"
    curl_pid=$SPAWN_PID
    sleep "$delay"
    for i in $(seq 1 400); do
      if ! kill -0 "$curl_pid" 2>/dev/null; then
        break
      fi
      socket_action pause || true
    done
    wait "$curl_pid" || true
    code=$(cat "$REQ_DIR/drag.code")
    n=$(sent_after "pointer steps")
    echo "drag cancel attempt $attempt status $code sent ${n:-none} total $total $(error_text "$HTTP_BODY")"
    if button_is_down; then
      query_state "$XTEST_POINTER" >&2 || true
      die "left button stayed down"
    fi
    if [[ $code == 409 && -n $n && $n -ge 2 && $n -lt $total ]]; then
      drag_ok=1
      break
    fi
  done
  (( drag_ok == 1 )) || die "could not cancel a drag while the button was down"
  if button_is_down; then
    die "left button stayed down after cancel"
  fi
}

scenario_auto_resume() {
  stop_companion
  start_companion 2
  ensure_running
  # The visor reports running as soon as the socket start lands. The
  # companion learns that on its next poll. A move before that poll is
  # input while the bug still shows off, and it does not pause.
  sleep 0.5
  uinput_cmd "rel 28 6"
  wait_phase paused 40
  sleep 0.4
  [[ $(phase) == paused ]] || die "resumed before the idle timeout"
  wait_phase running 80
  echo "idle resume returned to running"
}

# A second account. sudo --chdir is not permitted, so commands run from /tmp.
as_guest() {
  ( cd /tmp && sudo -n -u hm-visor-guest "$@" )
}

ensure_guest() {
  if ! id -u hm-visor-guest >/dev/null 2>&1; then
    sudo useradd --system --no-create-home --shell /usr/sbin/nologin hm-visor-guest
  fi
}

assert_no_tcp_listen() {
  python3 - "$1" <<'PY'
import os, pathlib, sys
pid = int(sys.argv[1])
inodes = set()
for entry in pathlib.Path(f"/proc/{pid}/fd").iterdir():
    try:
        target = os.readlink(entry)
    except OSError:
        continue
    if target.startswith("socket:[") and target.endswith("]"):
        inodes.add(target[8:-1])
for name in ("tcp", "tcp6"):
    path = pathlib.Path("/proc/net") / name
    if not path.exists():
        continue
    for line in path.read_text().splitlines()[1:]:
        parts = line.split()
        if len(parts) > 9 and parts[3] == "0A" and parts[9] in inodes:
            sys.stderr.write(line + "\n")
            raise SystemExit(1)
PY
}

guest_unix_denied() {
  local sock=$1 err=$REQ_DIR/guest-connect.err rc=0
  allow_write "$err"
  as_guest /usr/bin/python3 - "$sock" >"$err" 2>&1 <<'PY' || rc=$?
import socket, sys
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    client.connect(sys.argv[1])
except OSError as exc:
    sys.stderr.write(f"{type(exc).__name__}: {exc}\n")
    raise SystemExit(0)
sys.stderr.write("connected\n")
raise SystemExit(1)
PY
  [[ $rc -eq 0 ]] || die "guest connected to $sock ($(cat "$err"))"
  grep -q PermissionError "$err" || die "guest connect to $sock was not EACCES ($(cat "$err"))"
}

# Last in the list: it restarts the visor with the opt-in port.
scenario_other_user_cannot_drive() {
  ensure_guest
  start_visor || die "visor did not listen"
  [[ -S $CONTROL_SOCK ]] || die "control socket is not up"
  [[ -S $RUNTIME/companion.sock ]] || die "companion socket is not up"
  if grep -q '^listening ' "$DISPLAY_LOG_DIR/visor.out"; then
    die "TCP opened without --tcp"
  fi
  assert_no_tcp_listen "$VISOR_PID" || die "the visor is listening on TCP"
  guest_unix_denied "$CONTROL_SOCK"
  guest_unix_denied "$RUNTIME/companion.sock"
  local dir_err=$REQ_DIR/guest-dir.err dir_rc=0
  allow_write "$dir_err"
  as_guest /usr/bin/python3 - "$RUNTIME" >"$dir_err" 2>&1 <<'PY' || dir_rc=$?
import os, sys
try:
    os.listdir(sys.argv[1])
except PermissionError as exc:
    sys.stderr.write(f"{type(exc).__name__}: {exc}\n")
    raise SystemExit(0)
raise SystemExit(1)
PY
  [[ $dir_rc -eq 0 ]] || die "guest listed the runtime directory ($(cat "$dir_err"))"

  stop_visor
  start_visor tcp || die "visor did not open the opt-in port"
  local token_file=$RUNTIME/api.token token mode
  token=$(tr -d '[:space:]' <"$token_file")
  mode=$(stat -c '%a' "$token_file")
  [[ $mode == 600 && ${#token} -eq 64 ]] || die "api.token mode=$mode len=${#token}"
  local token_err=$REQ_DIR/guest-token.err
  allow_write "$token_err" "$HTTP_BODY"
  if as_guest /bin/cat "$token_file" >/dev/null 2>"$token_err"; then
    die "guest read api.token"
  fi
  grep -qi 'permission denied' "$token_err" || die "guest token read ($(cat "$token_err"))"

  local path code
  for path in /session /session/00000000-0000-0000-0000-000000000001/view \
    /session/00000000-0000-0000-0000-000000000001/mouse \
    /session/00000000-0000-0000-0000-000000000001/type \
    /companion/automation /health; do
    code=$(as_guest /usr/bin/curl -sS --max-time 5 -o /dev/null -w '%{http_code}' \
      -X POST "$TCP_BASE$path" -H 'content-type: application/json' -d '{}')
    [[ $code == 401 ]] || die "guest $path status $code"
    code=$(as_guest /usr/bin/curl -sS --max-time 5 -o /dev/null -w '%{http_code}' \
      -X POST "$TCP_BASE$path" -H 'content-type: application/json' \
      -H 'authorization: Bearer not-the-token' -d '{}')
    [[ $code == 401 ]] || die "guest $path with a wrong token status $code"
  done
  code=$(command curl -sS --max-time 5 -o "$HTTP_BODY" -w '%{http_code}' \
    -X POST "$TCP_BASE/companion/automation" \
    -H 'content-type: application/json' \
    -H "authorization: Bearer $token" \
    -d '{"action":"pause"}')
  [[ $code == 204 ]] || die "pause with the bearer returned $code ($(cat "$HTTP_BODY"))"
  code=$(command curl -sS --max-time 5 -o "$HTTP_BODY" -w '%{http_code}' \
    -X POST "$TCP_BASE/companion/automation" \
    -H 'content-type: application/json' \
    -H "authorization: Bearer $token" \
    -d '{"action":"resume"}')
  # X11 refuses the lift with 403. Wayland refuses resume before that check.
  if [[ $code == 403 ]]; then
    :
  elif [[ $code == 501 ]] && grep -q '#17' "$HTTP_BODY"; then
    :
  else
    die "bearer resume returned $code ($(cat "$HTTP_BODY"))"
  fi
  echo "other user cannot drive the control socket, the companion socket, or the opted-in port"
}

X11_SCENARIOS=(
  uinput-channel
  key-down-detects
  tcp-start-without-poll
  inject-lands
  scroll-lands
  scroll-editor
  stop-resume-during-type
  uinput-key-pauses-type
  uinput-move-pauses
  pausing-click-does-not-ask
  off-bug-toggles-menu
  xtest-click-does-nothing
  watchdog-pauses
  held-release
  auto-resume
  other-user-cannot-drive
)

WAYLAND_SCENARIOS=(
  wayland-refuses-automation
  portal-libei
  other-user-cannot-drive
)

require_scenario() {
  local only=$1
  shift
  local name
  [[ -z $only ]] && return 0
  for name in "$@"; do
    [[ $name == "$only" ]] && return 0
  done
  echo "unknown scenario: $only" >&2
  echo "valid scenarios: $*" >&2
  exit 1
}

run_x11() {
  local only=${1:-}
  require_scenario "$only" "${X11_SCENARIOS[@]}"
  setup_x11
  local name limit ran=0
  for name in "${X11_SCENARIOS[@]}"; do
    if [[ -n $only && $only != "$name" ]]; then
      continue
    fi
    case $name in
      uinput-key-pauses-type | held-release | stop-resume-during-type | other-user-cannot-drive) limit=60 ;;
      auto-resume) limit=30 ;;
      key-down-detects) limit=20 ;;
      *) limit=45 ;;
    esac
    run_named "$name" "$limit" "scenario_${name//-/_}"
    ran=$((ran + 1))
  done
  if (( ran == 0 )); then
    echo "no scenarios ran" >&2
    echo "valid scenarios: ${X11_SCENARIOS[*]}" >&2
    exit 1
  fi
  echo "PHYSICAL driver=$PHYSICAL_DRIVER"
}

x11_sockets() {
  ls /tmp/.X11-unix 2>/dev/null | sed 's/^X/:/' | sort || true
}

runtime_sockets() {
  local dir=${XDG_RUNTIME_DIR:-}
  [[ -n $dir && -d $dir ]] || return 0
  find "$dir" -maxdepth 1 -type s -printf '%f\n' 2>/dev/null | sort
}

# Sway picks wayland-N itself. Weston is asked for hm-display.
discover_wayland_socket() {
  local before=$1 named fresh
  if [[ -f $DISPLAY_LOG_DIR/compositor.log ]]; then
    named=$(awk -F"'" '/wayland display/ { sock = $2 } END { print sock }' \
      "$DISPLAY_LOG_DIR/compositor.log")
    if [[ -n $named && -S ${XDG_RUNTIME_DIR}/$named ]]; then
      printf '%s\n' "$named"
      return 0
    fi
  fi
  fresh=$(comm -13 <(printf '%s\n' "$before") <(runtime_sockets) \
    | grep -E '^wayland-[0-9]+$' | head -n 1 || true)
  if [[ -n $fresh && -S ${XDG_RUNTIME_DIR}/$fresh ]]; then
    printf '%s\n' "$fresh"
    return 0
  fi
  return 1
}

wait_for_new_display() {
  local before_x11=$1 socket_mode=$2 label=$3 i sock now extra
  for i in $(seq 1 40); do
    sock=
    if [[ $socket_mode == hm-display ]]; then
      if [[ -S ${XDG_RUNTIME_DIR:-}/hm-display ]]; then
        sock=hm-display
      fi
    else
      sock=$(discover_wayland_socket "$WAYLAND_SOCK_BEFORE" || true)
    fi
    if [[ -n $sock ]]; then
      export WAYLAND_DISPLAY=$sock
      now=$(x11_sockets)
      extra=$(comm -13 <(printf '%s\n' "$before_x11") <(printf '%s\n' "$now") | head -n 1 || true)
      if [[ -n $extra ]]; then
        export DISPLAY=$extra
        echo "wayland compositor=$label DISPLAY=$DISPLAY WAYLAND_DISPLAY=$WAYLAND_DISPLAY"
        return 0
      fi
    fi
    if [[ -n ${COMPOSITOR_PID:-} ]] && ! kill -0 "$COMPOSITOR_PID" 2>/dev/null; then
      return 1
    fi
    sleep 0.25
  done
  return 1
}

start_compositor() {
  local before_x11=$1 socket_mode=$2
  shift 2
  allow_write "$DISPLAY_LOG_DIR/compositor.log"
  "$@" >"$DISPLAY_LOG_DIR/compositor.log" 2>&1 &
  COMPOSITOR_PID=$!
  local bin
  bin=$(command -v "$1" || true)
  [[ -n $bin ]] || bin=$1
  note_started "$COMPOSITOR_PID" "$bin"
  write_state compositor.pid "$COMPOSITOR_PID"
  write_state compositor.exe "$bin"
  wait_for_new_display "$before_x11" "$socket_mode" "$1"
}

start_wayland() {
  if path_inside_roots "$(owned_path "$XDG_RUNTIME_DIR")"; then
    mkdir -p "$XDG_RUNTIME_DIR"
    chmod_inside 0700 "$XDG_RUNTIME_DIR"
  elif [[ ! -d $XDG_RUNTIME_DIR ]]; then
    die "XDG_RUNTIME_DIR is not a directory this harness created"
  fi
  export XDG_SESSION_TYPE=wayland
  unset WAYLAND_DISPLAY || true
  export WLR_BACKENDS=headless
  export WLR_RENDERER=pixman
  export WLR_LIBINPUT_NO_DEVICES=1
  local before
  before=$(x11_sockets)
  allow_write "$DISPLAY_LOG_DIR/sway.conf"
  cat >"$DISPLAY_LOG_DIR/sway.conf" <<'EOF'
xwayland enable
output * resolution 1280x720
EOF
  # Sway ignores a client-side WAYLAND_DISPLAY and binds wayland-N.
  WAYLAND_SOCK_BEFORE=$(runtime_sockets)
  if start_compositor "$before" discover sway -c "$DISPLAY_LOG_DIR/sway.conf"; then
    return 0
  fi
  echo "sway did not provide Xwayland; trying weston headless" >&2
  cat "$DISPLAY_LOG_DIR/compositor.log" >&2 || true
  if [[ -n ${COMPOSITOR_PID:-} ]]; then
    signal_if_exe "$COMPOSITOR_PID" "$(read_state compositor.exe)" || true
    COMPOSITOR_PID=
    clear_state compositor.pid
    clear_state compositor.exe
  fi
  unset WAYLAND_DISPLAY || true
  before=$(x11_sockets)
  allow_write "$DISPLAY_LOG_DIR/weston.ini"
  cat >"$DISPLAY_LOG_DIR/weston.ini" <<'EOF'
[core]
backend=headless-backend.so
xwayland=true

[output]
name=headless
mode=1280x720
EOF
  WAYLAND_SOCK_BEFORE=$(runtime_sockets)
  if start_compositor "$before" hm-display weston --config "$DISPLAY_LOG_DIR/weston.ini" --socket=hm-display; then
    return 0
  fi
  echo "weston did not provide Xwayland" >&2
  cat "$DISPLAY_LOG_DIR/compositor.log" >&2 || true
  return 1
}

scenario_wayland_refuses_automation() {
  start_visor
  open_session
  start_companion "" wayland
  local action code
  for action in start resume; do
    http_code "$REQ_DIR/wl.code" POST "$BASE/companion/automation" "{\"action\":\"$action\"}"
    code=$(cat "$REQ_DIR/wl.code")
    [[ $code == 501 ]] || die "$action returned $code ($(cat "$HTTP_BODY"))"
    grep -q '#17' "$HTTP_BODY" || die "$action body $(cat "$HTTP_BODY")"
  done
  http_code "$REQ_DIR/wl.code" POST "$BASE/session/$SID/mouse" '{"action":"move","x":1,"y":1}'
  [[ $(cat "$REQ_DIR/wl.code") == 501 ]] || die "mouse returned $(cat "$REQ_DIR/wl.code")"
  grep -q '#17' "$HTTP_BODY" || die "mouse body $(cat "$HTTP_BODY")"
  http_code "$REQ_DIR/wl.code" POST "$BASE/session/$SID/type" '{"text":"no"}'
  [[ $(cat "$REQ_DIR/wl.code") == 501 ]] || die "type returned $(cat "$REQ_DIR/wl.code")"
  grep -q '#17' "$HTTP_BODY" || die "type body $(cat "$HTTP_BODY")"
  local i
  for i in $(seq 1 30); do
    if grep -q '#17' "$DISPLAY_LOG_DIR/companion.log"; then
      break
    fi
    sleep 0.2
  done
  grep -q '#17' "$DISPLAY_LOG_DIR/companion.log" || die "companion did not log the wayland note"
  kill -0 "$(cat "$COMPANION_PID_FILE")" || die "companion exited"
}

scenario_portal_libei_placeholder() {
  echo "SKIP portal-libei"
  echo "Portal and libei injection are not asserted in this job."
  echo "Issue #17 still refuses automation on Wayland."
  echo "When that backend lands, add named scenarios here for a portal remote-desktop session and for libei pointer and keyboard events, and keep the physical-input pause checks."
}

run_wayland() {
  local only=${1:-}
  require_scenario "$only" "${WAYLAND_SCENARIOS[@]}"
  build_bins
  start_wayland || die "no headless compositor"
  local name ran=0
  for name in "${WAYLAND_SCENARIOS[@]}"; do
    if [[ -n $only && $only != "$name" ]]; then
      continue
    fi
    case $name in
      portal-libei) run_named "$name" 15 scenario_portal_libei_placeholder ;;
      other-user-cannot-drive) run_named "$name" 60 "scenario_${name//-/_}" ;;
      *) run_named "$name" 45 "scenario_${name//-/_}" ;;
    esac
    ran=$((ran + 1))
  done
  if (( ran == 0 )); then
    echo "no scenarios ran" >&2
    echo "valid scenarios: ${WAYLAND_SCENARIOS[*]}" >&2
    exit 1
  fi
}

usage() {
  echo "usage: bash ci/display/run.sh x11 [scenario] | wayland [scenario]" >&2
  exit 2
}

main() {
  start_tcp_watch
  remember_uinput
  assert_uinput_mode || die "/dev/uinput mode is not the private mode recorded at start"
  local kind=${1:-}
  local only=${2:-}
  case $kind in
    x11) run_x11 "$only" ;;
    wayland) run_wayland "$only" ;;
    *) usage ;;
  esac
  if ((${#FAILED[@]})); then
    echo "failed: ${FAILED[*]}" >&2
    exit 1
  fi
  echo "display scenarios passed"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
