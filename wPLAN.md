# w-sender 実装計画

## 目的

Windows 専用の低遅延 sender として `w-sender` を作る。

`w-sender` は、VB-CABLE / WASAPI capture が音声データを渡したタイミングを送信イベントとして扱い、その場で UDP packet にして receiver へ送り出す。sender 側で 5ms timer などを使って packet 間隔を作り直さない。

Windows アプリから VB-CABLE に音が出て、VB-CABLE Output / WASAPI capture で `w-sender` に届くまでの時間は、sender が制御できない固定レイテンシーとして扱う。`w-sender` はその固定レイテンシーを補正しようとせず、観測とログだけを行う。

## 設計方針

- Windows 専用 binary: `w-sender.exe`
- 入力は VB-CABLE Output などの WASAPI capture device のみ
- capture event が来たら、可能な限り同じ thread で packetize して UDP send する
- packet pacing 用の `sleep` / timer は使わない
- capture queue は持たない
- receiver feedback による sender-side ASRC は使わない
- packet format は既存 receiver と互換の `W2MA` v1 を使う
- 送信 payload は 48kHz / stereo / s16le を基本とする
- sample clock は capture device から実際に届いた frame 数を基準にする

## レイテンシーモデル

`w-sender` で扱う時間を次のように分ける。

```text
Windows app playback
  -> VB-CABLE Input
  -> VB-CABLE internal path
  -> VB-CABLE Output / WASAPI capture buffer
  -> w-sender capture event
  -> UDP send
  -> receiver jitter/output
```

このうち、`Windows app playback -> w-sender capture event` は固定の外部レイテンシーとして扱う。ここは sender が packet pacing や ASRC で改善できる場所ではない。

`w-sender` の責務は、`capture event -> UDP send` をできるだけ短く、かつ揺らさないことに絞る。

## 既存 sender との違い

現在の `sender` は汎用 sender として以下を持つ。

- CPAL capture callback
- capture queue
- `latest` / `fifo` queue mode
- optional packet pacing
- optional sender-side ASRC
- Windows 以外でも動く構成

`w-sender` はこの汎用性を捨て、Windows / VB-CABLE capture のイベントをそのまま送信タイミングにする。

既存 `sender` は残す。`w-sender` は低遅延 Windows 実験用の別 binary として追加する。

## 実装単位

追加する想定ファイル:

```text
w-sender/
  Cargo.toml
  src/
    main.rs
    args.rs
    wasapi.rs
    packetizer.rs
    metrics.rs

scripts/
  windows-w-sender.ps1
  windows-w-sender.bat
```

変更する想定ファイル:

```text
Cargo.toml
common/src/packet.rs
README.md
TODO.md
```

`common/src/packet.rs` には、既存 `sender` 内にある packet byte writer 相当を移す。`w-sender` では payload 用 `Vec<i16>` を別途作らず、事前確保した byte buffer に header と s16le payload を直接書く。

## CLI

初期 CLI:

```powershell
w-sender.exe `
  --target 192.168.x.x:50000 `
  --bind 0.0.0.0:0 `
  --device "CABLE Output" `
  --max-packet-frames 240 `
  --metrics-interval-sec 1.0
```

オプション:

- `--list-devices`: WASAPI capture device 一覧を表示
- `--device <NAME_PART>`: capture device 名の部分一致
- `--target <ADDR>`: receiver の UDP address
- `--bind <ADDR>`: sender UDP bind address
- `--max-packet-frames <N>`: 1 UDP packet に入れる最大 frame 数
- `--require-48k-stereo`: 48kHz stereo 以外なら起動失敗
- `--duration-sec <SEC>`: テスト用の自動終了
- `--metrics-interval-sec <SEC>`: metrics 出力間隔

`--packet-ms` は持たせない。必要なら `--max-packet-frames` で MTU 回避用の分割上限だけを指定する。

## Packet 化ルール

原則は「event chunk を待たずに送る」。

```text
WASAPI event で N frames 届く
  -> format convert
  -> max-packet-frames 以下に split
  -> split packet を同じ event 処理内で連続 send
