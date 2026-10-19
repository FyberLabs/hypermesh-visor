#!/usr/bin/env python3
"""Kernel uinput pointer and keyboard for the display harness.

These are ordinary evdev devices. Xorg (libinput or evdev) reads them as
slave devices. They are not the XTEST pointer or keyboard, so the companion
can treat their events as a person at the machine.

The process stays up so the devices stay alive. The harness is the only
writer on stdin. There is no socket. Commands are lines:

    rel DX DY
    click
    down
    up
    key F12|A|LEFTCTRL|LEFTSHIFT
    quit

A closed stdin is the same as quit: the devices are destroyed and the
process exits.
"""

import ctypes
import os
import struct
import sys
import time

EV_SYN = 0
EV_KEY = 1
EV_REL = 2
REL_X = 0
REL_Y = 1
REL_WHEEL = 8
SYN_REPORT = 0
BTN_LEFT = 0x110
BTN_RIGHT = 0x111
BTN_MIDDLE = 0x112
BUS_USB = 0x03
INPUT_PROP_POINTER = 0x00

KEYS = {
    "LEFTCTRL": 29,
    "A": 30,
    "LEFTSHIFT": 42,
    "F12": 88,
}

POINTER_NAME = "hypermesh-display-pointer"
KEYBOARD_NAME = "hypermesh-display-keyboard"


def _iow(nr, size):
    return (1 << 30) | (size << 16) | (ord("U") << 8) | nr


def _io(nr):
    return (ord("U") << 8) | nr


UI_DEV_CREATE = _io(1)
UI_DEV_DESTROY = _io(2)
UI_SET_EVBIT = _iow(100, 4)
UI_SET_KEYBIT = _iow(101, 4)
UI_SET_RELBIT = _iow(102, 4)
UI_SET_PROPBIT = _iow(110, 4)


class InputId(ctypes.Structure):
    _fields_ = [
        ("bustype", ctypes.c_uint16),
        ("vendor", ctypes.c_uint16),
        ("product", ctypes.c_uint16),
        ("version", ctypes.c_uint16),
    ]


class UinputSetup(ctypes.Structure):
    _fields_ = [
        ("id", InputId),
        ("name", ctypes.c_char * 80),
        ("ff_effects_max", ctypes.c_uint32),
    ]


UI_DEV_SETUP = _iow(3, ctypes.sizeof(UinputSetup))


def emit(fd, ev_type, code, value):
    os.write(fd, struct.pack("llHHi", 0, 0, ev_type, code, value))


def syn(fd):
    emit(fd, EV_SYN, SYN_REPORT, 0)


def event_node(name):
    root = "/sys/class/input"
    if not os.path.isdir(root):
        return None
    for entry in os.listdir(root):
        if not entry.startswith("event"):
            continue
        path = os.path.join(root, entry, "device", "name")
        try:
            with open(path, "r", encoding="utf-8") as handle:
                found = handle.read().strip()
        except OSError:
            continue
        if found == name:
            return "/dev/input/" + entry
    return None


def create_device(fd, name):
    libc = ctypes.CDLL(None, use_errno=True)
    setup = UinputSetup()
    setup.id.bustype = BUS_USB
    setup.id.vendor = 0x1234
    setup.id.product = 0x5678
    setup.name = name.encode()[:79]
    if libc.ioctl(fd, UI_DEV_SETUP, ctypes.byref(setup)) == 0:
        if libc.ioctl(fd, UI_DEV_CREATE, 0) == 0:
            return
        err = ctypes.get_errno()
        raise OSError(err, os.strerror(err), "UI_DEV_CREATE")
    # Kernels before UI_DEV_SETUP take a uinput_user_dev write, then create.
    name_bytes = name.encode()[:79].ljust(80, b"\0")
    ident = struct.pack("HHHH", BUS_USB, 0x1234, 0x5678, 0)
    axes = b"\0" * (64 * 4 * 4)
    os.write(fd, name_bytes + ident + struct.pack("I", 0) + axes)
    if libc.ioctl(fd, UI_DEV_CREATE, 0) < 0:
        err = ctypes.get_errno()
        raise OSError(err, os.strerror(err), "UI_DEV_CREATE")


def open_device(name, setup_bits):
    fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
    try:
        setup_bits(fd)
        create_device(fd, name)
    except Exception:
        os.close(fd)
        raise
    return fd


