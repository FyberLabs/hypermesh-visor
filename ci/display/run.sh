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
# failure captures go to display-logs/.
set -euo pipefail
set -m

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

DISPLAY_LOG_DIR=$root/display-logs
mkdir -p "$DISPLAY_LOG_DIR"
HTTP_BODY=$DISPLAY_LOG_DIR/http.body
RUNTIME=${HYPERMESH_RUNTIME_DIR:-/tmp/hm-run}
CONFIG=${HYPERMESH_CONFIG_DIR:-/tmp/hm-config}
VISOR_BIN=$root/target/debug/hypermesh-visor
COMPANION_BIN=$root/target/debug/hypermesh-companion
COMPANION_PID_FILE=$DISPLAY_LOG_DIR/companion.pid
XEV_LOG=$DISPLAY_LOG_DIR/xev.log
XEV_AT=0
FAILED=()
PHYSICAL_DRIVER=
BASE=
SID=
UINPUT_FD=
UINPUT_PID=
SOCK_FD=
SOCK_PID=
XORG_PID=
XEV_PID=
VISOR_PID=
VISOR_PID_FILE=$DISPLAY_LOG_DIR/visor.pid
COMPOSITOR_PID=
COMPANION_WAS_UP=
CORE_POINTER_ID=
CORE_KEYBOARD_ID=
ROOT_W=
ROOT_H=

export HYPERMESH_RUNTIME_DIR=$RUNTIME
export HYPERMESH_CONFIG_DIR=$CONFIG
unset HYPERMESH_AUTOMATION_RESUME_SECS || true

die() {
  echo "FAIL: $*" >&2
  exit 1
}

MAIN_BASHPID=$BASHPID

cleanup() {
  local status=$?
  # Subshells inherit this trap. Only the top-level shell tears the displays down.
  if [[ ${BASHPID:-} != "$MAIN_BASHPID" ]]; then
    return 0
  fi
  if [[ -n ${UINPUT_FD:-} ]]; then
    printf 'quit\n' >&"$UINPUT_FD" 2>/dev/null || true
  fi
  if [[ -n ${XEV_PID:-} ]]; then
    kill "$XEV_PID" 2>/dev/null || true
  fi
  if [[ -f $COMPANION_PID_FILE ]]; then
    kill "$(cat "$COMPANION_PID_FILE")" 2>/dev/null || true
  fi
  if [[ -n ${VISOR_PID:-} ]]; then
    kill "$VISOR_PID" 2>/dev/null || true
  fi
  if [[ -f $VISOR_PID_FILE ]]; then
    kill "$(cat "$VISOR_PID_FILE")" 2>/dev/null || true
  fi
  if [[ -n ${SOCK_FD:-} ]]; then
    printf 'quit\n' >&"$SOCK_FD" 2>/dev/null || true
  fi
  if [[ -n ${UINPUT_PID:-} ]]; then
    kill "$UINPUT_PID" 2>/dev/null || true
  fi
  if [[ -n ${SOCK_PID:-} ]]; then
    kill "$SOCK_PID" 2>/dev/null || true
  fi
  if [[ -n ${COMPOSITOR_PID:-} ]]; then
    kill "$COMPOSITOR_PID" 2>/dev/null || true
  fi
  if [[ -f /tmp/.X99-lock ]]; then
    sudo kill "$(head -n 1 /tmp/.X99-lock)" 2>/dev/null || true
    sudo rm -f /tmp/.X99-lock /tmp/.X11-unix/X99 2>/dev/null || true
  fi
  if [[ -n ${XORG_PID:-} ]]; then
    kill "$XORG_PID" 2>/dev/null || sudo kill "$XORG_PID" 2>/dev/null || true
  fi
  exit "$status"
}
trap cleanup EXIT

http_code() {
  local out=$1 method=$2 url=$3 body=${4:-}
  if [[ -n $body ]]; then
    curl --max-time 20 -sS -o "$HTTP_BODY" -w '%{http_code}' -X "$method" "$url" \
      -H 'content-type: application/json' --data-binary "$body" >"$out"
  else
    curl --max-time 20 -sS -o "$HTTP_BODY" -w '%{http_code}' -X "$method" "$url" >"$out"
  fi
}

