#!/usr/bin/env python3
"""Pre-arm an observer and inject one physical F12 after the first typed q.

Uses the harness's inherited uinput pipes and command lock, never a socket.
The bounded log observer emits timing evidence even when the overlap is missed.
"""
import fcntl
import json
import os
from pathlib import Path
import select
import sys
import time


def interrupt(log, offset, command_fd, ack_fd, lock, ready, timeout=5):
    started = time.monotonic_ns()
    evidence = {"observed_q": False, "acknowledged": False}
    deadline = time.monotonic() + timeout
    try:
        with open(log, "rb") as events, open(lock, "a") as guard:
            events.seek(offset)
            Path(ready).touch()
            pending = b""
            while time.monotonic() < deadline:
                pending += events.read(65536)
                lines = pending.split(b"\n")
                pending = lines.pop()
                if any(b"keysym 0x71, q" in line for line in lines):
                    evidence["observed_q"] = True
                    evidence["first_q_ms"] = (time.monotonic_ns() - started) / 1e6
                    # Match uinput_cmd's serialization; there is no competing
                    # command in this scenario, but never interleave pipe replies.
                    while True:
                        try:
                            fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            break
                        except BlockingIOError:
                            if time.monotonic() >= deadline:
                                raise TimeoutError("uinput lock deadline")
                            time.sleep(0.001)
                    evidence["stage"] = "send_key"
                    os.write(command_fd, b"key F12\n")
                    evidence["key_sent_ms"] = (time.monotonic_ns() - started) / 1e6
                    evidence["stage"] = "read_ack"
                    ack = b""
                    while not ack.endswith(b"\n"):
                        remaining = deadline - time.monotonic()
                        if remaining <= 0 or not select.select([ack_fd], [], [], remaining)[0]:
                            raise TimeoutError("uinput acknowledgement deadline")
                        chunk = os.read(ack_fd, 1)
                        if not chunk:
                            raise RuntimeError("uinput channel closed")
                        ack += chunk
                        if len(ack) > 128:
                            raise RuntimeError("invalid uinput acknowledgement")
                    if ack.strip() != b"ok":
                        raise RuntimeError("uinput command rejected")
                    evidence["acknowledged"] = True
                    return evidence
                time.sleep(0.001)
            raise TimeoutError("no typed q observed")
    except (OSError, RuntimeError, TimeoutError) as error:
        evidence["error"] = str(error)
        return evidence
    finally:
        evidence["elapsed_ms"] = (time.monotonic_ns() - started) / 1e6


if __name__ == "__main__":
    result = interrupt(sys.argv[1], int(sys.argv[2]), int(sys.argv[3]),
                       int(sys.argv[4]), sys.argv[5], sys.argv[6])
    Path(sys.argv[7]).write_text(json.dumps(result) + "\n")
    sys.exit(0 if result["acknowledged"] else 1)