def set_bit(fd, request, value):
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.ioctl(fd, request, ctypes.c_int(value)) < 0:
        err = ctypes.get_errno()
        raise OSError(err, os.strerror(err), request)


def setup_pointer(fd):
    set_bit(fd, UI_SET_EVBIT, EV_KEY)
    set_bit(fd, UI_SET_EVBIT, EV_REL)
    set_bit(fd, UI_SET_EVBIT, EV_SYN)
    set_bit(fd, UI_SET_PROPBIT, INPUT_PROP_POINTER)
    for code in (REL_X, REL_Y, REL_WHEEL):
        set_bit(fd, UI_SET_RELBIT, code)
    for code in (BTN_LEFT, BTN_RIGHT, BTN_MIDDLE):
        set_bit(fd, UI_SET_KEYBIT, code)


def setup_keyboard(fd):
    set_bit(fd, UI_SET_EVBIT, EV_KEY)
    set_bit(fd, UI_SET_EVBIT, EV_SYN)
    for code in range(1, 120):
        set_bit(fd, UI_SET_KEYBIT, code)


def destroy(fd):
    libc = ctypes.CDLL(None, use_errno=True)
    libc.ioctl(fd, UI_DEV_DESTROY, 0)
    os.close(fd)


def handle(pointer, keyboard, line):
    parts = line.strip().split()
    if not parts:
        return "ok"
    command = parts[0]
    if command == "rel":
        if len(parts) != 3:
            return "err rel needs dx dy"
        dx, dy = int(parts[1]), int(parts[2])
        if dx:
            emit(pointer, EV_REL, REL_X, dx)
        if dy:
            emit(pointer, EV_REL, REL_Y, dy)
        syn(pointer)
        return "ok"
    if command == "click":
        emit(pointer, EV_KEY, BTN_LEFT, 1)
        syn(pointer)
        time.sleep(0.02)
        emit(pointer, EV_KEY, BTN_LEFT, 0)
        syn(pointer)
        return "ok"
    if command == "down":
        emit(pointer, EV_KEY, BTN_LEFT, 1)
        syn(pointer)
        return "ok"
    if command == "up":
        emit(pointer, EV_KEY, BTN_LEFT, 0)
        syn(pointer)
        return "ok"
    if command == "key":
        if len(parts) != 2 or parts[1] not in KEYS:
            return "err unknown key"
        code = KEYS[parts[1]]
        emit(keyboard, EV_KEY, code, 1)
        syn(keyboard)
        time.sleep(0.01)
        emit(keyboard, EV_KEY, code, 0)
        syn(keyboard)
        return "ok"
    if command == "quit":
        return "quit"
    return "err unknown command"


def main():
    if not os.path.exists("/dev/uinput"):
        print(
            "uinput: /dev/uinput is missing. sudo modprobe uinput on the runner. "
            "Physical input is not faked with XTEST.",
            file=sys.stderr,
        )
        return 1
    pointer = open_device(POINTER_NAME, setup_pointer)
    keyboard = open_device(KEYBOARD_NAME, setup_keyboard)
    pointer_node = None
    keyboard_node = None
    for _ in range(30):
        pointer_node = pointer_node or event_node(POINTER_NAME)
        keyboard_node = keyboard_node or event_node(KEYBOARD_NAME)
        if pointer_node and keyboard_node:
            break
        time.sleep(0.1)
    if not pointer_node or not keyboard_node:
        print(
            f"uinput: device nodes did not appear (pointer={pointer_node}, keyboard={keyboard_node})",
            file=sys.stderr,
        )
        destroy(pointer)
        destroy(keyboard)
        return 1
    print(
        f"ready pointer={pointer_node} keyboard={keyboard_node}",
        flush=True,
    )
    try:
        while True:
            raw = sys.stdin.readline()
            if raw == "":
                return 0
            try:
                result = handle(pointer, keyboard, raw)
            except Exception as err:
                result = f"err {err}"
            if result == "quit":
                sys.stdout.write("ok\n")
                sys.stdout.flush()
                return 0
            sys.stdout.write(result + "\n")
            sys.stdout.flush()
    finally:
        destroy(pointer)
        destroy(keyboard)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except OSError as err:
        print(f"uinput: {err}", file=sys.stderr)
        sys.exit(1)
