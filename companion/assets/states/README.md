# Companion mascot states

Animated frames for the desktop beetle. The companion plays the `400x520` row. `states.json` chooses the frame list, the hold time, and whether the state loops.

The automation state is paired with the bug cursor. That pairing is [Automation mode](../../../README.md#automation-mode).

`states.json` is the source of truth for the frame list, the frame count, the milliseconds per frame, and whether a state loops. Disconnected has 6 frames. The other states have 8. Every state loops.

These sprites are part of this repository and use the [MIT License](../../../LICENSE).

`preview.png` is a contact sheet with one row per state. Each `<state>.gif` is a 256 px loop of that row, using the same hold time as `states.json`.

## When to show a state

Show one state at a time. Play the frames in the order `states.json` lists, hold each one for that state's `ms_per_frame`, and return to the first frame after the last.

| State | When it shows | What the frames do | Frames | ms per frame |
| --- | --- | --- | --- | --- |
| idle | The app is running and was used recently, and while an automation is paused | Waving, bobbing, and blinking | 8 | 150 |
| automation | An automation is running | A light sweeps across the visor | 8 | 90 |
| upgrading | The app is updating | A cocoon rocks, with twinkling sparkles | 8 | 130 |
| disconnected | The connection is lost, or the app has crashed | On its back, legs kicking, cable unplugged | 6 | 110 |
| sleeping | Still running, unused for a long time | Nightcap and floating Zs | 8 | 260 |

## Sizes

Each frame is a transparent PNG named `<state>_<n>_<size>.png`. `n` starts at 0.

| Size key | Pixels | Role |
| --- | --- | --- |
| `128` | 128×128 | Small |
| `256` | 256×256 | Medium, and the size of the GIFs |
| `400x520` | 400×520 | Same canvas as `../bug-idle.png` and `../bug-active.png` |

## Files

`states.json` names every PNG. The preview loops are `idle.gif`, `automation.gif`, `upgrading.gif`, `disconnected.gif`, and `sleeping.gif`.
