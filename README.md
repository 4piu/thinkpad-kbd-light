# thinkpad-kbd-light

Keyboard backlight control for ThinkPads on Windows.

ThinkPad firmware forgets the keyboard backlight level across sleep and reboot, and always
comes back dark. This fixes that, and adds idle dimming, idle timeout, and separate
profiles for AC and battery.

**Status: milestone 2 — hardware layer and core logic done.** The `tkbl` CLI can read and
set the backlight, and the idle state machine and settings model are implemented and
tested. The tray application is not built yet.

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
- [ ] **3** — tray app: idle detection, power/session events
- [ ] **4** — settings window
- [ ] **5** — autostart, single instance, failure surfacing

## Licence

MIT
