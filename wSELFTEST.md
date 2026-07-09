# w-sender localhost self-test notes

Date: 2026-07-09
Host: Windows, VB-Audio Virtual Cable present

## Build / device checks

Commands:

```powershell
mise exec -- cargo test --workspace
mise exec -- cargo build --release -p receiver -p w-sender
mise exec -- cargo run -p w-sender -- --list-devices
mise exec -- cargo run -p receiver -- --list-devices
```

Results:

- Workspace tests passed: 41 tests total across `common`, `receiver`, and `sender`; `w-sender` has no unit tests yet.
- Release build passed for `receiver` and `w-sender`.
- `w-sender --list-devices` found `CABLE Output (2- VB-Audio Virtual Cable)`.
- `receiver --list-devices` found the default Realtek speaker and VB-CABLE render devices.

Observed VB-CABLE capture format:

```text
48000Hz/2ch/F32/32bit block_align=8
```

## w-sender standalone check

Command:

```powershell
mise exec -- cargo run -p w-sender -- `
  --target 127.0.0.1:50000 `
  --device "CABLE Output" `
  --duration-sec 3 `
  --metrics-interval-sec 1 `
  --max-packet-frames 240 `
  --require-48k-stereo
```

Result:

- WASAPI mode: shared event capture
- VB-CABLE event size: 480 frames
- UDP packetization: 240 + 240 frames per event
- Event rate: about 100 events/s
- Packet rate: about 200 packets/s
- Sender send errors: 0
- Sender would-blocks: 0
- Event gap max: about 11.3-11.6ms
- Event-to-send max: under 1ms; observed 0.696ms or lower in the standalone run

Interpretation:

`w-sender` is doing the intended event-driven send. The 10ms event cadence is coming from VB-CABLE / WASAPI, not from sender-side packet pacing.

## localhost receiver self-test

Release binaries were used:

```powershell
target\release\receiver.exe
target\release\w-sender.exe
```

Baseline command shape:

```powershell
target\release\receiver.exe `
  --listen 127.0.0.1:<PORT> `
  --fixed-delay-frames <FRAMES> `
  --duration-sec 6 `
  --metrics-interval-sec 1

target\release\w-sender.exe `
  --target 127.0.0.1:<PORT> `
  --device "CABLE Output" `
  --duration-sec 7 `
  --metrics-interval-sec 1 `
  --max-packet-frames 240 `
  --require-48k-stereo
```

Important setting note:

- `--output-buffer-size-frames 128` broke on this Realtek device because the actual callback delivered larger buffers and receiver reported continuous `scratch_overflow`.
- `--output-buffer-size-frames 480` mostly worked but still showed occasional scratch overflow.
- For the jitter-buffer floor sweep below, receiver output buffer was left at backend default. This keeps the receiver implementation unchanged and avoids forcing a buffer size the device does not honor.

## receiver fixed-delay floor sweep

CSV:

```text
logs/floor-defaultbuf-summary-20260709-231105.csv
```

Summary:

| fixed delay | ms | state | steady under | resyncs | scratch overflow |
| ---: | ---: | --- | ---: | ---: | ---: |
| 480 fr | 10.000 ms | Running | 0 | 0 | 0 |
| 360 fr | 7.500 ms | Running | 0 | 0 | 0 |
| 240 fr | 5.000 ms | Running | 0 | 0 | 0 |
| 120 fr | 2.500 ms | Running | 0 | 0 | 0 |
| 60 fr | 1.250 ms | Running | 0 | 0 | 0 |
| 30 fr | 0.625 ms | Running | 0 | 0 | 0 |
| 12 fr | 0.250 ms | Running | 0 | 0 | 0 |
| 6 fr | 0.125 ms | Running | 0 | 0 | 0 |
| 1 fr | 0.021 ms | Running | 0 | 0 | 0 |

Longer confirmation:

```text
logs/long1fr-receiver-20260709-231231.out.log
logs/long1fr-w-sender-20260709-231231.out.log
```

Result for 1 frame / 30 seconds:

- Receiver state stayed `Running`.
- `steady_under=0`
- `missing_calls=0.0/s`
- `missing_frames=0/s`
- `qdrop=0.0/s`
- `loss=0.0/s`
- `resyncs=0`
- `scratch_overflow=0.0/s`
- Sender `send_error=0.0/s`
- Sender `send_would_block=0.0/s`
- Sender `event_to_send_max` stayed below 1ms in the displayed tail.

Interpretation:

For localhost with silent VB-CABLE capture on this machine, receiver's internal fixed-delay buffer can be lowered to `1` source frame without breaking. This does not mean end-to-end audible latency is 1 frame, because Windows playback, VB-CABLE, WASAPI event cadence, output callback, and hardware latency still add fixed external delay. It only proves that the receiver jitter buffer itself is not the limiting factor in this localhost setup.

