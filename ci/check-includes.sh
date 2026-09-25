#!/usr/bin/env bash
# Every include_str! and include_bytes! path has to exist in a clean checkout.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
exec python3 "$root/ci/check-includes.py"
