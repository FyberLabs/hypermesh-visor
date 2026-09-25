#!/usr/bin/env python3
"""Draw the desktop companion beetle sprites.

hypermesh-companion embeds companion/assets/bug-idle.png and bug-active.png.
The previous tree stored one PNG as ~12KB base64 shards and never committed
most of them, so a clean checkout could not compile. This script is the source
for the two shipped sprites. Run it from the repo root or from this directory:

    python3 companion/assets/render_sprites.py
"""

import math
import os
import struct
import zlib

WIDTH = 400
HEIGHT = 520

SHELL = (34, 108, 54, 255)
SHELL_DARK = (16, 52, 28, 255)
SHELL_RIM = (22, 72, 36, 255)
HEAD = (40, 122, 64, 255)
HEAD_DARK = (18, 64, 34, 255)
PRONOTUM = (52, 138, 72, 255)
LEG = (24, 58, 32, 255)
LEG_DARK = (14, 36, 20, 255)
ANTENNA = (28, 64, 36, 255)
EYE = (252, 252, 250, 255)
PUPIL = (12, 14, 18, 255)
SPOT = (18, 62, 34, 255)
CHEEK = (86, 168, 96, 255)
# Green stays at or above 50 so the pupil flood-fill does not treat the mouth as ink.
MOUTH = (32, 72, 36, 255)
HIGHLIGHT = (132, 186, 120, 255)


def new_image():
    return bytearray(WIDTH * HEIGHT * 4)


