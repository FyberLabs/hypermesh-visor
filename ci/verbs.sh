#!/usr/bin/env bash
# Drive the daemon over HTTP and check the five session verbs.
# View, watch, and listen must match the committed fixtures.
# Mouse and type must match the events the input path recorded.
# No model calls.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

if grep -R -n --exclude-dir=.git -e "hypermesh-judge-hook" .github >/dev/null 2>&1; then
  echo "ci/hypermesh-judge-hook.sh must stay out of GitHub Actions" >&2
  exit 1
fi

bin="${BIN:-$root/target/debug/hypermesh-visor}"
if [[ ! -x "$bin" ]]; then
  cargo build --locked
fi

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

"$bin" --listen 127.0.0.1:0 --fixture --fixture-log "$tmpdir/events.jsonl" \
  >"$tmpdir/stdout" 2>"$tmpdir/stderr" &
pid=$!

addr=""
for _ in $(seq 1 50); do
  if [[ -s "$tmpdir/stdout" ]]; then
    addr=$(awk '/^listening / { print $2; exit }' "$tmpdir/stdout")
    if [[ -n "$addr" ]]; then
      break
    fi
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "daemon exited before listen" >&2
    cat "$tmpdir/stderr" >&2 || true
    exit 1
  fi
  sleep 0.1
done
if [[ -z "$addr" ]]; then
  echo "daemon did not print a listen line" >&2
  cat "$tmpdir/stdout" "$tmpdir/stderr" >&2 || true
  exit 1
fi

base="http://${addr}"
session=$(curl -fsS -X POST "$base/session" \
  -H 'content-type: application/json' \
  -d '{"purpose":"check the five verbs","recipes":[],"agent":{"name":"fixture","instructions":""},"skills":[]}')
id=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$session")

base64 -d tests/fixtures/view.png.b64 > "$tmpdir/expected.png"
base64 -d tests/fixtures/listen.pcm.b64 > "$tmpdir/expected.pcm"

view_code=$(curl -sS -o "$tmpdir/view.png" -w '%{http_code}' -X POST "$base/session/$id/view")
if [[ "$view_code" != "200" ]]; then
  echo "view status $view_code" >&2
  exit 1
fi
cmp -s "$tmpdir/view.png" "$tmpdir/expected.png"

set +e
curl -sS --max-time 2 -D "$tmpdir/watch.headers" -o "$tmpdir/watch.body" \
  "$base/session/$id/watch?mode=frames" 2>"$tmpdir/watch.err"
watch_curl=$?
set -e
if [[ "$watch_curl" -ne 0 && "$watch_curl" -ne 28 ]]; then
  cat "$tmpdir/watch.err" >&2
  echo "watch request failed" >&2
  exit 1
fi

listen_code=$(curl -sS -D "$tmpdir/listen.headers" -o "$tmpdir/listen.pcm" -w '%{http_code}' \
  "$base/session/$id/listen")
if [[ "$listen_code" != "200" ]]; then
  echo "listen status $listen_code" >&2
  exit 1
fi
cmp -s "$tmpdir/listen.pcm" "$tmpdir/expected.pcm"

post_mouse() {
  local body=$1
  local code
  code=$(curl -sS -o "$tmpdir/mouse.body" -w '%{http_code}' -X POST "$base/session/$id/mouse" \
    -H 'content-type: application/json' \
    -d "$body")
  if [[ "$code" != "204" ]]; then
    echo "mouse status $code for $body" >&2
    cat "$tmpdir/mouse.body" >&2
    exit 1
  fi
}

post_mouse '{"action":"move","x":120,"y":40}'
post_mouse '{"action":"click","x":15,"y":80,"button":"left"}'
post_mouse '{"action":"click","x":16,"y":81,"button":"right"}'
post_mouse '{"action":"click","x":17,"y":82,"button":"middle"}'
post_mouse '{"action":"drag","x":3,"y":4,"to_x":30,"to_y":40,"button":"left"}'

type_code=$(curl -sS -o "$tmpdir/type.body" -w '%{http_code}' -X POST "$base/session/$id/type" \
  -H 'content-type: application/json' \
  -d '{"text":"Hi","keys":[{"key":"return","action":"tap"}]}')
if [[ "$type_code" != "204" ]]; then
  echo "type status $type_code" >&2
  cat "$tmpdir/type.body" >&2
  exit 1
fi

python3 - "$tmpdir" <<'PY'
import base64, json, pathlib, sys

tmpdir = pathlib.Path(sys.argv[1])
watch_headers = (tmpdir / "watch.headers").read_text()
if "200" not in watch_headers.splitlines()[0]:
    raise SystemExit(f"watch status line missing 200: {watch_headers.splitlines()[0]}")
if "text/event-stream" not in watch_headers.lower():
    raise SystemExit("watch is not an event stream")
frame = None
for line in (tmpdir / "watch.body").read_text().splitlines():
    if line.startswith("data: "):
        frame = json.loads(line[len("data: "):])
        break
if frame is None:
    raise SystemExit("watch produced no event")
if frame.get("kind") != "frame" or frame.get("mime") != "image/png":
    raise SystemExit(f"watch event was {frame}")
png = base64.b64decode(frame["png_base64"])
expected_png = (tmpdir / "expected.png").read_bytes()
if png != expected_png:
    raise SystemExit("watch frame does not match tests/fixtures/view.png.b64")

listen_headers = (tmpdir / "listen.headers").read_text().lower()
if "x-audio-format: s16le;rate=48000;channels=1" not in listen_headers:
    raise SystemExit("listen audio format header mismatch")

events = [json.loads(line) for line in (tmpdir / "events.jsonl").read_text().splitlines() if line]
expected_mouse = [
    {"kind": "mouse", "action": "move", "x": 120, "y": 40},
    {"kind": "mouse", "action": "click", "x": 15, "y": 80, "button": "left"},
    {"kind": "mouse", "action": "click", "x": 16, "y": 81, "button": "right"},
    {"kind": "mouse", "action": "click", "x": 17, "y": 82, "button": "middle"},
    {"kind": "mouse", "action": "drag", "x": 3, "y": 4, "to_x": 30, "to_y": 40, "button": "left"},
]
mouse = [event for event in events if event.get("kind") == "mouse"]
if mouse != expected_mouse:
    raise SystemExit(f"mouse events were {mouse}")

typed = [event for event in events if event.get("kind") == "type"]
if len(typed) != 1:
    raise SystemExit(f"type events were {typed}")
strokes = typed[0]["strokes"]
expected_strokes = []
for keysym in (ord("H"), ord("i"), 0xFF0D):
    expected_strokes.append({"keysym": keysym, "down": True})
    expected_strokes.append({"keysym": keysym, "down": False})
if strokes != expected_strokes:
    raise SystemExit(f"type strokes were {strokes}")

def characters(items):
    text = []
    for stroke in items:
        if not stroke["down"]:
            continue
        keysym = stroke["keysym"]
        if keysym == 0xFF0D:
            text.append("\n")
        elif keysym == 0xFF09:
            text.append("\t")
        elif keysym <= 0xFF:
            text.append(chr(keysym))
        else:
            raise SystemExit(f"unexpected keysym {keysym}")
    return "".join(text)

got = characters(strokes)
if got != "Hi\n":
    raise SystemExit(f"typed characters were {got!r}")
if [event.get("kind") for event in events] != ["mouse"] * 5 + ["type"]:
    raise SystemExit(f"event order was {events}")
PY

echo "verbs ok"
