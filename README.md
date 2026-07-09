# my-lan-audio

`my-lan-audio` は、PC の音声を LAN/UDP で別マシンへ送るための実験用 audio bridge です。

現在の主な実用構成は以下です。

```text
Windows VB-CABLE Output
  -> Windows sender
  -> UDP over LAN
  -> macOS receiver
  -> BlackHole 2ch または macOS の物理出力
```

Dante / AES67 のような汎用 audio network 互換を目指すのではなく、手元の LAN で使えること、ログで状態を読めること、設定を少数に保つことを優先しています。現時点の安定運用目標は `30ms` receiver buffer + Windows sender feedback です。

## 現在の状態

現在の実運用パスは次の通りです。

- `sender`: live capture 専用
- `receiver`: direct CPAL/CoreAudio output
- packet format: 48kHz stereo, 16-bit PCM over UDP
- sender 側で capture device rate から 48kHz へ resampling
- receiver 側で output device rate へ resampling
- Windows sender の packet dispatch thread は MMCSS / high priority 化済み
- capture callback 内 allocation を避けるため、capture queue chunk を preallocate
- receiver -> sender feedback UDP により sender-side ASRC が送出レートを微調整
- receiver log に buffer percentile、arrival gap、sender send gap を出力
- sender log に capture callback gap、packet dispatch gap を出力

VNC で Windows に接続したまま多少雑に操作しても、現在の `30ms` 構成では実用的に安定しています。より低い buffer も動きますが、OS scheduling の揺れを直接受けるため余裕は薄くなります。

実用レンジの目安:

```text
1440 frames / 30ms   安定運用目標
720 frames / 15ms    攻めた設定
480 frames / 10ms    かなり攻めた設定
360 frames / 7.5ms   実験用
```

## 構成

```text
.
├── common/              packet, audio, resampler, jitter, status 共通実装
├── receiver/            UDP receiver と audio output
├── sender/              capture sender
├── scripts/
│   ├── windows-sender.bat
│   ├── windows-sender.ps1
│   └── windows-receiver.ps1
├── Makefile             macOS 側の起動 shortcut
├── TODO.md              latency work の残り
└── README.md
```

## 必要なもの

macOS receiver:

- `mise`
- `mise.toml` で指定された Rust
- `BlackHole 2ch`、MacBook speakers、headphones などの出力先

Windows sender:

- PowerShell から Rust build できる環境
- VB-Audio Virtual Cable
- `CABLE Output` のような capture input device
- PowerShell 5 以降

## セットアップとビルド

macOS:

```bash
mise install
mise exec -- rustc -Vv
mise exec -- cargo build --release
```

Windows では、repository root または `scripts/` から launcher を使います。内部では `mise exec -- cargo run --release -p sender -- ...` を呼びます。

```bat
scripts\windows-sender.bat
```

## 現在の安定起動

まず Windows sender を起動します。`scripts\windows-sender.bat` の既定値は、現在の `30ms` receiver 運用に寄せています。

```text
target:                  192.168.11.65:50000
feedback listen:         0.0.0.0:50001
capture device:          CABLE Output
packet ms:               10
capture queue capacity:  16
capture queue mode:      fifo
capture packet pacing:   on
input buffer frames:     256
```

同じ設定は環境変数で上書きできます。

```bat
set LAN_AUDIO_TARGET=192.168.11.65:50000
set LAN_AUDIO_FEEDBACK_LISTEN=0.0.0.0:50001
set LAN_AUDIO_DEVICE=CABLE Output
set LAN_AUDIO_PACKET_MS=10
set LAN_AUDIO_CAPTURE_QUEUE_CAPACITY=16
set LAN_AUDIO_CAPTURE_QUEUE_MODE=fifo
set LAN_AUDIO_CAPTURE_PACKET_PACING=on
set LAN_AUDIO_INPUT_BUFFER_SIZE_FRAMES=256
scripts\windows-sender.bat
```

次に macOS receiver を起動します。

```bash
make d-receiver
```

`make d-receiver` は現在、release receiver を次の設定で起動します。