```

例:

- event で 128 frames 届いたら、128 frames packet を即送る
- event で 480 frames 届いたら、240 + 240 frames の 2 packet を即送る
- event で 512 frames 届いたら、240 + 240 + 32 frames の 3 packet を即送る

固定サイズ packet を作るために、次の event まで待たない。小さい packet になっても、VB-CABLE が渡したタイミングを優先する。

`sample_position` は送信した frame 数で連続増加させる。packet が可変 frame 数でも receiver は header の `frames` と `sample_position` を基準に処理できるようにする。

## WASAPI 方針

MVP は native WASAPI event-driven capture で実装する。

使う API:

- `IMMDeviceEnumerator`
- `IMMDevice`
- `IAudioClient`
- `IAudioCaptureClient`
- `IAudioClient::Initialize`
- `IAudioClient::SetEventHandle`
- `IAudioClient::GetService`
- `IAudioCaptureClient::GetNextPacketSize`
- `IAudioCaptureClient::GetBuffer`
- `IAudioCaptureClient::ReleaseBuffer`

`IAudioClient::Initialize` は shared mode + event callback を基本にする。

```text
AUDCLNT_SHAREMODE_SHARED
AUDCLNT_STREAMFLAGS_EVENTCALLBACK
AUDCLNT_STREAMFLAGS_NOPERSIST
```

exclusive mode は初期実装では入れない。VB-CABLE / Windows 側の安定性確認後、必要な証拠がある場合だけ検討する。

## Thread model

```text
main thread
  - parse args
  - open UDP socket
  - select WASAPI device
  - spawn capture event thread
  - print metrics

capture event thread
  - enter MMCSS Pro Audio
  - wait WASAPI event
  - drain all available capture packets
  - packetize
  - UDP send immediately
```

capture event thread では以下を避ける。

- heap allocation
- lock 待ち
- timer sleep
- receiver feedback 待ち
- ログの大量出力

metrics は atomic counter に積み、main thread が一定間隔で読む。

## UDP 方針

- `UdpSocket::bind(bind)`
- `UdpSocket::connect(target)`
- send path は `send()` を使う
- socket は nonblocking を検討する
- `WouldBlock` は packet drop として数える
- send error は metrics に出し、連続エラー時は終了するか警告継続にする

capture event thread が network I/O を直接行うため、UDP send が詰まったときは古い音を溜めずに drop する。低遅延 sender なので backlog は作らない。

## Format 方針

Phase 1 では 48kHz / stereo を最優先にする。

- VB-CABLE 側を Windows の Sound 設定で 48kHz stereo に合わせる
- WASAPI mix format が 48kHz stereo ならそのまま送る
- float32 / s16 / s24 / s32 は s16le payload に変換する
- mono は stereo duplicate する
- 48kHz 以外は `--require-48k-stereo` なら起動失敗

Phase 2 で必要なら streaming resampler を足す。ただし `w-sender` の目的は「VB-CABLE のイベントをそのまま送る」ことなので、resampler は最小限にする。receiver feedback による動的 ASRC は入れない。

## Metrics

出力したい metrics:

```text
w-sender:
  device="CABLE Output ..."
  format=48000Hz/2ch/F32
  event_rate=100.0/s
  event_gap_max=10.3ms
  event_frames_avg=480
  packets=200.0/s
  bitrate=1.62Mbps
  send_error=0
  send_would_block=0
  discontinuity=0
  silent_frames=0
  event_to_send_max=0.35ms
