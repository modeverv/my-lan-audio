# my-lan-audio

Windows/macOS LAN audio bridge prototype.

The current implementation follows `PLAN.md`'s macOS-first path:

- fixed 48 kHz / stereo / signed 16-bit little-endian PCM packets
- UDP packet header with stream id, sequence, sample position, and sender monotonic timestamp
- dummy, sine, WAV, and live capture sender inputs
- packet loss, jitter, reorder, and drift simulation options on the sender
- receiver-side packet validation, sequence/loss/duplicate/late metrics
- sample-position based jitter buffer with silence fill for missing frames
- buffer-level based adaptive linear resampling
- receiver outputs for `null`, WAV file, and CoreAudio output devices such as BlackHole
- receiver test tone output for CoreAudio/BlackHole checks
- cpal-backed capture adapter for Windows WASAPI and other host APIs

## Toolchain

This repo pins Rust through mise:

```bash
mise exec -- rustc -Vv
mise exec -- cargo test
```

The checked-in `mise.toml` selects `rust@1.95.0`. Use `mise exec -- ...` so the aarch64 toolchain is used on Apple Silicon.

## Build And Test

```bash
mise exec -- cargo fmt --all -- --check
mise exec -- cargo test
mise exec -- cargo run -p sender -- --help
mise exec -- cargo run -p receiver -- --help
```

## Local UDP Smoke Test

Terminal 1:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 127.0.0.1:50000 \
  --output-file /tmp/my-lan-audio-loopback.wav \
  --duration-sec 8 \
  --target-buffer-ms 50 \
  --start-threshold-ms 50
```

Terminal 2:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input sine \
  --duration-sec 2
```

Expected sender metrics are about 200 packets/sec and about 1.6 Mbps for 5 ms stereo PCM packets. The receiver should enter `Running` while packets arrive and write a 48 kHz stereo 16-bit WAV file.

## Sender Examples

Dummy packets:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input dummy
```

Sine packets with simulation:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input sine \
  --drop-rate 0.01 \
  --jitter-ms 20 \
  --reorder-rate 0.01 \
  --drift-ppm 50
```

WAV input:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input-file test.wav \
  --loop-input
```

Capture input:

```bash
mise exec -- cargo run -p sender -- --list-devices
mise exec -- cargo run -p sender -- \
  --input capture \
  --device "CABLE Output" \
  --target 192.168.11.20:50000
```

Meter-only and capture-to-WAV modes:

```bash
mise exec -- cargo run -p sender -- \
  --device "CABLE Output" \
  --meter-only

mise exec -- cargo run -p sender -- \
  --device "CABLE Output" \
  --output-file capture.wav \
  --duration-sec 10
```

## Receiver Examples

Null output for metrics-only soak tests:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output null
```

WAV output:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output-file received.wav
```

BlackHole/CoreAudio output:

```bash
mise exec -- cargo run -p receiver -- --list-devices
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output audio \
  --output-device "BlackHole" \
  --target-buffer-ms 100
```

Receiver-side test tone:

```bash
mise exec -- cargo run -p receiver -- \
  --test-tone \
  --output audio \
  --output-device "BlackHole" \
  --duration-sec 10
```

## Current Verification Status

Verified locally on macOS:

- `cargo fmt --all -- --check`
- `cargo test`
- `sender --help`
- `receiver --help`
- sine sender to localhost UDP receiver to WAV output

Still requires real devices or long-running manual runs:

- BlackHole output observed by another macOS app
- Windows `CABLE Output` capture on a Windows host
- Windows to macOS end-to-end run
- 1-hour and 24-hour soak tests

