# thinkpad-kbd-light

Keyboard backlight control for ThinkPads on Windows.

ThinkPad firmware forgets the keyboard backlight level across sleep and reboot, and always
comes back dark. This fixes that, and adds idle dimming, idle timeout, and separate
profiles for AC and battery.

**Status: feature complete.** `tkbl-tray` runs in the notification area and does the whole
job — light on input, dim and off on idle, AC/battery profiles, re-applying the level after
sleep, and a settings window.

## How it works

`\\.\IBMPMDRV` — the Lenovo Power Management driver — is a *generic ACPI method
evaluator*: each IOCTL code names a 4-character ACPI method that the driver evaluates with
a single DWORD argument.

```
IOCTL 0x222680  ->  ACPI MLCG  ->  get backlight state
IOCTL 0x222684  ->  ACPI MLCS  ->  set backlight level
```

Both are `METHOD_BUFFERED` with a 4-byte input and 4-byte output. The device grants
`FILE_ANY_ACCESS`, so **no administrator rights are needed**.

This replaces the approach used by the older
[kingboyk/Thinkpad-Backlight](https://github.com/kingboyk/Thinkpad-Backlight), which
depended on `PWMTR64V.DLL` from the long-retired ThinkVantage Power Manager. That DLL is
gone from modern systems — but it was only a wrapper. The kernel driver underneath is
still shipped, still WHQL-signed, and still maintained, so we talk to it directly.

The interface was recovered by static analysis of `ibmpmdrv.sys`; the full derivation is in
`notes/` (untracked).

## Requirements

- A ThinkPad with the Lenovo PM driver (`ibmpmdrv.sys`) installed — it ships with the
  standard ThinkPad driver set.
- Windows 10/11, x64.
- Rust (MSVC toolchain) to build.

Developed and verified on a **ThinkPad X1 Carbon 6th Gen (20KG)**, BIOS 1.68, Windows 11
26100. Other ThinkPads that expose the `MLCG`/`MLCS` ACPI methods should work; some models
use `KLCG`/`KLCS` instead, which the two-tier HAL makes easy to add.

## Build

```sh
cargo build --release
```

Produces a self-contained `target/release/tkbl.exe`, about 135 KB, no runtime dependencies.

## Usage

```sh
tkbl get              # -> off | low | high
tkbl set low          # off|low|high, or 0|1|2
tkbl info             # driver status + decoded MLCG bitfield
tkbl test             # cycle off -> low -> high, then restore
```

### The application

```sh
tkbl-tray             # the real thing: runs in the notification area, no console
```

The icon shows the current level. Left-click toggles between off and the profile's active
level. Right-click gives:

```
  On (high) / Dim (low) / Off      pick a level by hand
  Profile: AC                      which profile is active
  Pause automatic control
  Settings…                        the settings window
  Start with Windows
  Exit                             restores the active level on the way out
```

Picking a level by hand, like pressing `Fn+Space`, pauses automatic control until the next
wake — the program will not fight you for the backlight.

### Settings

Both profiles are shown side by side, because the asymmetry between them is the point:

```
Profile           AC power        Battery
Enabled              [x]            [x]
Active level      [High  v]      [Low   v]
Dim after (s)     [x] 30         [ ] 30      <- greyed: nothing to dim to
Off after (s)     [x] 300        [x] 60

[x] Turn the backlight off while locked
[x] Restore the light when exiting
[x] Hide Lenovo's popup for automatic changes
```

Each timeout has its own tick box, so either can be switched off independently.

There is no "dim to" setting. With three levels, the only target that is not already
expressible through the two timers is the level below Active — anything else just turns
the dim timer into a second off timer — so dimming always drops one step.

Controls that cannot apply are greyed out rather than silently ignored: an active level of
`Low` leaves no room to dim first, so only the off timer applies; `Off` disables both.

Settings live in `%LOCALAPPDATA%\ThinkpadKbdLight\settings.json`. Hand edits are picked up
within a few seconds, no restart needed.

### The console daemon

```sh
tkbld                 # same behaviour, but logs what it is doing
```

Reads `%LOCALAPPDATA%\ThinkpadKbdLight\settings.json`, falling back to built-in defaults
when it does not exist. Logs each phase change so you can watch what it is doing:

```
tkbld running. settings: C:\Users\foo\AppData\Local\ThinkpadKbdLight\settings.json
power source: Ac
[phase] Active
[phase] Dim
[phase] Off
[phase] Active
```

```
$ tkbl info
device      : \\.\IBMPMDRV (opened, no elevation)
supported   : true
level       : high (2)
max level   : 2
raw MLCG    : 0x00050202
  bits 0-1  : 2 (current level)
  bit 9     : 1 (supported)
```

## Lenovo's on-screen popup

`tposd.exe` shows a popup every time the backlight level changes, which is useful when you
press `Fn+Space` and irritating when the program dims the keyboard on a timer.

There is no supported way to switch it off. The `MLCS` ACPI method raises a notification on
both of its branches — the `0x00010000` argument bit chooses between `MHKQ` and `MHKK`, not
whether to notify — and the values under `Software\Lenovo\OnScreenDisplay` are state, not
preferences.

So the popup is hidden instead, and only the one our own write caused: the visible windows
of `tposd` are noted immediately before writing, and anything that appears just after is
hidden. `Fn+Space` keeps its popup, because for that we never write. Switch it off with
"Hide Lenovo's popup for automatic changes" if you would rather see them.

## The hardware has three states

`off`, `low`, `high`. There is no percentage — "dim" is literally the `low` state, so the
active → dim → off behaviour maps onto `high → low → off`.

The firmware animates between levels over roughly a second, so this program never
implements a fade of its own: it writes the target level and lets the EC ramp. `MLCG`
reports the *target* level immediately rather than the animated intermediate, so
reading back straight after a write is reliable.

## A note on safety

Every backlight change traps into SMM via the firmware, so the library deliberately:

- **never writes when the value is already correct** (write-on-change),
- **serialises all calls** behind a lock, so two SMI traps are never in flight,
- **rate-limits writes** in the HAL itself rather than trusting callers.

The `tkbl raw` subcommand can evaluate arbitrary ACPI methods and is gated behind
`--force`. Guessing IOCTL codes against a power-management driver can wedge the embedded
controller; don't.

## Library

```rust
use tkbl::{Backlight, BacklightController, Level};

let backlight = Backlight::open()?;
if backlight.is_supported() {
    backlight.set_level(Level::Low)?;
}
```

`FakeBacklight` implements the same trait for testing without hardware.

## Roadmap

- [x] **1** — HAL + CLI
- [x] **2** — idle state machine, settings model, AC/battery profiles
- [x] **3a** — daemon: idle detection, power/session/resume events
- [x] **3b** — tray icon and menu, autostart, single instance, restore-on-exit
- [x] **4** — settings window
- [x] **5** — first-run hint, OSD suppression, driver-failure surfacing

## Licence

MIT
