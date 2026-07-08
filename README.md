# my-lan-audio

`my-lan-audio` は、PC の音声を LAN/UDP で別マシンへ送るための実験用 audio bridge です。

主な想定は Windows の `VB-CABLE Output` を sender で capture し、macOS の receiver で受けて BlackHole や物理スピーカーへ出す構成です。現在は macOS 1台だけでも、`BlackHole -> sender -> receiver -> MacBook のスピーカー` のように信号を流して検証できます。

## 現在の状態

2026-07-07 時点の実装は macOS-first で安定性改善まで入っています。
2026-07-08 に Windows/MSVC host での build check、WASAPI デバイス一覧確認、Windows 向け PowerShell launcher、CPAL の PCM/float sample format 対応拡張を追加しました。同日に localhost 低レイテンシー検証用として、receiver の direct audio path と packet arrival clock sync も追加しています。

実装済み:

- Rust workspace: `common`, `sender`, `receiver`
- UDP packet format: 48 kHz / stereo / signed 16-bit little-endian PCM
- sender input: `dummy`, `sine`, WAV file, live capture
- sender-side capture resampling: input device が 44.1 kHz でも 48 kHz packet に変換
- receiver output: `null`, WAV file, CPAL audio output (CoreAudio / WASAPI)
- receiver-side output resampling: output device が 44.1 kHz / 48 kHz どちらでも出力
- jitter buffer, loss / late / duplicate / out-of-order metrics
- fixed-buffer playout scheduler: `--fixed-delay-frames <FRAMES>` は receiver 内部の `jitter + output ring` 合算予算。`--fixed-latency-ms <MS>` は人間向け alias
- receiver -> sender feedback status UDP
- receiver の既定 `ring` path では audio callback は SPSC ring 読み取り専用
- `JitterBuffer` は renderer / timed output 側が単独所有し、audio callback や UDP receive thread と `Mutex<JitterBuffer>` を共有しない
- receiver の UDP thread -> renderer queue は満杯時に古いeventを捨て、新しいpacketを残す
- renderer は output ring 水位と UDP event 到着で起床し、固定sleepによる余分な待ちを抑える
- receiver audio output は既定の `ring` path に加えて、callback が `JitterBuffer` から直接 pull する `direct` path を選べる
- receiver `--clock-sync packet` は packet arrival 時刻を基準に、receiver 側 playout 位置を前方へ追いつかせる
- sender live capture は処理遅れ時に古いcapture chunkを捨て、最新chunkへ追いつく
- output ring の既定値は `40ms`
- queue drop / ring underrun / steady underrun などの安定性 metrics

未採用の大きな設計案:

- receiver clock master の pull model
- PTP などによる sender / receiver 間の明示的な絶対時刻 network clock sync
- Dante / AES67 / RTP / PTP 互換
- 暗号化、認証、複数送信元、マルチキャスト

Windows capture / playback のコードと helper script はありますが、Windows -> macOS の長時間実機検証は別途必要です。

## 構成

```text
.
├── Cargo.toml
├── mise.toml
├── PLAN.md
├── UPDATE.md
├── README.md
├── common/
│   └── src/
│       ├── audio.rs
│       ├── jitter.rs
│       ├── packet.rs
│       ├── resampler.rs
│       └── status.rs
├── sender/
│   └── src/main.rs
└── receiver/
    ├── src/audio_ring.rs
    └── src/main.rs
```

実行時の大まかな流れ:

```text
capture / sine / wav
  -> sender
  -> UDP audio packets
  -> receiver UDP thread
  -> bounded packet event queue
  -> renderer-owned JitterBuffer
  -> SPSC output ring
  -> CPAL audio callback
  -> output device
```

低レイテンシー検証用の `--audio-path direct` では、renderer thread と SPSC output ring を通さず、CPAL audio callback が `JitterBuffer` を直接所有して pull します。これは receiver 側の実装切り替えなので、Windows sender の capture / UDP packet 送信実装はそのまま使います。

## 必要なもの

共通:

- `mise`
- Rust `1.95.0`。`mise.toml` で指定済み

