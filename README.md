# my-lan-audio

`my-lan-audio` は、PC の音声を LAN/UDP で別マシンへ送るための実験用 audio bridge です。

主な想定は Windows の `VB-CABLE Output` を sender で capture し、macOS の receiver で受けて BlackHole や物理スピーカーへ出す構成です。現在は macOS 1台だけでも、`BlackHole -> sender -> receiver -> MacBook のスピーカー` のように信号を流して検証できます。

## 現在の状態

2026-07-07 時点の実装は macOS-first で安定性改善まで入っています。

実装済み:

- Rust workspace: `common`, `sender`, `receiver`
- UDP packet format: 48 kHz / stereo / signed 16-bit little-endian PCM
- sender input: `dummy`, `sine`, WAV file, live capture
- sender-side capture resampling: input device が 44.1 kHz でも 48 kHz packet に変換
- receiver output: `null`, WAV file, CoreAudio output
- receiver-side output resampling: output device が 44.1 kHz / 48 kHz どちらでも出力
- jitter buffer, loss / late / duplicate / out-of-order metrics
- receiver-only adaptive resampling with PI correction
- receiver -> sender feedback status UDP
- receiver audio callback は SPSC ring 読み取り専用
- `JitterBuffer` は renderer / timed output 側が単独所有し、audio callback や UDP receive thread と `Mutex<JitterBuffer>` を共有しない
- output ring の既定値は `40ms`
- queue drop / ring underrun / steady underrun などの安定性 metrics

未採用の大きな設計案:

- sender-side ASRC / pacing
- receiver clock master の pull model
- Dante / AES67 / RTP / PTP 互換
- 暗号化、認証、複数送信元、マルチキャスト

Windows capture adapter のコードはありますが、Windows -> macOS の長時間実機検証は別途必要です。

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
  -> CoreAudio callback
  -> output device
```

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
  --target-buffer-ms 80 \
  --start-threshold-ms 80 \
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
  --output audio \
  --output-device "MacBook Proのスピーカー" \
  --target-buffer-ms 100 \
  --start-threshold-ms 100 \
  --max-buffer-ms 250
```

Terminal 2: sender

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --feedback-listen 127.0.0.1:50001 \
  --input capture \
  --device "BlackHole"
```

確認ポイント:

- sender の `rms=.../...dB` が再生音に応じて動く
- sender の `remote_latency`, `remote_outq`, `remote_qdrop` が表示される
- receiver の `packets` と `queued` が約 `200/s`
- receiver の `qdrop=0.0/s`, `steady_under=0`, `ring_under=0.0/s`
- receiver の `latency` が `target` 付近に留まる

注意:

- receiver の出力先に BlackHole を選ぶと、スピーカーからは聞こえません。聞く場合は `MacBook Proのスピーカー` や headphones を選んでください。
- システム出力を BlackHole にすると、通常のスピーカーからは直接音が出なくなります。このアプリの receiver がスピーカーへ戻す役割になります。
- feedback port は audio port と別です。上の例では audio が `50000`, feedback が `50001` です。

## Windows -> macOS

macOS 側 receiver:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output audio \
  --output-device "BlackHole" \
  --target-buffer-ms 100 \
  --start-threshold-ms 100 \
  --max-buffer-ms 250
```

Windows 側 sender:

```powershell
sender.exe `
  --target <macOSのLAN IP>:50000 `
  --input capture `
  --device "CABLE Output"
```

sender 側で receiver feedback も見る場合:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --feedback-target <WindowsのLAN IP>:50001 \
  --output audio \
  --output-device "BlackHole"
```

```powershell
sender.exe `
  --target <macOSのLAN IP>:50000 `
  --feedback-listen 0.0.0.0:50001 `
  --input capture `
  --device "CABLE Output"
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

CoreAudio output:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output audio \
  --output-device "BlackHole"
```

低遅延寄りの localhost 設定例:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 127.0.0.1:50000 \
  --output audio \
  --output-device "MacBook Proのスピーカー" \
  --target-buffer-ms 80 \
  --start-threshold-ms 80 \
  --max-buffer-ms 160 \
  --output-ring-ms 40 \
  --output-ring-capacity-ms 200 \
  --render-chunk-ms 5
