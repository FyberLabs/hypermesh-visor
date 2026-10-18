# Companion cursor sprites

Top-down art for the beetle pointer. The companion installs these frames as the X11 cursor while an automation runs, and outside an automation only when `bug_cursor` is on in `companion.toml`. The click point is `hotspot.json`.

During an automation this cursor is paired with the automation visor on the companion. That pairing is [Automation mode](../../../README.md#automation-mode). Away from an automation, this cursor stays optional.

The frames are top-down. The head points up-left, the same way a standard arrow cursor does. `preview.png` shows pointer, crawl, fly, click, type, and busy at a large size and at 32 px.

These sprites are part of this repository and use the [MIT License](../../../LICENSE).

`cursor.json` is the same map in a form a program can read. `hotspot.json` is the click point per size.

## States

Show one state at a time. Advance its frames in order and loop while that state lasts. A click can play once, then return to reach or pointer.

| State | When it shows | Frames, in order |
| --- | --- | --- |
| pointer | Default, while the pointer is still | `pointer` |
| crawl | The pointer is moving | `crawl` |
| fly | A fast move, or a long one | `fly` |
| reach | Hovering something that can be clicked | `reach` |
| click | A mouse button press | `click1`, then `click2` |
| type | Keyboard input | `type1`, then `type2` |
| busy | Working or waiting | `busy1`, `busy2`, `busy3`, `busy4` |

Each name is a file `{frame}_{size}.png`. Pointer, crawl, fly, and reach are a single pose. Click is two poses (the antenna winds up, then extends). Type is two beats of the beetle holding a small keyboard. Busy is four beats of motion ticks around the legs.

## Sizes

Every pose is a square PNG with a transparent background:

| Size | Use |
| --- | --- |
| 32 px | Small pointer, and the small row in `preview.png` |
| 64 px | Medium pointer |
| 128 px | Large pointer, and the size of the GIFs |

## Timing

Hold each PNG for about **100 ms**, then show the next frame.

The GIFs are 128 px loops of the same states. Reach is one pose and has no GIF. GIF delay is stored in centiseconds, so the files sit near a 100 ms step. Use the PNG sequence when the timing matters. The click GIF has more frames than `click1` and `click2`.

| GIF | Frames in the file | Delay stored in the GIF |
| --- | --- | --- |
| `pointer.gif` | 1 | 500 ms |
| `crawl.gif` | 2 | 160 ms |
| `fly.gif` | 2 | 80 ms |
| `click.gif` | 5 | 110 ms |
| `type.gif` | 2 | 150 ms |
| `busy.gif` | 4 | 100 ms |

## Hotspot

The click point is the antenna tip. It is the same pixel on every frame of a size, including poses where the body has shifted away from that pixel, so the click point stays put between states. Origin is the top-left pixel of the frame. `x` increases to the right and `y` increases downward.

| Size | Hotspot `[x, y]` |
| --- | --- |
| 32 | `[10, 5]` |
| 64 | `[20, 10]` |
| 128 | `[41, 20]` |

For another size, scale the 128 px hotspot by `size / 128` and round half to even:

- `x = round(41 * size / 128)`
- `y = round(20 * size / 128)`

That rule reproduces the 32 and 64 rows. `41 * 64 / 128` is 20.5, which rounds to 20.

## Files

| State | 32 px | 64 px | 128 px | GIF |
| --- | --- | --- | --- | --- |
| pointer | `pointer_32.png` | `pointer_64.png` | `pointer_128.png` | `pointer.gif` |
| crawl | `crawl_32.png` | `crawl_64.png` | `crawl_128.png` | `crawl.gif` |
| fly | `fly_32.png` | `fly_64.png` | `fly_128.png` | `fly.gif` |
| reach | `reach_32.png` | `reach_64.png` | `reach_128.png` | — |
| click | `click1_32.png`, `click2_32.png` | `click1_64.png`, `click2_64.png` | `click1_128.png`, `click2_128.png` | `click.gif` |
| type | `type1_32.png`, `type2_32.png` | `type1_64.png`, `type2_64.png` | `type1_128.png`, `type2_128.png` | `type.gif` |
| busy | `busy1_32.png` … `busy4_32.png` | `busy1_64.png` … `busy4_64.png` | `busy1_128.png` … `busy4_128.png` | `busy.gif` |