macOS で仮想音声ループを試す場合:

- BlackHole などの仮想オーディオデバイス
- 出力先として使う物理デバイス。例: `MacBook Proのスピーカー`, Bluetooth headphones

Windows で実音声を送る場合:

- VB-Audio Virtual Cable
- Windows 側で Rust build できる環境
- PowerShell 5 以降
- 入力デバイスとして `CABLE Output` が見える状態

## セットアップ

```bash
mise install
mise exec -- rustc -Vv
mise exec -- cargo --version
```

Apple Silicon Mac では `rustc -Vv` の `host` が `aarch64-apple-darwin` になっていることを確認してください。

## ビルド

開発ビルド:

```bash
mise exec -- cargo build
```

release ビルド:

```bash
mise exec -- cargo build --release
```

生成される主なバイナリ:

```text
target/debug/sender
target/debug/receiver
target/release/sender
target/release/receiver
target/debug/sender.exe       # Windows
target/debug/receiver.exe     # Windows
target/release/sender.exe     # Windows
target/release/receiver.exe   # Windows
```

## テスト

```bash
mise exec -- cargo fmt --all -- --check
mise exec -- cargo clippy --all-targets -- -D warnings
mise exec -- cargo test
```

CLI option の確認:

```bash
mise exec -- cargo run -p sender -- --help
mise exec -- cargo run -p receiver -- --help
```

## まず動かす: WAV loopback

macOS 1台で UDP 経路と jitter buffer を確認します。receiver を先に起動してください。

Terminal 1:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 127.0.0.1:50000 \
  --output wav \
  --output-file /tmp/my-lan-audio-loopback.wav \
  --fixed-delay-frames 14400 \
  --duration-sec 5
```

Terminal 2:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input sine \
  --duration-sec 3
```

期待値:

- sender が約 `200 packets/s` で送る
- bitrate が約 `1.6 Mbps`
- receiver が `state=Running` になる
- `loss`, `late`, `dup`, `ooo`, `qdrop` が 0
- `/tmp/my-lan-audio-loopback.wav` が生成される

## まず動かす: receiver 単体の音出し

UDP を使わず、receiver が output device を開けるか確認します。

出力デバイス一覧:

```bash
mise exec -- cargo run -p receiver -- --list-devices
```

test tone:

```bash
mise exec -- cargo run -p receiver -- \
  --test-tone \
  --output audio \
  --output-device "MacBook Proのスピーカー" \
  --duration-sec 5
```

`--output-device` は部分一致です。表示名に合わせて `"SOUNDPEATS"`, `"BlackHole"`, `"MacBook"` など短めに指定できます。

## macOS 1台で BlackHole -> sender -> receiver -> スピーカー

システム音声や YouTube Music を BlackHole に出し、それを sender で capture して、receiver から物理スピーカーへ戻す確認手順です。

1. macOS のシステムサウンド出力を `BlackHole 2ch` にする
2. sender 側で BlackHole が入力デバイスとして見えることを確認する
3. receiver 側で MacBook のスピーカーなど物理出力が見えることを確認する
4. receiver を起動する
5. sender を起動する
6. YouTube Music などを再生する

入力デバイス一覧:

```bash
mise exec -- cargo run -p sender -- --list-devices
```

出力デバイス一覧:

```bash
mise exec -- cargo run -p receiver -- --list-devices
```

Terminal 1: receiver

```bash
mise exec -- cargo run -p receiver -- \
  --listen 127.0.0.1:50000 \
  --feedback-target 127.0.0.1:50001 \
  --fixed-delay-frames 14400 \
  --output audio \
  --output-device "MacBook Proのスピーカー" \
  --output-ring-ms 60 \
  --output-ring-capacity-ms 160 \
  --render-chunk-ms 2
```

Terminal 2: sender

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --feedback-listen 127.0.0.1:50001 \
  --input capture \
  --device "BlackHole 2ch"