```text
--listen 0.0.0.0:50000
--feedback-target 192.168.11.96:50001
--output-device "BlackHole 2ch"
--fixed-delay-frames 1440
--clock-sync on
--output-sample-rate 48000
--output-buffer-size-frames 32
```

Windows sender の LAN IP が違う場合は、feedback host だけ指定します。

```bash
make d-receiver SENDER_FEEDBACK_HOST=<Windows sender LAN IP>
```

または feedback target 全体を指定します。

```bash
make d-receiver RECEIVER_FEEDBACK_TARGET=<Windows sender LAN IP>:50001
```

feedback を一時的に無効にする場合:

```bash
make d-receiver RECEIVER_FEEDBACK_TARGET=
```

`d-receiver` と `d-sender` は `logs/` に timestamp 付き log を出します。`logs/` は git 管理外です。

## Makefile targets

```bash
make d-receiver
make d-sender
```

よく使う override:

```text
SENDER_FEEDBACK_HOST=<Windows sender LAN IP>
RECEIVER_FEEDBACK_TARGET=<Windows sender LAN IP>:50001
RECEIVER_OUTPUT_DEVICE="BlackHole 2ch"
DIRECT_FIXED_DELAY_FRAMES=1440
DIRECT_OUTPUT_SAMPLE_RATE=48000
DIRECT_OUTPUT_BUFFER_SIZE_FRAMES=32
DIRECT_CLOCK_SYNC=on
LOG_DIR=logs
```

`make d-sender` は主に macOS / local test 用です。実用中の Windows sender profile は `scripts/windows-sender.bat` 側にあります。

## Windows sender script

入力デバイス一覧:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\windows-sender.ps1 -ListDevices
```

明示的に起動する場合:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\windows-sender.ps1 `
  -Target 192.168.11.65:50000 `
  -FeedbackListen 0.0.0.0:50001 `
  -Device "CABLE Output" `
  -PacketMs 10 `
  -CaptureQueueCapacity 16 `
  -CaptureQueueMode fifo `
  -CapturePacketPacing on `
  -InputBufferSizeFrames 256 `
  -Release