```

重要な観測点:

- `event_gap_max`: VB-CABLE / WASAPI からデータが来る間隔の最大値
- `event_to_send_max`: event を受けてから最後の UDP packet を出すまでの最大時間
- `discontinuity`: WASAPI が discontinuity flag を返した回数
- `silent_frames`: WASAPI が silent flag を返した frame 数

これにより、問題が `Windows -> capture event` の固定側にあるのか、`event -> UDP send` の sender 側にあるのかを分けて見られる。

## 実装フェーズ

### Phase 0: 既存資産の整理

- `sender/src/main.rs` の packet writer を `common` へ移せる形にする
- 既存 `sender` の挙動を変えない
- `common` に packet writer の unit test を追加する

完了条件:

- `cargo test -p lan-audio-common`
- 既存 `sender` が build できる

### Phase 1: w-sender crate skeleton

- workspace に `w-sender` crate を追加
- `clap` で CLI を定義
- Windows 以外では「unsupported」と表示して終了
- UDP socket open / connected send の最小実装を作る
- `--list-devices` はまだ stub でもよい

完了条件:

- `cargo check -p w-sender`
- `w-sender --help` が読める

### Phase 2: WASAPI device list / open

- capture device 一覧を出す
- `--device "CABLE Output"` で部分一致選択する
- selected device 名、mix format、default period を表示する
- 48kHz stereo 判定を入れる

完了条件:

- Windows 実機で `w-sender --list-devices`
- `CABLE Output` が選べる
- mix format がログに出る

### Phase 3: Event capture loop

- WASAPI shared event capture を開始する
- event thread を MMCSS `Pro Audio` に上げる
- event ごとに `GetNextPacketSize` / `GetBuffer` / `ReleaseBuffer` を drain する
- 音声はまだ送らず、RMS と event metrics だけ出す

完了条件:

- Windows の再生音に応じて RMS が動く
- `event_gap_max` が観測できる
- 無音時の `silent_frames` が観測できる

### Phase 4: Immediate UDP send

- event chunk を直接 packetize する
- `max-packet-frames` で split する
- split packet は同じ event 処理内で連続 send する
- `sample_position` / `sequence` / `send_time_ns` を更新する
- send buffer は事前確保し、通常 path で allocation しない

完了条件:

- localhost receiver に届く
- receiver が可変 frame 数 packet を処理できる
- `event_to_send_max` が 1ms 未満を目標に観測できる

### Phase 5: Windows launcher

- `scripts/windows-w-sender.ps1` を追加
- `scripts/windows-w-sender.bat` を追加
- default device は `CABLE Output`
- default target は環境変数で上書き可能にする
- release build 起動を簡単にする

完了条件:

- PowerShell から list devices
- bat から release w-sender 起動
- log file に command と metrics が残る

### Phase 6: 実機検証

検証順:

1. Windows: `w-sender --list-devices`
2. Windows: `w-sender --device "CABLE Output" --duration-sec 10` で event/RMS 確認
3. Windows localhost: receiver WAV 保存
4. Windows localhost: receiver audio 出力
5. Windows -> macOS: receiver direct path で音出し
6. Windows -> macOS: 10分連続
7. Windows -> macOS: 1時間連続

確認すること:

- packet loss / send error が増えない
- receiver 側 underrun が増えない
- `event_gap_max` と receiver 側の揺れが対応しているか
- `event_to_send_max` が十分小さいか
- 体感遅延は固定 offset として安定しているか

## Done 条件

- `w-sender.exe` が `CABLE Output` を選べる
- VB-CABLE が渡した event chunk を timer pacing なしで UDP 送信できる
- 既存 receiver が `w-sender` の packet を受けられる
- sender 側 metrics で `event_gap_max` と `event_to_send_max` を分けて見られる
- Windows -> macOS で 1時間、音切れなしまたは原因が metrics で説明できる状態になる

## やらないこと

- Windows の再生アプリから VB-CABLE Output に届くまでの固定レイテンシー補正
- sender-side ASRC
- receiver feedback による送信 rate 制御
- timer ベースの packet pacing
- capture backlog を溜める queue
- ASIO 対応
- WASAPI exclusive mode の初期実装

## 追加検討

イベント到着そのものが 10ms 単位で固まる場合、`w-sender` 側ではそれ以上細かく送れない。その場合は次の順で切り分ける。

1. VB-CABLE / Windows Sound 設定の sample rate と buffer を確認する
2. WASAPI default period / minimum period をログに出す
3. event gap が固定なら、その gap を Windows 側固定レイテンシーとして扱う
4. それでも用途に足りない場合だけ、ASIO や別仮想ケーブルを検討する

