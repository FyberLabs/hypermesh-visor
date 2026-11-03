# Assigned Mac native helper qualification

On 2026-11-02 (America/New_York), tested the installed development helper from
commit `594c8db` on MACMINI-1, Apple Silicon, macOS 27.2. The user enabled Screen
Recording, Accessibility and Input Monitoring for the installed app. The helper
executable hash is recorded alongside the machine-readable report.

Passed against a disposable AppKit test window:

- All three permission preflight checks, from the installed app's launch context.
- Foreground PID matched the owned fixture before keyboard and mouse injection.
- Unicode keyboard input including an emoji, verified by the fixture's text value.
- Pointer movement and left-button click, verified by the fixture's submission.
- ScreenCaptureKit PNG with dimensions matching the 1920×1080 logical display.

Capture pixels are not committed: the screen can contain unrelated user data.
Only dimensions and byte count are retained. Fixture and driver sources are
included to describe exactly what ran. The driver uses `/tmp/visor-native-qualification`
on the Mac; compile/start `Fixture.swift` there before running `run.py`. Use a
fresh fixture directory for another run. The installed app must already exist
and have the permissions granted. The driver launches it in the background via
LaunchServices and waits for a complete JSON-line response.

A direct SSH invocation of the same executable reported permissions denied.
Launching through LaunchServices reported them granted and passed these tests.
Do not attribute the app's permission grant to an SSH process or another binary.

This is direct helper acceptance only. The daemon currently invokes its sibling
helper directly; packaging/launch-context integration remains unfinished. Native
companion UI and physical-input monitoring/pause are also unfinished. This record
does not close #18/#26/#29 or qualify unattended automation. No release signing
certificate or notarization was used; the app has a local ad-hoc signature.