```

確認ポイント:

- sender の `rms=.../...dB` が再生音に応じて動く
- sender の `remote_buf`, `remote_outq`, `remote_total`, `remote_qdrop` が表示される
- receiver の `packets` と `queued` が約 `200/s`
- receiver の `qdrop=0.0/s`, `steady_under=0`, `ring_under=0.0/s`
- receiver の `total_buf` が固定値付近に留まる

注意:

- receiver の出力先に BlackHole を選ぶと、スピーカーからは聞こえません。聞く場合は `MacBook Proのスピーカー` や headphones を選んでください。
- システム出力を BlackHole にすると、通常のスピーカーからは直接音が出なくなります。このアプリの receiver がスピーカーへ戻す役割になります。
- feedback port は audio port と別です。上の例では audio が `50000`, feedback が `50001` です。

### 固定バッファ Makefile shortcut

localhost で固定バッファ設定を試す場合は、receiver を先に起動してから sender を起動します。Makefile の既定値は `FIXED_DELAY_FRAMES=14400`、つまり 48kHz 基準で `300ms` です。

Terminal 1:

```bash
make receiver
```

Terminal 2:

```bash
make sender
```

`make receive` も `make receiver` の alias です。

既定値は以下です。

```text
receiver output device: system default
sender input device: BlackHole 2ch
receiver listen: 0.0.0.0:50000
sender target: 127.0.0.1:50000
sender feedback listen: 0.0.0.0:50001
fixed-delay-frames: 14400
fixed-latency-ms alias: 300
packet-ms: 5
sender-side ASRC: enabled
```

デバイス名や固定遅延は make 変数で上書きできます。

```bash
make receiver RECEIVER_OUTPUT_DEVICE="BlackHole 2ch"
make receiver FIXED_DELAY_FRAMES=9600
make receiver FIXED_DELAY_FRAMES= FIXED_LATENCY_MS=300
make sender SENDER_INPUT_DEVICE="BlackHole" PACKET_MS=5
```

direct path + packet clock sync で 1 frame まで詰める場合は、release build の低遅延 shortcut を使います。

Terminal 1:

```bash
make d-receiver
```

Terminal 2:

```bash
make d-sender
```

`d-receiver` の既定値は `DIRECT_FIXED_DELAY_FRAMES`, `DIRECT_OUTPUT_SAMPLE_RATE`, `DIRECT_OUTPUT_BUFFER_SIZE_FRAMES`, `DIRECT_CLOCK_SYNC` で調整できます。`DIRECT_CLOCK_SYNC=on` は packet arrival clock sync を有効にします。`packet` も同じ意味で、`off` は無効です。`d-sender` は `DIRECT_PACKET_MS`, `DIRECT_CAPTURE_QUEUE_CAPACITY`, `DIRECT_CAPTURE_QUEUE_MODE`, `DIRECT_CAPTURE_PACKET_PACING` で送信packet幅、capture queue容量、読み取り方式、packet pacingを調整します。既定の `DIRECT_CAPTURE_QUEUE_MODE=fifo` はcapture済みchunkを古い順に送り、`latest` は低遅延維持のために古いchunkを捨てます。`DIRECT_CAPTURE_PACKET_PACING=on` はcapture chunkを一気送信せず、`DIRECT_PACKET_MS` の間隔で1packetずつ送ります。どちらも起動時に `logs/d-receiver-YYYYmmdd-HHMMSS.log` / `logs/d-sender-YYYYmmdd-HHMMSS.log` へ build と実行ログを残します。`logs/` は git 管理外です。

## Windows で動かす

PowerShell の実行ポリシーで `.ps1` を直接実行できない場合は、以下のように `-ExecutionPolicy Bypass` 付きで起動してください。以降の例はプロジェクトルートから実行します。

sender input device 一覧:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\windows-sender.ps1 -ListDevices
```

receiver output device 一覧:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\windows-receiver.ps1 -ListDevices
```

Windows 1台で UDP 経路だけ確認する場合:

Terminal 1:

```powershell
.\scripts\windows-receiver.ps1 `
  -Listen 127.0.0.1:50000 `
  -Output wav `
  -OutputFile .\target\windows-loopback.wav `
  -LatencyMode normal `
  -TargetBufferMs 80 `
  -StartThresholdMs 80 `
  -DurationSec 5
```