```

さらに低い latency を狙う場合は `--output-ring-ms 20` も指定できますが、macOS の thread scheduling によって `ring_under` が出やすくなります。安定性優先では default の `40ms` 以上を推奨します。

主な receiver option:

```text
--listen <ADDR>                         UDP listen address。default: 0.0.0.0:50000
--feedback-target <ADDR>                senderへstatusを返すUDP address
--output null|audio|wav                 出力先。default: null
--output-device <NAME_PART>             CoreAudio output device name filter
--output-file <PATH>                    WAV保存。指定するとwav output扱い
--list-devices                          出力デバイス一覧
--test-tone                             receiver単体でtest tone出力
--sample-rate <HZ>                      packet sample rate。現在は48000のみ
--channels <N>                          channel数。現在は2のみ
--capacity-ms <MS>                      jitter buffer容量。default: 1000
--target-buffer-ms <MS>                 目標jitter buffer水位。default: 100
--max-buffer-ms <MS>                    trim開始目安。default: 300
--start-threshold-ms <MS>               再生開始まで貯める量。default: 100
--kp / --ki                             latency correctionのPI係数
--error-filter-alpha <A>                buffer error low-pass係数
--max-ppm <PPM>                         通常補正上限。default: 1000
--emergency-max-ppm <PPM>               大きいズレ用の補正上限。default: 5000
--no-adaptive-resampling                adaptive resamplingを無効化
--output-buffer-size-frames <FRAMES>    CoreAudio buffer size固定指定
--output-ring-ms <MS>                   CoreAudio callback前のSPSC ring目標量。default: 40
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
        remote_latency=...ms remote_outq=...ms remote_qdrop=0 ...
```

見るところ:

- `packets`: 5ms packet なら約 `200/s`
- `bitrate`: 48kHz / stereo / 16-bit なら約 `1.6Mbps`
- `rms`: capture 音量。無音なら `-120dB` 付近
- `dropped`, `errors`: sender 側送信異常
- `remote_latency`: receiver の jitter buffer 水位
- `remote_outq`: receiver の output ring 水位
- `remote_qdrop`: receiver の UDP thread -> renderer queue drop
- `remote_steady_under`, `remote_ring_under`: 通常運転中の underrun
- `remote_ratio`: receiver の実効 resampling ratio

receiver:

```text
receiver: state=Running packets=200.0/s queued=200.0/s qdrop=0.0/s qinvalid=0.0/s
          loss=0.0/s late=0.0/s dup=0.0/s ooo=0.0/s
          latency=75.0ms target=80ms outq=40.0ms
          ratio=0.999970 drift=...ppm startup_under=... steady_under=0
          lock_miss=0.0/s ring_under=0.0/s ring_missing=0/s ring_overflow=0/s
```

安定している目安:

- `packets` と `queued` が約 `200/s`
- `qdrop=0.0/s`
- `loss=0.0/s`, `late=0.0/s`, `dup=0.0/s`, `ooo=0.0/s`
- `steady_under=0`
- `ring_under=0.0/s`
- `lock_miss=0.0/s`
- `latency` が `target` 付近
- `outq` が `--output-ring-ms` 付近

`startup_under` は receiver 起動直後や sender 開始前の priming 中に増えることがあります。通常運転の評価では `steady_under`, `ring_under`, `qdrop` を重視してください。

## latency について

ログ上の主な latency は以下です。

```text
latency / remote_latency: receiver jitter buffer 内の音声水位
outq / remote_outq: CoreAudio callback手前のSPSC output ring水位
target: jitter bufferの目標水位
```

実際に耳で感じる遅延は、おおむね以下の合計です。

```text
jitter buffer latency
+ output ring latency
+ CoreAudio / device / Bluetooth 側の遅延
+ capture device 側の遅延
```

default では `target-buffer-ms=100` と `output-ring-ms=40` なので、安定性優先の設定です。localhost や有線LANで攻める場合は `target-buffer-ms=80`, `output-ring-ms=40` 程度から試すのが現実的です。

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
frames per packet: 240
payload bytes per packet: 960
nominal packet rate: 200 packets/s
nominal bitrate: 1.536 Mbps + UDP/IP overhead
```

5ms packet は UDP payload が通常の Ethernet MTU 1500 bytes 未満に収まるようにしています。

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

receiver の output ring が薄いか、renderer thread が CoreAudio callback に追いついていません。

まず安定寄りにします。

```bash
--output-ring-ms 40 \
--output-ring-capacity-ms 200 \
--render-chunk-ms 5
```

それでも増える場合:

- `--target-buffer-ms 100` 以上にする
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