phase() {
  curl --max-time 5 -fsS "$BASE/companion" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["automation"]["phase"])'
}

injecting() {
  curl --max-time 5 -fsS "$BASE/companion" \
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

socket_action() {
  if [[ -n ${SOCK_FD:-} ]]; then
    printf '%s\n' "$1" >&"$SOCK_FD"
    local ack
    IFS= read -r -t 2 ack <&"$SOCK_FD" || die "socket helper did not answer: $1"
    if [[ $ack != ok* ]]; then
      echo "socket $1 -> $ack" >&2
      return 1
    fi
    return 0
  fi
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
  SID=$(curl --max-time 10 -fsS -X POST "$BASE/session" \
    -H 'content-type: application/json' -d "$body" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
  echo "session $SID"
}

start_visor() {
  if [[ -n ${VISOR_PID:-} ]] && kill -0 "$VISOR_PID" 2>/dev/null; then
    return 0
  fi
  mkdir -p "$RUNTIME" "$CONFIG"
  chmod 0700 "$RUNTIME" "$CONFIG"
  rm -f "$RUNTIME/companion.sock" "$RUNTIME/companion.lock"
  "$VISOR_BIN" --listen 127.0.0.1:0 \
    >"$DISPLAY_LOG_DIR/visor.out" 2>"$DISPLAY_LOG_DIR/visor.err" &
  VISOR_PID=$!
  echo "$VISOR_PID" >"$VISOR_PID_FILE"
  disown "$VISOR_PID" 2>/dev/null || true
  local i
  for i in $(seq 1 50); do
    if [[ -s $DISPLAY_LOG_DIR/visor.out ]]; then
      BASE=$(awk '/^listening / { print "http://"$2; exit }' "$DISPLAY_LOG_DIR/visor.out")
      if [[ -n ${BASE:-} ]]; then
        echo "visor $BASE"
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
  local pid=
  if [[ -f $COMPANION_PID_FILE ]]; then
    pid=$(cat "$COMPANION_PID_FILE")
  fi
  if [[ -n $pid ]]; then
    kill "$pid" 2>/dev/null || true
    sleep 0.2
  fi
  rm -f "$COMPANION_PID_FILE"
}

start_companion() {
  local resume=${1:-}
  local mode=${2:-x11}
  stop_companion
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
  echo "$pid" >"$COMPANION_PID_FILE"
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
  for i in $(seq 1 50); do
    http_code "$DISPLAY_LOG_DIR/ready.code" POST "$BASE/companion/automation" '{"action":"start"}'
    code=$(cat "$DISPLAY_LOG_DIR/ready.code")
    if [[ $code == 204 ]]; then
      socket_action stop
      wait_phase off
      return 0
    fi
    if [[ $code == 403 || $code == 501 ]]; then
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
  if [[ -n $pid ]] && kill -0 "$pid" 2>/dev/null; then
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
  printf '%s\n' "$*" >&"$UINPUT_FD"
  local ack
  IFS= read -r -t 2 ack <&"$UINPUT_FD" || die "uinput did not answer: $*"
  if [[ $ack != ok ]]; then
    die "uinput $* -> $ack"
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
  if ! out=$(xinput query-state "$id" 2>"$DISPLAY_LOG_DIR/xinput.err"); then
    echo "xinput query-state $id failed" >&2
    cat "$DISPLAY_LOG_DIR/xinput.err" >&2 || true
    xinput list >&2 || true
    exit 1
  fi
  printf '%s\n' "$out"
}

button_is_down() {
  query_state "$CORE_POINTER_ID" | grep -q 'button\[1\]=down'
}

key_is_down() {
  local code=$1 state
  state=$(query_state "$CORE_KEYBOARD_ID")
  python3 - "$code" "$state" <<'PY'
import sys
code, state = sys.argv[1], sys.argv[2]
nums = []
for line in state.splitlines():
    if "keys:" not in line:
        continue
    rest = line.split("keys:", 1)[1].strip()
    if rest and rest != "none":
        nums = rest.split()
    break
sys.exit(0 if code in nums else 1)
PY
}

keycode_for() {
  xmodmap -pke | awk -v name="$1" '$4 == name { print $2; exit }'
}

capture_shot() {
  local name=$1
  if [[ -n ${DISPLAY:-} ]] && command -v xwd >/dev/null 2>&1; then
    xwd -root -silent -out "$DISPLAY_LOG_DIR/${name}-root.xwd" 2>/dev/null || true
    xwd -name Hypermesh -silent -out "$DISPLAY_LOG_DIR/${name}-companion.xwd" 2>/dev/null || true
    xinput list >"$DISPLAY_LOG_DIR/${name}-xinput.txt" 2>/dev/null || true
  fi
  if [[ -n ${WAYLAND_DISPLAY:-} ]] && command -v grim >/dev/null 2>&1; then
    grim "$DISPLAY_LOG_DIR/${name}.png" 2>/dev/null || true
  fi
}

run_named() {
  local name=$1 limit=$2
  shift 2
  echo "=== $name ==="
  local flag="$DISPLAY_LOG_DIR/$name.running"
  touch "$flag"
  (
    sleep "$limit"
    if [[ -f $flag && -f $DISPLAY_LOG_DIR/$name.pid ]]; then
      echo "TIMEOUT $name after ${limit}s" >>"$DISPLAY_LOG_DIR/$name.log"
      local pid
      pid=$(cat "$DISPLAY_LOG_DIR/$name.pid")
      kill -TERM -- -"$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
    fi
  ) &
  local timer=$!
  set +e
  (
    # The parent cleared -e so `wait` can collect the status. Turn it back
    # on here, or a failed companion start still looks like a pass.
    set -euo pipefail
    trap - EXIT
    "$@"
  ) >"$DISPLAY_LOG_DIR/$name.log" 2>&1 &
  local test_pid=$!
  echo "$test_pid" >"$DISPLAY_LOG_DIR/$name.pid"
  wait "$test_pid"
  local status=$?
  set -e
  rm -f "$flag" "$DISPLAY_LOG_DIR/$name.pid"
  kill "$timer" 2>/dev/null || true
  wait "$timer" 2>/dev/null || true
  if [[ $status -eq 0 ]]; then
    echo "PASS $name"
    return 0
  fi
  echo "FAIL $name (exit $status)" >&2
  cat "$DISPLAY_LOG_DIR/$name.log" >&2 || true
  capture_shot "$name" || true
  FAILED+=("$name")
}

stop_xorg() {
  if [[ -f /tmp/.X99-lock ]]; then
    sudo kill "$(head -n 1 /tmp/.X99-lock)" 2>/dev/null || true
    sleep 0.2
    sudo rm -f /tmp/.X99-lock /tmp/.X11-unix/X99 2>/dev/null || true
  fi
  if [[ -n ${XORG_PID:-} ]]; then
    kill "$XORG_PID" 2>/dev/null || sudo kill "$XORG_PID" 2>/dev/null || true
  fi
  XORG_PID=
}

wait_for_display() {
  local i
  for i in $(seq 1 40); do
    if DISPLAY=:99 xdpyinfo >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.15
  done
  return 1
}

start_xorg() {
  local conf=$1 log=$2
  stop_xorg
  Xorg :99 -ac -noreset -novtswitch -sharevts -nolisten tcp \
    -config "$conf" -logfile "$log" >/dev/null 2>&1 &
  XORG_PID=$!
  if wait_for_display; then
    return 0
  fi
  stop_xorg
  sudo Xorg :99 -ac -noreset -novtswitch -sharevts -nolisten tcp \
    -config "$conf" -logfile "$log" >/dev/null 2>&1 &
  XORG_PID=$!
  wait_for_display
}

have_physical() {
  local list
  list=$(DISPLAY=:99 xinput list)
  grep -q 'hypermesh-display-pointer' <<<"$list" \
    && grep -q 'hypermesh-display-keyboard' <<<"$list"
}

write_evdev_conf() {
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

start_uinput() {
  sudo modprobe uinput 2>/dev/null || true
  if [[ ! -e /dev/uinput ]]; then
    echo "uinput: /dev/uinput is missing after modprobe. Physical input is not faked with XTEST." >&2
    return 1
  fi
  sudo chmod 0666 /dev/uinput || true
  python3 -u "$root/ci/display/uinput.py" \
    >"$DISPLAY_LOG_DIR/uinput.ready" 2>"$DISPLAY_LOG_DIR/uinput.err" &
  UINPUT_PID=$!
  local i line
  for i in $(seq 1 50); do
    if [[ -s $DISPLAY_LOG_DIR/uinput.ready ]]; then
      line=$(head -n 1 "$DISPLAY_LOG_DIR/uinput.ready")
      if [[ $line == ready* ]]; then
        UINPUT_PORT=$(sed -n 's/.*port=\([0-9]*\).*/\1/p' <<<"$line")
        UINPUT_POINTER=$(sed -n 's/.*pointer=\([^ ]*\).*/\1/p' <<<"$line")
        UINPUT_KEYBOARD=$(sed -n 's/.*keyboard=\([^ ]*\).*/\1/p' <<<"$line")
        break
      fi
    fi
    if ! kill -0 "$UINPUT_PID" 2>/dev/null; then
      echo "uinput helper exited" >&2
      cat "$DISPLAY_LOG_DIR/uinput.err" >&2 || true
      return 1
    fi
    sleep 0.1
  done
  if [[ -z ${UINPUT_PORT:-} ]]; then
    echo "uinput helper did not become ready" >&2
    cat "$DISPLAY_LOG_DIR/uinput.err" >&2 || true
    return 1
  fi
  sudo udevadm settle 2>/dev/null || true
  sudo chmod 0666 "$UINPUT_POINTER" "$UINPUT_KEYBOARD" || true
  exec {UINPUT_FD}<>/dev/tcp/127.0.0.1/"$UINPUT_PORT"
  echo "uinput pointer=$UINPUT_POINTER keyboard=$UINPUT_KEYBOARD"
}

probe_physical_move() {
  export DISPLAY=:99
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
  python3 -u "$root/ci/display/sock.py" "$RUNTIME/companion.sock" \
    >"$DISPLAY_LOG_DIR/sock.ready" 2>"$DISPLAY_LOG_DIR/sock.err" &
  SOCK_PID=$!
  local i line port
  for i in $(seq 1 50); do
    if [[ -s $DISPLAY_LOG_DIR/sock.ready ]]; then
      line=$(head -n 1 "$DISPLAY_LOG_DIR/sock.ready")
      if [[ $line == ready* ]]; then
        port=$(sed -n 's/.*port=\([0-9]*\).*/\1/p' <<<"$line")
        break
      fi
    fi
    if ! kill -0 "$SOCK_PID" 2>/dev/null; then
      echo "socket helper exited" >&2
      cat "$DISPLAY_LOG_DIR/sock.err" >&2 || true
      return 1
    fi
    sleep 0.05
  done
  if [[ -z ${port:-} ]]; then
    echo "socket helper did not become ready" >&2
    cat "$DISPLAY_LOG_DIR/sock.err" >&2 || true
    return 1
  fi
  exec {SOCK_FD}<>/dev/tcp/127.0.0.1/"$port"
  echo "socket helper port=$port"
}

capture_core_devices() {
  CORE_POINTER_ID=$(xinput list --id-only 'Virtual core pointer' | head -n 1)
  CORE_KEYBOARD_ID=$(xinput list --id-only 'Virtual core keyboard' | head -n 1)
  [[ -n $CORE_POINTER_ID && -n $CORE_KEYBOARD_ID ]] || die "core pointer or keyboard id missing"
  query_state "$CORE_POINTER_ID" >/dev/null
  query_state "$CORE_KEYBOARD_ID" >/dev/null
  read -r ROOT_W ROOT_H < <(xwininfo -root | awk '/Width:/ { w = $2 } /Height:/ { h = $2 } END { print w, h }')
  echo "core pointer=$CORE_POINTER_ID keyboard=$CORE_KEYBOARD_ID root=${ROOT_W}x${ROOT_H}"
  (( ROOT_W >= 8000 && ROOT_H >= 800 )) || die "root ${ROOT_W}x${ROOT_H} is too small for a cancellable drag"
}

setup_x11() {
  unset WAYLAND_DISPLAY || true
  export XDG_SESSION_TYPE=x11
  mkdir -p "$CONFIG"
  rm -f "$CONFIG/companion.toml"
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
    DISPLAY=:99 xinput list >&2 || true
    write_evdev_conf
    if ! start_xorg "$DISPLAY_LOG_DIR/xorg-evdev.conf" "$DISPLAY_LOG_DIR/xorg-evdev.log"; then
      echo "Xorg did not start" >&2
      cat "$DISPLAY_LOG_DIR/xorg-evdev.log" >&2 || true
      return 1
    fi
    if ! have_physical; then
      echo "uinput nodes are not slave devices. Physical input was not faked with XTEST." >&2
      DISPLAY=:99 xinput list >&2 || true
      cat "$DISPLAY_LOG_DIR/xorg-evdev.log" >&2 || true
      return 1
    fi
    PHYSICAL_DRIVER=evdev
  fi
  export DISPLAY=:99
  probe_physical_move
  xev -geometry 500x400+40+40 >"$XEV_LOG" 2>&1 &
  XEV_PID=$!
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

scenario_tcp_start_without_poll() {
  stop_companion
  if [[ -n $COMPANION_WAS_UP ]]; then
    sleep 2.2
  fi
  http_code "$DISPLAY_LOG_DIR/tcp-start.code" POST "$BASE/companion/automation" '{"action":"start"}'
  local code
  code=$(cat "$DISPLAY_LOG_DIR/tcp-start.code")
  [[ $code == 503 ]] || die "tcp start returned $code ($(cat "$HTTP_BODY"))"
  grep -q 'no recent companion poll' "$HTTP_BODY" || die "body $(cat "$HTTP_BODY")"
  [[ $(phase) == off ]] || die "phase became $(phase)"
}

scenario_inject_lands() {
  ensure_companion
  ensure_running
  focus_xev
  xev_mark
  http_code "$DISPLAY_LOG_DIR/last.code" POST "$BASE/session/$SID/mouse" '{"action":"move","x":80,"y":90}'
  [[ $(cat "$DISPLAY_LOG_DIR/last.code") == 204 ]] || die "move $(cat "$DISPLAY_LOG_DIR/last.code") $(cat "$HTTP_BODY")"
  http_code "$DISPLAY_LOG_DIR/last.code" POST "$BASE/session/$SID/mouse" '{"action":"click","x":100,"y":120,"button":"left"}'
  [[ $(cat "$DISPLAY_LOG_DIR/last.code") == 204 ]] || die "click $(cat "$DISPLAY_LOG_DIR/last.code")"
  http_code "$DISPLAY_LOG_DIR/last.code" POST "$BASE/session/$SID/mouse" '{"action":"drag","x":60,"y":70,"to_x":180,"to_y":200,"button":"left"}'
  [[ $(cat "$DISPLAY_LOG_DIR/last.code") == 204 ]] || die "drag $(cat "$DISPLAY_LOG_DIR/last.code")"
  http_code "$DISPLAY_LOG_DIR/last.code" POST "$BASE/session/$SID/type" '{"text":"Hi","keys":[{"key":"return","action":"tap"}]}'
  [[ $(cat "$DISPLAY_LOG_DIR/last.code") == 204 ]] || die "type $(cat "$DISPLAY_LOG_DIR/last.code") $(cat "$HTTP_BODY")"
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

scenario_uinput_key_pauses_type() {
  ensure_companion
  focus_xev
  python3 - <<'PY' >"$DISPLAY_LOG_DIR/long-type.json"
import json
print(json.dumps({"text": "q" * 2000}))
PY
  local attempt code n
  for attempt in 1 2 3; do
    ensure_running
    xev_mark
    http_code "$DISPLAY_LOG_DIR/type.code" POST "$BASE/session/$SID/type" @"$DISPLAY_LOG_DIR/long-type.json" &
    local curl_pid=$!
    sleep 0.05
    local spins=0
    while kill -0 "$curl_pid" 2>/dev/null; do
      uinput_cmd "key F12" || true
      spins=$((spins + 1))
      if (( spins > 100 )); then
        break
      fi
      sleep 0.02
    done
    wait "$curl_pid" || true
    code=$(cat "$DISPLAY_LOG_DIR/type.code")
    n=$(sent_after keystrokes)
    echo "attempt $attempt status $code sent ${n:-none}"
    if [[ $code == 409 && -n $n && $n -gt 0 && $n -lt 4000 ]]; then
      local qcount
      qcount=$(xev_since | grep -c 'keysym 0x71, q' || true)
      echo "xev q keypresses $qcount"
      if (( qcount >= 2000 )); then
        die "xev received the whole string"
      fi
      return 0
    fi
  done
  die "physical key did not cancel the rest of the type"
}

scenario_uinput_move_pauses() {
  ensure_companion
  ensure_running
  uinput_cmd "rel 30 12"
  wait_phase paused 40
  http_code "$DISPLAY_LOG_DIR/mouse.code" POST "$BASE/session/$SID/mouse" '{"action":"move","x":12,"y":12}'
  [[ $(cat "$DISPLAY_LOG_DIR/mouse.code") == 409 ]] || die "mouse returned $(cat "$DISPLAY_LOG_DIR/mouse.code")"
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
  kill "$pid"
  rm -f "$COMPANION_PID_FILE"
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
  http_code "$DISPLAY_LOG_DIR/last.code" POST "$BASE/session/$SID/type" \
    '{"keys":[{"key":"control","action":"down"},{"key":"a","action":"down"}]}'
  [[ $(cat "$DISPLAY_LOG_DIR/last.code") == 204 ]] || die "held type $(cat "$DISPLAY_LOG_DIR/last.code") $(cat "$HTTP_BODY")"
  local control a
  control=$(keycode_for Control_L)
  a=$(keycode_for a)
  [[ -n $control && -n $a ]] || die "keymap has no Control_L or a"
  if key_is_down "$control" || key_is_down "$a"; then
    query_state "$CORE_KEYBOARD_ID" >&2 || true
    die "finished type left a key down"
  fi

  python3 - <<'PY' >"$DISPLAY_LOG_DIR/held-type.json"
import json
keys = [{"key": "control", "action": "down"}]
keys += [{"key": "b", "action": "tap"} for _ in range(1500)]
print(json.dumps({"keys": keys}))
PY
  local attempt code n
  for attempt in 1 2 3; do
    ensure_running
    http_code "$DISPLAY_LOG_DIR/type.code" POST "$BASE/session/$SID/type" @"$DISPLAY_LOG_DIR/held-type.json" &
    local curl_pid=$!
    local i
    sleep 0.02
    for i in $(seq 1 400); do
      if ! kill -0 "$curl_pid" 2>/dev/null; then
        break
      fi
      socket_action pause || true
    done
    wait "$curl_pid" || true
    code=$(cat "$DISPLAY_LOG_DIR/type.code")
    n=$(sent_after keystrokes)
    echo "key cancel attempt $attempt status $code sent ${n:-none}"
    if [[ $code == 409 && -n $n && $n -gt 0 ]]; then
      if key_is_down "$control"; then
        query_state "$CORE_KEYBOARD_ID" >&2 || true
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
    http_code "$DISPLAY_LOG_DIR/drag.code" POST "$BASE/session/$SID/mouse" \
      "{\"action\":\"drag\",\"x\":0,\"y\":0,\"to_x\":$to_x,\"to_y\":$to_y,\"button\":\"left\"}" &
    curl_pid=$!
    sleep "$delay"
    for i in $(seq 1 400); do
      if ! kill -0 "$curl_pid" 2>/dev/null; then
        break
      fi
      socket_action pause || true
    done
    wait "$curl_pid" || true
    code=$(cat "$DISPLAY_LOG_DIR/drag.code")
    n=$(sent_after "pointer steps")
    echo "drag cancel attempt $attempt status $code sent ${n:-none} total $total $(error_text "$HTTP_BODY")"
    if button_is_down; then
      query_state "$CORE_POINTER_ID" >&2 || true
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
  uinput_cmd "rel 28 6"
  wait_phase paused 40
  sleep 0.4
  [[ $(phase) == paused ]] || die "resumed before the idle timeout"
  wait_phase running 80
  echo "idle resume returned to running"
}

run_x11() {
  local only=${1:-}
  setup_x11
  local names=(
    tcp-start-without-poll
    inject-lands
    uinput-key-pauses-type
    uinput-move-pauses
    pausing-click-does-not-ask
    xtest-click-does-nothing
    watchdog-pauses
    held-release
    auto-resume
  )
  local name limit
  for name in "${names[@]}"; do
    if [[ -n $only && $only != "$name" ]]; then
      continue
    fi
    case $name in
      uinput-key-pauses-type | held-release) limit=60 ;;
      auto-resume) limit=30 ;;
      *) limit=45 ;;
    esac
    run_named "$name" "$limit" "scenario_${name//-/_}"
  done
  echo "PHYSICAL driver=$PHYSICAL_DRIVER"
}

x11_sockets() {
  ls /tmp/.X11-unix 2>/dev/null | sed 's/^X/:/' | sort || true
}

wait_for_new_display() {
  local before=$1 label=$2 i now extra
  for i in $(seq 1 40); do
    if [[ -S ${XDG_RUNTIME_DIR:-}/hm-display ]]; then
      now=$(x11_sockets)
      extra=$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$now") | head -n 1 || true)
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
  local before=$1
  shift
  "$@" >"$DISPLAY_LOG_DIR/compositor.log" 2>&1 &
  COMPOSITOR_PID=$!
  wait_for_new_display "$before" "$1"
}

start_wayland() {
  export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/tmp/hm-xdg}
  mkdir -p "$XDG_RUNTIME_DIR"
  chmod 0700 "$XDG_RUNTIME_DIR"
  export XDG_SESSION_TYPE=wayland
  export WAYLAND_DISPLAY=hm-display
  export WLR_BACKENDS=headless
  export WLR_RENDERER=pixman
  export WLR_LIBINPUT_NO_DEVICES=1
  local before
  before=$(x11_sockets)
  cat >"$DISPLAY_LOG_DIR/sway.conf" <<'EOF'
xwayland enable
output * resolution 1280x720
EOF
  if start_compositor "$before" sway -c "$DISPLAY_LOG_DIR/sway.conf"; then
    return 0
  fi
  echo "sway did not provide Xwayland; trying weston headless" >&2
  cat "$DISPLAY_LOG_DIR/compositor.log" >&2 || true
  if [[ -n ${COMPOSITOR_PID:-} ]]; then
    kill "$COMPOSITOR_PID" 2>/dev/null || true
    COMPOSITOR_PID=
  fi
  before=$(x11_sockets)
  cat >"$DISPLAY_LOG_DIR/weston.ini" <<'EOF'
[core]
backend=headless-backend.so
xwayland=true

[output]
name=headless
mode=1280x720
EOF
  if start_compositor "$before" weston --config "$DISPLAY_LOG_DIR/weston.ini" --socket=hm-display; then
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
    http_code "$DISPLAY_LOG_DIR/wl.code" POST "$BASE/companion/automation" "{\"action\":\"$action\"}"
    code=$(cat "$DISPLAY_LOG_DIR/wl.code")
    [[ $code == 501 ]] || die "$action returned $code ($(cat "$HTTP_BODY"))"
    grep -q '#17' "$HTTP_BODY" || die "$action body $(cat "$HTTP_BODY")"
  done
  http_code "$DISPLAY_LOG_DIR/wl.code" POST "$BASE/session/$SID/mouse" '{"action":"move","x":1,"y":1}'
  [[ $(cat "$DISPLAY_LOG_DIR/wl.code") == 501 ]] || die "mouse returned $(cat "$DISPLAY_LOG_DIR/wl.code")"
  grep -q '#17' "$HTTP_BODY" || die "mouse body $(cat "$HTTP_BODY")"
  http_code "$DISPLAY_LOG_DIR/wl.code" POST "$BASE/session/$SID/type" '{"text":"no"}'
  [[ $(cat "$DISPLAY_LOG_DIR/wl.code") == 501 ]] || die "type returned $(cat "$DISPLAY_LOG_DIR/wl.code")"
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
  build_bins
  start_wayland || die "no headless compositor"
  if [[ -z $only || $only == wayland-refuses-automation ]]; then
    run_named wayland-refuses-automation 45 scenario_wayland_refuses_automation
  fi
  if [[ -z $only || $only == portal-libei ]]; then
    run_named portal-libei 15 scenario_portal_libei_placeholder
  fi
}

usage() {
  echo "usage: bash ci/display/run.sh x11 [scenario] | wayland [scenario]" >&2
  exit 2
}

main() {
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

main "$@"