def put(img, x, y, color):
    if x < 0 or y < 0 or x >= WIDTH or y >= HEIGHT:
        return
    i = (y * WIDTH + x) * 4
    src_a = color[3]
    if src_a == 255:
        img[i : i + 4] = bytes(color)
        return
    if src_a == 0:
        return
    dst_a = img[i + 3]
    out_a = src_a + dst_a * (255 - src_a) // 255
    if out_a == 0:
        return
    for channel in range(3):
        s = color[channel]
        d = img[i + channel]
        img[i + channel] = (s * src_a + d * dst_a * (255 - src_a) // 255) // out_a
    img[i + 3] = out_a


def disc(img, cx, cy, radius, color):
    r2 = radius * radius
    for y in range(int(cy - radius) - 1, int(cy + radius) + 2):
        for x in range(int(cx - radius) - 1, int(cx + radius) + 2):
            dx = x + 0.5 - cx
            dy = y + 0.5 - cy
            if dx * dx + dy * dy <= r2:
                put(img, x, y, color)


def ellipse(img, cx, cy, rx, ry, color):
    if rx <= 0 or ry <= 0:
        return
    for y in range(int(cy - ry) - 1, int(cy + ry) + 2):
        for x in range(int(cx - rx) - 1, int(cx + rx) + 2):
            dx = (x + 0.5 - cx) / rx
            dy = (y + 0.5 - cy) / ry
            if dx * dx + dy * dy <= 1.0:
                put(img, x, y, color)


def stroke(img, points, radius, color):
    for (x0, y0), (x1, y1) in zip(points, points[1:]):
        steps = max(int(math.hypot(x1 - x0, y1 - y0)), 1)
        for step in range(steps + 1):
            t = step / steps
            disc(img, x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, radius, color)


def leg(img, joints, radius):
    stroke(img, joints, radius + 1.5, LEG_DARK)
    stroke(img, joints, radius, LEG)
    disc(img, joints[-1][0], joints[-1][1], radius + 1.2, LEG)


def antenna(img, points):
    stroke(img, points, 3.2, ANTENNA)
    tip = points[-1]
    disc(img, tip[0], tip[1], 7.5, ANTENNA)
    disc(img, tip[0], tip[1], 4.2, PRONOTUM)


def eye(img, cx, cy, rx, ry):
    ellipse(img, cx, cy, rx + 4, ry + 4, HEAD_DARK)
    ellipse(img, cx, cy, rx, ry, EYE)
    disc(img, cx, cy, min(rx, ry) * 0.34, PUPIL)


def spots(img, pairs):
    for cx, cy, rx, ry in pairs:
        ellipse(img, cx, cy, rx, ry, SPOT)
        ellipse(img, WIDTH - cx, cy, rx, ry, SPOT)


def beetle(active):
    img = new_image()
    # Soft contact shadow. Alpha stays under the eye detector's threshold.
    ellipse(img, 200, 470, 120, 22, (0, 0, 0, 70))

    if active:
        legs = [
            [(148, 250), (78, 210), (42, 250), (58, 300)],
            [(132, 320), (48, 330), (28, 390)],
            [(150, 390), (62, 430), (88, 478)],
        ]
    else:
        legs = [
            [(156, 260), (96, 240), (72, 286), (90, 328)],
            [(146, 330), (70, 348), (64, 400)],
            [(160, 400), (96, 436), (118, 472)],
        ]
    for joints in legs:
        leg(img, joints, 7)
        mirrored = [(WIDTH - x, y) for x, y in joints]
        leg(img, mirrored, 7)

    ellipse(img, 200, 348, 132, 118, SHELL_RIM)
    ellipse(img, 200, 348, 122, 108, SHELL)
    # Elytra seam and a lighter patch along the crown of the shell.
    ellipse(img, 176, 300, 46, 28, HIGHLIGHT)
    ellipse(img, 224, 300, 46, 28, HIGHLIGHT)
    stroke(img, [(200, 268), (200, 448)], 3.0, SHELL_DARK)
    spots(
        img,
        [
            (150, 330, 16, 12),
            (138, 378, 18, 13),
            (158, 420, 14, 11),
        ],
    )

    ellipse(img, 200, 214, 96, 52, HEAD_DARK)
    ellipse(img, 200, 210, 88, 44, PRONOTUM)

    ellipse(img, 200, 168, 108, 86, HEAD_DARK)
    ellipse(img, 200, 166, 100, 78, HEAD)
    ellipse(img, 168, 132, 28, 16, HIGHLIGHT)
    ellipse(img, 232, 132, 28, 16, HIGHLIGHT)

    if active:
        # Small scarab horn, raised with the antennae while a session is open.
        ellipse(img, 200, 78, 14, 22, HEAD_DARK)
        ellipse(img, 200, 76, 10, 16, PRONOTUM)
        antenna(img, [(168, 104), (150, 52), (132, 22)])
        antenna(img, [(232, 104), (250, 52), (268, 22)])
        eye(img, 158, 162, 36, 40)
        eye(img, 242, 162, 36, 40)
        ellipse(img, 124, 196, 16, 10, CHEEK)
        ellipse(img, 276, 196, 16, 10, CHEEK)
        ellipse(img, 200, 228, 14, 10, MOUTH)
    else:
        antenna(img, [(164, 112), (118, 86), (92, 108)])
        antenna(img, [(236, 112), (282, 86), (308, 108)])
        eye(img, 158, 168, 34, 38)
        eye(img, 242, 168, 34, 38)
        ellipse(img, 126, 202, 14, 9, CHEEK)
        ellipse(img, 274, 202, 14, 9, CHEEK)
        stroke(img, [(176, 214), (200, 224), (224, 214)], 3.2, MOUTH)

    return img


def write_png(path, rgba):
    def chunk(tag, data):
        crc = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", crc)

    raw = bytearray()
    stride = WIDTH * 4
    for y in range(HEIGHT):
        raw.append(0)
        raw.extend(rgba[y * stride : (y + 1) * stride])
    ihdr = struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 6, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", ihdr)
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as handle:
        handle.write(png)


def eye_components(rgba):
    """Mirror companion/src/draw.rs find_eyes so the shipped art keeps two eyes."""

    def is_eye(x, y):
        i = (y * WIDTH + x) * 4
        r, g, b, a = rgba[i : i + 4]
        return a > 200 and r > 190 and g > 180 and b > 170

    seen = [False] * (WIDTH * HEIGHT)
    found = []
    for y in range(HEIGHT):
        for x in range(WIDTH):
            i = y * WIDTH + x
            if seen[i] or not is_eye(x, y):
                continue
            stack = [(x, y)]
            seen[i] = True
            min_x = max_x = x
            min_y = max_y = y
            area = 0
            while stack:
                cx, cy = stack.pop()
                area += 1
                min_x = min(min_x, cx)
                max_x = max(max_x, cx)
                min_y = min(min_y, cy)
                max_y = max(max_y, cy)
                for nx, ny in ((cx - 1, cy), (cx + 1, cy), (cx, cy - 1), (cx, cy + 1)):
                    if nx < 0 or ny < 0 or nx >= WIDTH or ny >= HEIGHT:
                        continue
                    ni = ny * WIDTH + nx
                    if seen[ni] or not is_eye(nx, ny):
                        continue
                    seen[ni] = True
                    stack.append((nx, ny))
            rect_w = max_x - min_x + 1
            rect_h = max_y - min_y + 1
            cy = min_y + rect_h // 2
            if area > 400 and cy < (HEIGHT * 70) // 100 and rect_w > 20 and rect_h > 20:
                found.append(area)
    return found


def main():
    out_dir = os.path.dirname(os.path.abspath(__file__))
    for name, active in (("bug-idle.png", False), ("bug-active.png", True)):
        rgba = beetle(active)
        eyes = eye_components(rgba)
        if len(eyes) != 2:
            raise SystemExit(f"{name} has {len(eyes)} eyes, expected 2 ({eyes})")
        # The idle render samples the scene center, which lands on the head.
        center = ((HEIGHT // 3) * WIDTH + WIDTH // 2) * 4
        if rgba[center + 3] < 200:
            raise SystemExit(f"{name} center is transparent")
        if rgba[3] != 0:
            raise SystemExit(f"{name} corner is opaque")
        path = os.path.join(out_dir, name)
        write_png(path, rgba)
        print(f"wrote {path} ({os.path.getsize(path)} bytes, eyes {eyes})")


if __name__ == "__main__":
    main()
