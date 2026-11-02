#!/usr/bin/env python3
"""Exercise observer ordering and bounded failures without a display or uinput."""
import importlib.util
import os
import subprocess
from pathlib import Path
import tempfile
import threading
import time
import unittest

spec = importlib.util.spec_from_file_location("observer", Path(__file__).with_name("interrupt-type.py"))
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)


class InterruptTests(unittest.TestCase):
    def run_observer(self, emit, ack=b"ok\n", child=False):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            log, ready = base / "xev", base / "ready"
            log.write_bytes(b"old keysym 0x71, q\n")
            offset = log.stat().st_size
            cr, cw = os.pipe()
            ar, aw = os.pipe()
            result = []
            def run():
                if child:
                    # Exercise the explicit descriptor duplication at exec, not
                    # just Python threads sharing open descriptors.
                    completed = subprocess.run([
                        "bash", "-c",
                        'python3 "$1" "$2" "$3" 3 4 "$6" "$7" 3>&"$4" 4<&"$5"',
                        "observer", str(Path(__file__).with_name("interrupt-type.py")),
                        str(log), str(offset), str(cw), str(ar), str(base / "lock"), str(ready),
                    ], pass_fds=(cw, ar), capture_output=True, text=True, timeout=6)
                    import json
                    result.append(json.loads(completed.stdout))
                else:
                    result.append(observer.interrupt(log, offset, cw, ar, base / "lock", ready, timeout=0.5))
            worker = threading.Thread(target=run)
            try:
                worker.start()
                deadline = time.monotonic() + 1
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.001)
                self.assertTrue(ready.exists())
                os.set_blocking(cr, False)
                with self.assertRaises(BlockingIOError):
                    os.read(cr, 128)  # old events must not trigger an injection
                if emit:
                    with log.open("ab", buffering=0) as output:
                        output.write(b"new keysym 0x71,")
                        time.sleep(0.01)
                        with self.assertRaises(BlockingIOError):
                            os.read(cr, 128)
                        output.write(b" q\n")
                    command = b""
                    while not command and time.monotonic() < deadline:
                        try:
                            command = os.read(cr, 128)
                        except BlockingIOError:
                            time.sleep(0.001)
                    self.assertEqual(command, b"key F12\n")
                    os.write(aw, ack)
                worker.join(1)
                self.assertFalse(worker.is_alive())
                return result[0]
            finally:
                worker.join(1)
                for fd in [cr, cw, ar, aw]:
                    os.close(fd)

    def test_observed_input_precedes_injection(self):
        result = self.run_observer(True)
        self.assertTrue(result["acknowledged"])
        self.assertLessEqual(result["first_q_ms"], result["key_sent_ms"])
        self.assertLessEqual(result["key_sent_ms"], result["elapsed_ms"])

    def test_subprocess_receives_explicit_pipe_descriptors(self):
        self.assertTrue(self.run_observer(True, child=True)["acknowledged"])

    def test_no_new_input_times_out_without_injection(self):
        result = self.run_observer(False)
        self.assertFalse(result["observed_q"])
        self.assertFalse(result["acknowledged"])

    def test_rejected_physical_input_is_not_success(self):
        result = self.run_observer(True, b"err\n")
        self.assertTrue(result["observed_q"])
        self.assertFalse(result["acknowledged"])


if __name__ == "__main__":
    unittest.main()