Terminal 2:

```powershell
mise exec -- cargo run -p sender -- `
  --target 127.0.0.1:50000 `
  --input sine `
  --duration-sec 3
```

Windows の VB-CABLE を capture して Windows のスピーカーへ戻す場合:

Terminal 1:

```powershell
.\scripts\windows-receiver.ps1 `
  -Listen 127.0.0.1:50000 `
  -FeedbackTarget 127.0.0.1:50001 `
  -Output audio `
  -OutputDevice "スピーカー"
```

Terminal 2:

```powershell
.\scripts\windows-sender.ps1 `
  -Target 127.0.0.1:50000 `
  -FeedbackListen 127.0.0.1:50001 `
  -Device "CABLE Output"
```

release build で起動する場合は各 script に `-Release` を付けます。`-OutputDevice` / `-Device` は部分一致なので、デバイス一覧に出た名前の一部で指定できます。

## Windows -> macOS

macOS 側 receiver:

```bash
make mac-ip
make p-receiver RECEIVER_OUTPUT_DEVICE="BlackHole 2ch"
```

Windows 側 sender:

```powershell
.\scripts\windows-sender.ps1 `
  -Target <make mac-ipで出たmacOSのLAN IP>:50000 `
  -Device "CABLE Output"
```

sender 側で receiver feedback も見る場合:

```bash
make p-receiver \
  RECEIVER_OUTPUT_DEVICE="BlackHole 2ch" \
  RECEIVER_FEEDBACK_TARGET=<WindowsのLAN IP>:50001
```

```powershell
.\scripts\windows-sender.ps1 `
  -Target <macOSのLAN IP>:50000 `
  -FeedbackListen 0.0.0.0:50001 `
  -Device "CABLE Output"
```

## sender の使い方

dummy packet:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input dummy
```

sine wave:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input sine \
  --freq 440
```

WAV file:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input-file test.wav
```

WAV loop:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input-file test.wav \
  --loop-input
```

capture meter のみ:

```bash
mise exec -- cargo run -p sender -- \
  --input capture \
  --device "BlackHole" \
  --meter-only
```

capture を WAV 保存:

```bash
mise exec -- cargo run -p sender -- \
  --input capture \
  --device "BlackHole" \
  --output-file /tmp/capture.wav \
  --duration-sec 10
```

packet loss / jitter simulation:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input sine \
  --drop-rate 0.01 \
  --jitter-ms 20 \
  --reorder-rate 0.01 \
  --drift-ppm 50
```

主な sender option:

```text
--target <ADDR>                  UDP送信先。default: 127.0.0.1:50000
--bind <ADDR>                    UDP bind address。default: 0.0.0.0:0
--feedback-listen <ADDR>         receiver statusを受けるUDP address
--input dummy|sine|capture       入力種別。default: sine
--input-file <PATH>              WAV入力
--device <NAME_PART>             capture device name filter
--list-devices                   入力デバイス一覧
--meter-only                     capture meterのみ
--output-file <PATH>             captureをWAV保存
--sample-rate <HZ>               packet sample rate。現在は48000のみ
--channels <N>                   channel数。現在は2のみ
--packet-ms <MS>                 packet duration。default: 5
--capture-queue-capacity <N>     live capture chunk queue容量。default: 32
--capture-queue-mode latest|fifo live capture queue読み取り方式。default: latest
--capture-packet-pacing off|on   live capture packet送信をpacket-ms間隔へ整形。default: off
--sender-side-asrc               receiver feedbackでsender送出レートを微調整
--sender-asrc-kp <K>             sender-side ASRCの比例係数
--sender-asrc-max-ppm <PPM>      sender-side ASRC補正上限
--duration-sec <SEC>             指定秒数で終了
--metrics-interval-sec <SEC>     metrics表示間隔
--drop-rate <RATE>               packet drop simulation
--jitter-ms <MS>                 jitter simulation
--reorder-rate <RATE>            reorder simulation
--drift-ppm <PPM>                sender pacing drift simulation
```

## receiver の使い方

