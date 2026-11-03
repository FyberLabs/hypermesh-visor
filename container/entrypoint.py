#!/usr/bin/env python3
"""Unprivileged private desktop supervisor. No host sockets or devices required."""
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import threading
import time
import uuid

os.umask(0o077)
stop = threading.Event()
for signum in (signal.SIGTERM, signal.SIGINT):
    signal.signal(signum, lambda *_: stop.set())
children = []
root = Path('/run/visor')
ready = root / 'ready'

def start(name, argv):
    process = subprocess.Popen(argv, start_new_session=True)
    children.append((name, process))
    return process

def wait_until(predicate, message):
    deadline = time.monotonic() + 20
    while not stop.is_set():
        if any(process.poll() is not None for _, process in children):
            raise RuntimeError('essential desktop process exited')
        if predicate():
            return
        if time.monotonic() >= deadline:
            raise RuntimeError(message)
        stop.wait(0.1)
    raise RuntimeError('startup interrupted')

try:
    if os.getuid() == 0:
        raise RuntimeError('container must run unprivileged')
    if Path('/var/run/docker.sock').exists() or Path('/dev/uinput').exists():
        raise RuntimeError('host control devices must not be mounted')
    ready.unlink(missing_ok=True)
    for path in ('/run/visor/hypermesh', '/run/visor/pulse', '/home/bot/.config/hypermesh', '/home/bot/.local/state/hypermesh', '/home/bot/workspace'):
        target = Path(path)
        target.mkdir(parents=True, exist_ok=True, mode=0o700)
        if target.is_symlink() or target.stat().st_uid != os.getuid():
            raise RuntimeError('private directory ownership invalid')
        target.chmod(0o700)
    identity = Path('/home/bot/.local/state/hypermesh/bot-id')
    try:
        with identity.open('x') as stream:
            stream.write(str(uuid.uuid4()) + '\n')
    except FileExistsError:
        uuid.UUID(identity.read_text().strip())
    subprocess.run(['xauth', '-f', os.environ['XAUTHORITY'], 'add', ':99', '.', secrets.token_hex(16)], check=True)
    start('display', ['Xvfb', ':99', '-screen', '0', '1280x800x24', '-nolisten', 'tcp', '-auth', os.environ['XAUTHORITY']])
    wait_until(lambda: subprocess.run(['xdpyinfo'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0, 'display unavailable')
    start('window-manager', ['openbox', '--sm-disable'])
    start('audio', ['pulseaudio', '--daemonize=no', '--exit-idle-time=-1', '--disallow-exit', '-n',
                    '--load=module-native-protocol-unix socket=/run/visor/pulse/native', '--load=module-null-sink sink_name=visor'])
    start('visor', ['/usr/local/bin/hypermesh-visor'])
    wait_until(lambda: Path('/run/visor/hypermesh/companion.sock').is_socket(), 'visor unavailable')
    start('companion', ['/usr/local/bin/hypermesh-companion'])
    ready.write_text('ready\n')
    print('private desktop ready; automation remains off until explicitly started', flush=True)
    while not stop.wait(0.2):
        if any(process.poll() is not None for _, process in children):
            raise RuntimeError('essential desktop process exited')
except Exception as error:
    print(f'container stopped: {error}', flush=True)
    raise SystemExit(1)
finally:
    ready.unlink(missing_ok=True)
    # Daemon first: stop admitting work and close sessions while display is alive.
    order = sorted(children, key=lambda item: item[0] != 'visor')
    for _, process in order:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
    deadline = time.monotonic() + 8
    for _, process in order:
        try:
            process.wait(timeout=max(0.1, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
