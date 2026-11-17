# logid

Control a Logitech MX Master 4 mouse from a Linux PC (haptic feedback, DPI, scroll wheel, battery) without going through Logi Options+.

`logid` is a small Rust daemon (no dependencies besides `libc`) that talks to the mouse over Logitech's HID++ 2.0 protocol through a `hidraw` node, and exposes a text command interface on a Unix socket. `logictl` is the matching command-line client.

```console
$ logictl vibrate
played sharp-state-change
$ logictl vibrate happy-alert
played happy-alert
```

> **Name clash:** [logiops](https://github.com/PixlOne/logiops) also installs a binary called `logid`. Don't install both.

## Requirements

- Linux. To build from source: a Rust toolchain (edition 2021); no runtime dependency
- The mouse paired to a Logi Bolt (or Unifying) receiver, or connected over Bluetooth
- Read/write access to the receiver's `hidraw` node. Solaar's udev rules grant it to the logged-in user (on NixOS: `hardware.logitech.wireless.enable = true`).

## Installation

### Nix

The repository is a flake that provides `packages.<system>.default` and `overlays.default`, which install the `logid` and `logictl` commands.

```console
$ nix run .            # try it without installing
$ nix profile install .
```

### From the repository

```console
$ cargo build --release
$ ./target/release/logid
$ ./target/release/logictl vibrate
```

`cargo install --path .` installs the `logid` and `logictl` commands. `nix develop` provides the toolchain.

## Usage

Start the daemon:

```console
$ logid
INFO logid.ipc: listening on /run/user/1000/logid.sock
```

| Option | Description |
|---|---|
| `--hidraw PATH` | HID++ `hidraw` node (default: the first Logitech HID++ interface found) |
| `--index N` | Device index: `1`–`6` on a receiver, `0xff` over Bluetooth (default: the first device that supports the requested feature) |
| `--socket PATH` | Unix socket path (default: `$XDG_RUNTIME_DIR/logid.sock`) |
| `-v`, `--verbose` | Log every report exchanged with the receiver |

Then send commands with the client:

| Command | Effect |
|---|---|
| `vibrate [waveform]` | Play a haptic waveform (default: `sharp-state-change`) |
| `dpi [value]` | Show the DPI (current, default, accepted range) or set it; a value the sensor does not accept is refused |
| `scroll [ratchet\|freespin\|auto N]` | Show or set the SmartShift wheel mode; `auto N` (1–50) is ratchet that frees the wheel above speed N |
| `wheel [invert on\|off] [hires on\|off]` | Show or set scroll direction inversion and high-resolution scrolling (other wheel bits are preserved) |
| `battery` | Show the battery level and charging state, e.g. `80% (discharging)` |
| `help` | List the commands (also the default when no command is given) |

Waveforms: `sharp-state-change`, `damp-state-change`, `sharp-collision`, `damp-collision`, `subtle-collision`, `happy-alert`, `angry-alert`, `completed`, `square`, `wave`, `firework`, `mad`, `knock`, `jingle`, `ringing`, `whisper-collision`. A waveform the mouse does not advertise is refused.

`logictl` exits with status 1 on error and prints the message on stderr, so it can be used in scripts and keyboard shortcuts. Both the daemon and the client honour the `LOGID_SOCKET` environment variable.

## How it works

The `hidraw` node is opened on the first request and kept open. On the first request, the daemon asks each receiver slot for the feature it needs and keeps the first device that has it. This lookup, the feature indexes and the device's haptic capabilities are cached, and forgotten when the device stops answering, so the next request searches again. If the receiver is unplugged, the next request reopens it.

> **The mouse must be awake.** A sleeping mouse cannot be reached: the receiver answers `connection failed` and the command fails. Move the mouse and try again.

`hidraw` is not exclusive: logid can run alongside Solaar or logiops. Its requests use their own software ID (`0x0A`), so replies are not mixed up.

### Architecture

```
logictl ──(Unix socket)──▶ ipc.rs ──▶ request.rs ──▶ hidpp.rs ──(hidraw)──▶ receiver ──▶ mouse
```

| Module | Role |
|---|---|
| `src/request.rs` | Business logic: knows feature IDs, function numbers and payloads. It receives a `Backend` (the device) and calls `request(feature_id, function, params)`. |
| `src/hidpp.rs` | Transport: `hidraw` discovery, device index lookup, feature index resolution, request/reply matching, HID++ errors. It does not interpret payloads. |
| `src/ipc.rs` | Unix socket server (one thread per client; requests are serialised on a mutex). It translates each text line into a call to `request.rs` and writes back the result. |
| `src/paths.rs`, `src/log.rs` | Socket path resolution; minimal stderr logger. |
| `src/bin/logid.rs` | Daemon: CLI options, signals (SIGTERM/SIGINT shut down cleanly). |
| `src/bin/logictl.rs` | Command-line client (standard library only). |

### IPC protocol

One UTF-8 text line per request and one line per response, prefixed with `ok:` or `err:`. A client may send several requests on the same connection.

```console
$ echo vibrate | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/logid.sock
ok: played sharp-state-change
```

The socket is created with mode `0600`. On startup, a stale socket left by a crashed instance is removed. A second instance refuses to start if another one is already listening.

## HID++ protocol

The protocol is documented by Logitech for some features and was reverse-engineered by [Solaar](https://github.com/pwr-Solaar/Solaar), which this project follows.

The receiver's HID++ interface is the `hidraw` node whose report descriptor uses the vendor usage page `0xFF00` and declares the reports `0x10` (short, 7 bytes) and `0x11` (long, 20 bytes). logid only sends long reports:

```
11 | device index | feature index | function << 4 | software ID | 16 bytes of parameters
```

- The device index is the receiver slot (`1`–`6`), or `0xFF` for a device connected directly.
- Features have a fixed 16-bit ID but a per-device index. The root feature (index `0`), function `0` (`getFeature`), takes the feature ID and returns its index (`0` means not supported).
- The reply repeats the first 4 bytes, followed by the result.
- A device error is `11 | dev | FF | feature index | function/swid | code`. An error from the receiver (device asleep, empty slot) is the short report `10 | dev | 8F | feature index | function/swid | code`.

| Request | Feature | Function | Parameters | Reply |
|---|---|---|---|---|
| Get feature index | `0x0000` Root | 0 | feature ID (2 bytes) | index, type |
| Haptic capabilities | `0x19B0` Haptic | 0 | — | bytes 4–7: bitmask of supported waveforms |
| Play waveform | `0x19B0` Haptic | 4 | waveform ID | — |
| Get DPI list | `0x2201` Adjustable DPI | 1 | sensor (0) | sensor, then big-endian words: list values, or `min, 0xE000\|step, max` ranges; 0 ends |
| Get DPI | `0x2201` Adjustable DPI | 2 | sensor (0) | sensor, current DPI (2 bytes), default DPI (2 bytes) |
| Set DPI | `0x2201` Adjustable DPI | 3 | sensor (0), DPI (2 bytes) | — |
| Get SmartShift | `0x2111` (else `0x2110`) | 1 (else 0) | — | mode (1 freespin, 2 ratchet), auto-disengage (`0xFF` never, 1–50) |
| Set SmartShift | `0x2111` (else `0x2110`) | 2 (else 1) | mode, auto-disengage | — |
| Get wheel mode | `0x2121` Hi-res wheel | 1 | — | mode bits: 0 divert, 1 hi-res, 2 invert |
| Set wheel mode | `0x2121` Hi-res wheel | 2 | mode bits | — |
| Battery | `0x1004` Unified battery (else `0x1000`) | 1 (else 0) | — | percent, level, charging status |

The DPI, SmartShift, wheel and battery feature IDs and payloads follow Solaar; they are not yet tested on MX Master 4 hardware. SmartShift and battery try the newer feature first and fall back to the older one when the device does not have it.

## Limitations and roadmap

- Only tested with the MX Master 4 on a Logi Bolt receiver.
- DPI, scroll, wheel and battery commands are untested on real hardware.
- Planned: haptic level (feature `0x19B0`, functions 1 and 2), a `systemd --user` service.