```

`-FeedbackListen` を指定すると、`-NoSenderSideAsrc` を付けない限り sender-side ASRC が有効になります。

## metrics の読み方

### receiver

主な receiver fields:

```text
packets / queued          packet 受信 rate と queue accepted rate
loss / late / dup / ooo   network / sequence 異常
qdrop / qinvalid          receiver queue drop または invalid packet
buf                       現在の jitter buffer 深さ
fixed                     設定された fixed receiver delay
buf_min / p05 / p50 / p95 直近 window の buffer 深さ
arrival_gap_max           receiver 側で見た最大 packet 到着間隔
send_gap_max              sender timestamp 上の最大送信間隔
steady_under              steady-state underrun
missing_calls             output call 内で音声 frame が欠けた回数
resyncs                   stream change / underrun resync count
scratch_overflow          output scratch buffer overflow
```

`30ms` profile で安定している目安:

```text
steady_under=0
missing_calls=0.0/s
missing_frames=0/s
loss=0.0/s
qdrop=0.0/s
resyncs が増え続けない
buf_min が通常は数ms以上残る
```

`startup_under` は receiver 起動直後、sender restart、priming 中に増えることがあります。通常運転の評価では `steady_under`、`missing_calls`、`resyncs` を重視します。

### sender

主な sender fields:

```text
packets                    packet 送信 rate
capture_buffer             未送信 capture backlog
errors                     UDP send errors
send_corr                  receiver feedback による sender-side ASRC 補正
pacing_drop_frames         packet pacing が捨てた frames
capture_callback_gap_max   capture callback 間隔の区間内最大値
packet_dispatch_gap_max    UDP packet dispatch 間隔の区間内最大値
capture_qdrop              capture queue drops
capture_lock_miss          capture callback が queue lock を取れなかった回数
remote_status              最新 receiver feedback、または waiting
```

`packet-ms=10` では `packets` は約 `100/s` です。`packet_dispatch_gap_max` が 10ms 前後なのは packet size 的に自然です。現在の Windows / VB-CABLE 経路では `capture_callback_gap_max` が 10-20ms になることがあり、これを吸収するために receiver 側 `30ms` を安定目標にしています。

`remote_status=waiting` は sender が receiver feedback を受け取れていない状態です。次を確認します。

- Windows sender が `0.0.0.0:50001` で feedback listen している
- macOS receiver が `--feedback-target <Windows IP>:50001` 付きで起動している
- firewall が UDP feedback を止めていない

## 安定性の判断

receiver は fixed delay が実際の arrival / send gap より大きい間は、sender や capture の塊化を吸収できます。現在の安定設定では:

- `packet-ms=10` で 1ms packet の scheduling pressure を避ける
- Windows capture callback は 10-20ms 単位で来ることがある
- `fixed-delay-frames=1440` で receiver 側に 30ms の余白を作る
- feedback により sender-side ASRC が長期 drift を補正する

receiver metrics が clean なのに音がぶちぶちする場合は、receiver buffer underrun ではなく、capture source や output monitoring path を疑います。

よくある原因:

- Windows sender 側の VNC / screen capture load
- VB-CABLE に入ってくる音自体の glitch
- macOS 側で BlackHole を monitor しているアプリの glitch
- feedback 未接続による長期 drift

## 低 latency 実験

receiver frame 数は次のように上書きできます。

```bash
make d-receiver DIRECT_FIXED_DELAY_FRAMES=720
make d-receiver DIRECT_FIXED_DELAY_FRAMES=480
make d-receiver DIRECT_FIXED_DELAY_FRAMES=360
```

実測上の意味:

```text
720fr / 15ms   動くが余白は少ない
480fr / 10ms   OS scheduling limit 付近
360fr / 7.5ms  動くことはあるが buffer floor に触れやすい
```

低 latency を試すときに見る値:

```text
buf_min
buf_p05
arrival_gap_max
send_gap_max
startup_under
steady_under
missing_calls
resyncs
```

`capture_callback_gap_max` がすでに 10-20ms なら、それより下の receiver delay は構造的に不安定になりやすいです。

## 開発チェック

```bash
mise exec -- cargo fmt --all -- --check
mise exec -- cargo test
```

macOS から Windows sender を cross-check する場合:

```bash
mise exec -- cargo check -p sender --target x86_64-pc-windows-msvc
```

launcher の展開確認:

```bash
make -n d-receiver
make -n d-sender
```

## トラブルシュート

### sender が `remote_status=waiting` のまま

feedback が接続できていません。receiver を次のように起動します。

```bash
make d-receiver SENDER_FEEDBACK_HOST=<Windows sender LAN IP>
```

Windows sender 側は `scripts\windows-sender.bat` の既定で feedback listen が有効です。

### receiver metrics は clean なのに音がぶちぶちする

`steady_under=0`、`missing_calls=0`、`loss=0`、`resyncs` が増えていないなら、receiver は枯渇していない可能性が高いです。sender capture source、VNC / screen capture load、BlackHole 後段の monitoring path を確認します。

### `capture_callback_gap_max` が 20ms 級

Windows capture data が UDP 送信前に塊で来ています。`30ms` receiver profile はこれを吸収するための設定です。さらに低 latency を狙う場合は `LAN_AUDIO_INPUT_BUFFER_SIZE_FRAMES` を変えるか、capture backend / device を見直します。

### device name が見つからない

device list を出し、表示名の一部を指定します。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\windows-sender.ps1 -ListDevices
```

```bash
target/release/receiver --list-devices
```

### BlackHole に出しているのに音が聞こえない

BlackHole は仮想デバイスです。receiver の出力先が `BlackHole 2ch` の場合、別アプリで BlackHole を monitor する必要があります。receiver から直接聞く場合は `RECEIVER_OUTPUT_DEVICE` に物理出力を指定します。

## Non-goals

現時点では以下は目標外です。

- Dante / AES67 / RTP / PTP 互換
- 暗号化、認証
- 複数送信元 mixer
- kernel driver や hard real-time audio system

現在の目標は、手元の LAN で使える、ログで状態を読める、調整しやすい audio bridge です。