metrics のみ:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output null
```

WAV 保存:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output wav \
  --output-file received.wav
```

Audio output:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output audio \
  --output-device "BlackHole"
```

固定遅延 300ms 設定:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --feedback-target <senderのLAN IP>:50001 \
  --output audio \
  --output-device "BlackHole" \
  --fixed-delay-frames 14400 \
  --output-ring-ms 60 \
  --output-ring-capacity-ms 160 \
  --render-chunk-ms 2
```

`--fixed-delay-frames` は 48kHz packet domain の frame 数です。`14400` は `300ms` です。`--fixed-latency-ms 300` も指定できますが、内部では `14400` frames に変換されます。

主な receiver option:

```text
--listen <ADDR>                         UDP listen address。default: 0.0.0.0:50000
--feedback-target <ADDR>                senderへstatusを返すUDP address
--output null|audio|wav                 出力先。default: null
--output-device <NAME_PART>             audio output device name filter
--output-file <PATH>                    WAV保存。指定するとwav output扱い
--audio-path ring|direct                audio output経路。default: ring
--clock-sync off|packet|on              packet arrival基準のreceiver側clock追従。on は packet と同義。default: off
--list-devices                          出力デバイス一覧
--test-tone                             receiver単体でtest tone出力
--sample-rate <HZ>                      packet sample rate。現在は48000のみ
--channels <N>                          channel数。現在は2のみ
--fixed-delay-frames <FRAMES>           receiver内部の合算固定 playout delay。default: 14400
--fixed-latency-ms <MS>                 人間向けalias。48kHz framesへ変換
--output-buffer-size-frames <FRAMES>    audio backend buffer size固定指定
--output-sample-rate <HZ>               audio backend sample rate指定。例: 48000
--output-ring-ms <MS>                   audio callback前のSPSC ring目標量。default: 40
--output-ring-capacity-ms <MS>          SPSC ring容量。default: 200
--render-chunk-ms <MS>                  rendererの生成chunk。default: 5
--socket-recv-buffer-bytes <BYTES>      UDP socket receive buffer。default: 1048576
--packet-queue-capacity <N>             UDP thread -> renderer queue容量。default: 2048
--duration-sec <SEC>                    指定秒数で終了
--metrics-interval-sec <SEC>            metrics表示間隔
```

## metrics の読み方

sender:

```text
sender: input=capture packets=200.0/s bitrate=1.619Mbps sequence=...
        capture_buffer=...ms rms=-20.8/-20.8dB dropped=0 errors=0
        capture_qdrop=0.0/s capture_qdrop_frames=0/s capture_lock_miss=0.0/s
        remote_buf=...fr/...ms remote_outq=...fr/...ms remote_total=...fr/...ms remote_qdrop=0 ...
```

見るところ:

- `packets`: 5ms packet なら約 `200/s`、2.5ms packet なら約 `400/s`
- `bitrate`: 48kHz / stereo / 16-bit なら約 `1.6Mbps`
- `rms`: capture 音量。無音なら `-120dB` 付近
- `dropped`, `errors`: sender 側送信異常
- `capture_qdrop`: sender live capture queue で低遅延維持のために捨てた古いchunk
- `capture_qdrop_frames`: 捨てたcapture frame数
- `capture_lock_miss`: capture callback が queue lock を取れず chunk を捨てた回数
- `remote_buf`: receiver の jitter buffer 水位
- `remote_outq`: receiver の output ring 水位
- `remote_total`: receiver 内部の合算 buffer 水位
- `remote_qdrop`: receiver の UDP thread -> renderer queue drop
- `remote_steady_under`, `remote_ring_under`: 通常運転中の underrun
- `send_corr`: sender-side ASRC の補正量
- `remote_ratio`: receiver の実効 resampling ratio

receiver:

```text
receiver: state=Running packets=200.0/s queued=200.0/s qdrop=0.0/s qinvalid=0.0/s
          loss=0.0/s late=0.0/s dup=0.0/s ooo=0.0/s
          buf=12480fr/260.0ms fixed=14400fr/300.0ms outq=1920fr/40.0ms total_buf=14400fr/300.0ms
          ratio=0.999970 drift=...ppm startup_under=... steady_under=0
          lock_miss=0.0/s ring_under=0.0/s ring_missing=0/s ring_overflow=0/s
```

