#!/usr/bin/env bash
# A second Linux user must not drive the desktop.
#
# The control and companion sockets live in a 0700 directory and are mode 0600,
# so another uid fails at connect, before SO_PEERCRED. The peercred check itself
# is covered by the admit unit test. TCP stays closed unless --tcp is set. With
# --tcp, every route is 401 without the bearer, and the bearer cannot resume a
# user pause. The other user cannot read api.token.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

if [[ "$(id -u)" -eq 0 ]]; then
  echo "cross-user check must run as the visor user, not root" >&2
  exit 1
fi
if ! sudo -n true; then
  echo "cross-user check needs passwordless sudo" >&2
  exit 1
fi

bin="${BIN:-$root/target/debug/hypermesh-visor}"
if [[ ! -x "$bin" ]]; then
  cargo build --locked
fi

guest=hm-visor-guest
if ! id -u "$guest" >/dev/null 2>&1; then
  sudo useradd --system --no-create-home --shell /usr/sbin/nologin "$guest"
fi

# --chdir is not allowed here. /tmp is traversable by the guest account.
as_guest() {
  ( cd /tmp && sudo -n -u "$guest" "$@" )
}

tmpdir=$(mktemp -d)
pid=""
cleanup() {
  if [[ -n "$pid" ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$tmpdir"
}
trap cleanup EXIT

export HYPERMESH_RUNTIME_DIR="$tmpdir/run"
mkdir -p "$HYPERMESH_RUNTIME_DIR"
chmod 700 "$HYPERMESH_RUNTIME_DIR"
unset HYPERMESH_TCP || true

start_visor() {
  "$bin" "$@" >"$tmpdir/stdout" 2>"$tmpdir/stderr" &
  pid=$!
}

stop_visor() {
  if [[ -n "$pid" ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    pid=""
  fi
}

wait_line() {
  local prefix=$1
  local found=""
  local _
  for _ in $(seq 1 50); do
    if [[ -s "$tmpdir/stdout" ]]; then
      found=$(awk -v p="$prefix" 'index($0, p) == 1 { print; exit }' "$tmpdir/stdout")
      if [[ -n "$found" ]]; then
        printf '%s\n' "$found"
        return 0
      fi
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "daemon exited before $prefix" >&2
      cat "$tmpdir/stderr" >&2 || true
      exit 1
    fi
    sleep 0.1
  done
  echo "daemon did not print $prefix" >&2
  cat "$tmpdir/stdout" "$tmpdir/stderr" >&2 || true
  exit 1
}

assert_no_tcp_listen() {
  python3 - "$1" <<'PY'
import os, pathlib, sys
pid = int(sys.argv[1])
inodes = set()
fd_dir = pathlib.Path(f"/proc/{pid}/fd")
for entry in fd_dir.iterdir():
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

guest_connect_fails() {
  local sock=$1
  local err="$tmpdir/guest-connect.err"
  local rc=0
  as_guest /usr/bin/python3 - "$sock" >"$err" 2>&1 <<'PY' || rc=$?
import socket, sys
path = sys.argv[1]
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    client.connect(path)
except OSError as exc:
    sys.stderr.write(f"{type(exc).__name__}: {exc}\n")
    raise SystemExit(0)
sys.stderr.write("connected\n")
raise SystemExit(1)
PY
  if [[ "$rc" -ne 0 ]]; then
    echo "guest reached $sock" >&2
    cat "$err" >&2 || true
    exit 1
  fi
  if ! grep -q 'PermissionError' "$err"; then
    echo "guest connect to $sock was not EACCES" >&2
    cat "$err" >&2 || true
    exit 1
  fi
}

http_code() {
  command curl -sS --max-time 5 -o "$tmpdir/body" -w '%{http_code}' "$@"
}

expect_code() {
  local want=$1
  shift
  local code
  code=$(http_code "$@")
  if [[ "$code" != "$want" ]]; then
    echo "expected $want got $code for $*" >&2
    cat "$tmpdir/body" >&2 || true
    exit 1
  fi
}

session_body='{"purpose":"cross-user","recipes":[],"agent":{"name":"fixture","instructions":""},"skills":[]}'
dummy=/session/00000000-0000-0000-0000-000000000001
routes=(
  "GET /health"
  "GET /no-such-route"
  "GET /companion"
  "POST /companion/automation"
  "POST /companion/interrupt"
  "POST /companion/notice"
  "POST /session"
  "DELETE ${dummy}"
  "POST ${dummy}/view"
  "GET ${dummy}/watch"
  "GET ${dummy}/feed/snapshot"
  "GET ${dummy}/feed"
  "GET ${dummy}/listen"
  "POST ${dummy}/mouse"
  "POST ${dummy}/type"
  "POST /prompt"
  "GET /audit"
  "GET /mcp-audit"
  "POST ${dummy}/stream"
  "GET ${dummy}/inbox"
  "GET ${dummy}/mcp"
  "POST ${dummy}/mcp/call"
)

start_visor --fixture --fixture-log "$tmpdir/events.jsonl"
control_line=$(wait_line "control socket ")
control_sock=${control_line#"control socket "}
companion_line=$(wait_line "companion socket ")
companion_sock=${companion_line#"companion socket "}
if [[ ! -S "$control_sock" || ! -S "$companion_sock" ]]; then
  echo "sockets are not listening" >&2
  exit 1
fi
if grep -q '^listening ' "$tmpdir/stdout"; then
  echo "TCP opened without --tcp" >&2
  exit 1
fi
if [[ -e "$HYPERMESH_RUNTIME_DIR/api.token" ]]; then
  echo "api.token exists while TCP is off" >&2
  exit 1
fi
dir_mode=$(stat -c '%a' "$HYPERMESH_RUNTIME_DIR")
sock_mode=$(stat -c '%a' "$control_sock")
companion_mode=$(stat -c '%a' "$companion_sock")
if [[ "$dir_mode" != "700" || "$sock_mode" != "600" || "$companion_mode" != "600" ]]; then
  echo "runtime mode dir=$dir_mode control=$sock_mode companion=$companion_mode" >&2
  exit 1
fi
assert_no_tcp_listen "$pid"

owner_code=$(command curl -sS --unix-socket "$control_sock" --max-time 5 \
  -o "$tmpdir/session.json" -w '%{http_code}' \
  -X POST "http://localhost/session" \
  -H 'content-type: application/json' \
  -d "$session_body")
if [[ "$owner_code" != "201" ]]; then
  echo "owner session status $owner_code" >&2
  cat "$tmpdir/session.json" >&2 || true
  exit 1
fi

guest_connect_fails "$control_sock"
guest_connect_fails "$companion_sock"
dir_rc=0
as_guest /usr/bin/python3 - "$HYPERMESH_RUNTIME_DIR" >"$tmpdir/guest-dir.err" 2>&1 <<'PY' || dir_rc=$?
import os, sys
try:
    os.listdir(sys.argv[1])
except PermissionError as exc:
    sys.stderr.write(f"{type(exc).__name__}: {exc}\n")
    raise SystemExit(0)
raise SystemExit(1)
PY
if [[ "$dir_rc" -ne 0 ]] || ! grep -q 'PermissionError' "$tmpdir/guest-dir.err"; then
  echo "guest could list the runtime directory" >&2
  cat "$tmpdir/guest-dir.err" >&2 || true
  exit 1
fi

stop_visor

: >"$tmpdir/stdout"
: >"$tmpdir/stderr"
start_visor --fixture --fixture-log "$tmpdir/events.jsonl" --tcp --listen 127.0.0.1:0
listen_line=$(wait_line "listening ")
addr=${listen_line#"listening "}
token_file="$HYPERMESH_RUNTIME_DIR/api.token"
token=""
for _ in $(seq 1 50); do
  if [[ -f "$token_file" ]]; then
    token=$(tr -d '[:space:]' <"$token_file")
    break
  fi
  sleep 0.1
done
if [[ -z "$token" ]]; then
  echo "api.token was not written" >&2
  exit 1
fi
token_mode=$(stat -c '%a' "$token_file")
if [[ "$token_mode" != "600" || ${#token} -ne 64 ]]; then
  echo "api.token mode=$token_mode len=${#token}" >&2
  exit 1
fi

base="http://${addr}"
for route in "${routes[@]}"; do
  method=${route%% *}
  path=${route#* }
  expect_code 401 -X "$method" "$base$path" -H 'content-type: application/json' -d '{}'
  expect_code 401 -X "$method" "$base$path" \
    -H 'content-type: application/json' \
    -H 'authorization: Bearer not-the-token' \
    -d '{}'
done

expect_code 201 -X POST "$base/session" \
  -H 'content-type: application/json' \
  -H "authorization: Bearer $token" \
  -d "$session_body"
expect_code 204 -X POST "$base/companion/automation" \
  -H 'content-type: application/json' \
  -H "authorization: Bearer $token" \
  -d '{"action":"pause"}'
expect_code 403 -X POST "$base/companion/automation" \
  -H 'content-type: application/json' \
  -H "authorization: Bearer $token" \
  -d '{"action":"resume"}'
expect_code 404 -X GET "$base/health" -H "authorization: Bearer $token"

if as_guest /bin/cat "$token_file" >/dev/null 2>"$tmpdir/guest-token.err"; then
  echo "guest read api.token" >&2
  exit 1
fi
if ! grep -qi 'permission denied' "$tmpdir/guest-token.err"; then
  echo "guest token read was not permission denied" >&2
  cat "$tmpdir/guest-token.err" >&2 || true
  exit 1
fi

guest_http() {
  local method=$1
  local path=$2
  as_guest /usr/bin/curl -sS --max-time 5 \
    -o /dev/null -w '%{http_code}' \
    -X "$method" "$base$path" \
    -H 'content-type: application/json' \
    -d '{}'
}

for path in /session "${dummy}/view" "${dummy}/mouse" "${dummy}/type" /companion/automation; do
  method=POST
  code=$(guest_http "$method" "$path")
  if [[ "$code" != "401" ]]; then
    echo "guest $method $path status $code" >&2
    exit 1
  fi
  code=$(as_guest /usr/bin/curl -sS --max-time 5 \
    -o /dev/null -w '%{http_code}' \
    -X POST "$base$path" \
    -H 'content-type: application/json' \
    -H 'authorization: Bearer not-the-token' \
    -d '{}')
  if [[ "$code" != "401" ]]; then
    echo "guest $path with a wrong token status $code" >&2
    exit 1
  fi
done

stop_visor
echo "cross-user check passed"