安定している目安:

- `packets` と `queued` が約 `200/s`
- `qdrop=0.0/s`。非ゼロなら receiver queue が古いeventを捨てて最新packetを優先している
- `loss=0.0/s`, `late=0.0/s`, `dup=0.0/s`, `ooo=0.0/s`
- `steady_under=0`
- `ring_under=0.0/s`
- `lock_miss=0.0/s`
- `total_buf` が固定 delay 付近
- `outq` が `--output-ring-ms` 付近

`startup_under` は receiver 起動直後や sender 開始前の priming 中に増えることがあります。通常運転の評価では `steady_under`, `ring_under`, `qdrop` を重視してください。

## latency について

ログ上の主な buffer / latency は以下です。

```text
buf / remote_buf: receiver jitter buffer 内の音声水位
outq / remote_outq: audio callback手前のSPSC output ring水位
total_buf / remote_total: buf + outqを48kHz source frame換算で合算した値
fixed / remote_fixed: 設定された固定delay frames
```

実際に耳で感じる遅延は、おおむね以下の合計です。

```text
jitter buffer latency
+ output ring latency
+ audio backend / device / Bluetooth 側の遅延
+ capture device 側の遅延
```

receiver は固定バッファ playout のみです。既定値は `--fixed-delay-frames 14400` で、48kHz 基準の 300ms です。この値は receiver 内部の `jitter buffer + output ring` の合算予算として扱われます。renderer は output ring に積まれた分を jitter 側の必要水位から差し引きます。

```text
fixed / remote_fixed = 14400fr
outq / remote_outq = audio callback手前のoutput ring水位
total_buf / remote_total ~= fixed
```

`--fixed-delay-frames 14400` かつ `--output-ring-ms 40` の場合、output ring が約 `1920fr` なら jitter buffer は約 `12480fr` を目標にします。receiver 内部の buffered latency は合算でおおむね `14400fr` です。実際に耳やDAWで観測される遅延には、そこへ backend/device/capture 側の固定遅延が加わります。DAW 側で固定補正する場合は、クリックやパルスで実測して補正値を決めてください。

audio output では `--output-ring-ms` が固定delay内に収まる必要があります。例えば `--fixed-latency-ms 20` を指定する場合、`--output-ring-ms` は 20ms 未満にしてください。

`--audio-path direct` は renderer thread と output ring を通さず、CPAL/CoreAudio callback が `JitterBuffer` から直接 pull します。localhost の低レイテンシー測定では、例えば以下の条件で callback 前 ring の待ちを外せます。

```bash
./target/release/receiver \
  --listen 127.0.0.1:50000 \
  --output audio \
  --output-device "BlackHole" \
  --audio-path direct \
  --output-sample-rate 48000 \
  --output-buffer-size-frames 64 \
  --fixed-latency-ms 1
```

さらに `--clock-sync on` または `--clock-sync packet` を使うと、最初に受け取った packet の sample position と receiver 到着時刻を anchor にして、callback 実行時刻から playout 位置を計算します。これは PTP / NTP のようなホスト間の絶対時刻同期ではなく、receiver 内部で packet arrival clock に追従する仕組みです。

```bash
./target/release/receiver \
  --listen 127.0.0.1:50000 \
  --output audio \
  --output-device "BlackHole" \
  --audio-path direct \
  --clock-sync packet \
  --output-sample-rate 48000 \
  --output-buffer-size-frames 64 \
  --fixed-delay-frames 1
```

2026-07-08 の localhost 測定では、`sender --input sine --target 127.0.0.1:50000 --packet-ms 1.0` と組み合わせて、`--audio-path direct` は `--fixed-latency-ms 1` まで clean、`--audio-path direct --clock-sync packet` は `--fixed-delay-frames 1` まで receiver 内部 metrics 上 clean でした。ただしこれは `total_buf` で見た receiver 内部 buffered latency です。実際の音として観測される遅延には audio callback period、CoreAudio / device / capture 側の固定遅延が加わります。

## bit depth と clipping

wire format は 16-bit PCM 固定です。内部処理と resampling は `f32` で行い、送信 packet / WAV 書き出し時に 16-bit へ変換します。

24-bit 化は現時点では未実装です。音量が 0 dBFS を超えている信号は bit depth に関係なく clipping するため、clipping が気になる場合は送信元またはシステム音量を少し下げてください。24-bit 化で clipping 耐性が大きく上がるというより、量子化ノイズ余裕が増える、という性質です。

## 固定仕様

現時点の UDP audio packet は以下に固定しています。

```text
sample rate: 48000 Hz
channels: 2
sample format: signed 16-bit little endian PCM
default packet duration: 5 ms
optional packet duration: configurable by --packet-ms
frames per packet: 240 at 5 ms, 120 at 2.5 ms, 48 at 1 ms
payload bytes per packet: 960 at 5 ms, 480 at 2.5 ms, 192 at 1 ms
nominal packet rate: 200 packets/s at 5 ms, 400 packets/s at 2.5 ms, 1000 packets/s at 1 ms
nominal bitrate: 1.536 Mbps + UDP/IP overhead
```

5ms / 2.5ms / 1ms packet はいずれも UDP payload が通常の Ethernet MTU 1500 bytes 未満に収まるようにしています。

## トラブルシュート

### receiver の device list に MacBook のスピーカーが出ない

```bash
mise exec -- cargo run -p receiver -- --list-devices
```

表示されない場合:

- macOS のサウンド設定や Audio MIDI Setup で目的の出力デバイスが有効か確認する
- Bluetooth headphone は接続済みか確認する
- `cargo run` ではなく build 済み binary を使っている場合、古い binary を見ていないか `mise exec -- cargo build` する

### BlackHole にしているのに音が聞こえない

BlackHole は仮想デバイスです。システム出力を BlackHole にすると、通常のスピーカーからは直接聞こえません。

聞くための構成:

```text
System Output = BlackHole
sender --input capture --device "BlackHole"
receiver --output audio --output-device "MacBook Proのスピーカー"
```

録音や配信アプリへ渡す構成:

```text
receiver --output audio --output-device "BlackHole"
録音/配信アプリ側 input = BlackHole
```

### `Sample rate ... is not supported` が出る

現在の receiver は output device の default output sample rate を使い、receiver 側で resampling します。古い build を実行している可能性があるので、まず再ビルドしてください。

```bash
mise exec -- cargo build
mise exec -- cargo run -p receiver -- --list-devices
```

### `ring_under` が増える

receiver の output ring が薄いか、renderer thread が audio callback に追いついていません。

まず安定寄りにします。

```bash
--output-ring-ms 40 \
--output-ring-capacity-ms 200 \
--render-chunk-ms 5
```

それでも増える場合:

- `--fixed-delay-frames` を大きくする。例: `14400` から `19200`
- Bluetooth 出力ではなく内蔵スピーカーや有線出力で確認する
- 他のCPU負荷を下げる

### `qdrop` / `remote_qdrop` が増える

UDP receive thread から renderer への bounded queue が詰まっています。

```bash
--packet-queue-capacity 4096
```

を試してください。通常の localhost / LAN では 0 のままが期待値です。

### receiver に packet が届かない

- receiver を先に起動する
- `--listen 0.0.0.0:50000` または正しい IP にする
- sender の `--target` が receiver の IP:port を指しているか確認する
- macOS Firewall / Windows Defender Firewall を確認する
- まず `127.0.0.1` の loopback を通す

### sender が `CABLE Output` / `BlackHole` を見つけない

```bash
mise exec -- cargo run -p sender -- --list-devices
```

実際に表示された名前の一部を `--device` に指定してください。

## 開発メモ

- 詳細な実装計画は `PLAN.md`
- localhost underrun / stability review は `UPDATE.md`
- 長時間テストや Windows 実機検証が進んだら、この README と `PLAN.md` の検証状況を更新してください。
