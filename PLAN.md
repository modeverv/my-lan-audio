# PLAN.md

# Windows VB-CABLE → LAN/UDP → macOS BlackHole 音声転送 実装計画

## 実装ステータス

2026-07-07 時点で、macOSファースト方針に沿ったRustワークスペース実装を追加済み。

完了:

```text
- [x] common: AudioPacketHeader
- [x] common: packet serialize / deserialize
- [x] common: sample conversion
- [x] common: jitter buffer
- [x] common: sequence欠落検出
- [x] common: late packet drop
- [x] common: buffer水位監視
- [x] common: linear resampler
- [x] common: adaptive resampling
- [x] macOS/local: dummy sender
- [x] macOS/local: sine sender
- [x] macOS/local: WAV sender
- [x] macOS/local: receiver metrics
- [x] macOS/local: receiver output.wav 保存
- [x] macOS/local: receiver null output
- [x] macOS/local: receiver CoreAudio / BlackHole 出力コード
- [x] macOS/local: receiver test tone outputコード
- [x] macOS/local: packet loss / jitter / reorder / drift simulation
- [x] Windows: cpal/WASAPI capture adapterコード
- [x] Windows: capture meterコード
- [x] Windows: capture WAV保存コード
- [x] Windows: WASAPI/cpal → UDP senderコード
```

この場で検証済み:

```text
- [x] mise rust@1.95.0 aarch64 toolchain
- [x] cargo fmt --all -- --check
- [x] cargo test
- [x] sender --help
- [x] receiver --help
- [x] sine sender → UDP localhost → receiver → WAV保存
```

実機または長時間実行が必要な未検証項目:

```text
- [ ] macOS: BlackHoleを実出力デバイスとして開き、別アプリで入力確認
- [ ] Windows: CABLE Outputデバイス一覧確認
- [ ] Windows: VB-CABLE Output capture meter確認
- [ ] Windows: VB-CABLE Output → capture.wav確認
- [ ] Windows → macOS: end-to-end確認
- [ ] macOS/local: 1時間テスト
- [ ] macOS/local: 24時間テスト
- [ ] Windows → macOS: 1時間テスト
- [ ] Windows → macOS: 24時間テスト
```

## 目的

Windows 側で `VB-CABLE Output` を WASAPI でキャプチャし、48kHz / 16bit / stereo の PCM 音声を UDP で LAN 越しに macOS へ送信する。

macOS 側では UDP を受信し、パケット順序・欠落・ジッターを吸収したうえで、CoreAudio 経由で `BlackHole` に出力する。

最終的には、BGM、映画、YouTube、PC音声転送用途で 24時間程度連続稼働しても、音切れ・バッファ肥大・バッファ枯渇が起きにくい実装を目指す。

---

## 想定構成

```text
Windows
  任意のアプリ
    ↓
  VB-CABLE Input
    ↓
  VB-CABLE Output
    ↓
  sender app
    - WASAPI capture
    - format conversion
    - packetize
    - UDP send

LAN / Wi-Fi
  UDP packets

macOS
  receiver app
    - UDP receive
    - reorder
    - jitter buffer
    - clock drift estimation
    - adaptive resampling
    - CoreAudio output
    ↓
  BlackHole output device
    ↓
  macOS上の録音/配信/加工/再生アプリ
```

---

## 基本仕様

### 音声フォーマット

初期実装では以下に固定する。

```text
sample rate: 48,000 Hz
channels: 2
sample format: signed 16-bit little endian PCM
frame size: 4 bytes
```

1秒あたりのデータ量:

```text
48,000 frames/sec × 2 channels × 2 bytes = 192,000 bytes/sec
= 1.536 Mbps
```

### パケット単位

初期値は 5ms 単位とする。

```text
48,000 Hz × 0.005 sec = 240 frames
240 frames × 2 channels × 2 bytes = 960 bytes
```

UDPペイロードはヘッダを含めてもおおむね 1KB 程度に収まるため、通常の Ethernet MTU 1500 bytes を超えない。

---

## 非目標

初期段階では以下はやらない。

```text
- Dante互換
- AES67互換
- PTP完全実装
- 複数送信元対応
- マルチキャスト配信
- 暗号化
- 認証
- 圧縮コーデック
- 楽器モニター用途の超低遅延化
```

まずは、単一 Windows → 単一 macOS のユニキャスト UDP 音声転送を安定させる。

---

## 推奨実装言語

候補:

```text
Rust
C++
Go
C#
```

推奨は Rust。

理由:

```text
- Windows / macOS の両方で動かしやすい
- UDP、リングバッファ、バイナリプロトコル、スレッド制御を書きやすい
- 低レベル処理と安全性のバランスが良い
- 将来的にGUIやサービス化もしやすい
```

ただし、WASAPI と CoreAudio の扱いに慣れているなら C++ でもよい。

以下の計画は言語非依存だが、実装イメージは Rust / C++ 寄りとする。

---

# 全体マイルストーン

## Phase 0: 実験用リポジトリ作成

目的:

```text
最小構成で sender / receiver を別プロセスとして起動できる状態にする。
```

成果物:

```text
/sender
/receiver
/common
README.md
PLAN.md
```

---

## Phase 1: UDP疎通確認

目的:

```text
音声ではなくダミーデータで Windows → macOS のUDP通信を確認する。
```

実装:

```text
sender:
  - 5msごとにsequence number付きパケットを送る

receiver:
  - UDPで受信
  - sequence numberを表示
  - 欠落数を表示
  - 到着間隔を表示
```

確認項目:

```text
- WindowsからmacOSへUDPが届く
- Windows Defender / macOS Firewallで止まっていない
- パケット欠落率が見える
- 到着間隔のジッターが見える
```

この段階では音声処理は不要。

---

## Phase 2: Windows側 WASAPI キャプチャ

目的:

```text
VB-CABLE Output から音声をキャプチャし、48kHz stereo PCMとして取り出す。
```

実装方針:

```text
1. Windowsの音声入力デバイス一覧を取得
2. 名前に "CABLE Output" を含むデバイスを探す
3. ユーザー指定で device id を選べるようにする
4. WASAPI shared mode でキャプチャ開始
5. 取得した音声を内部標準フォーマットへ変換
```

内部標準フォーマット:

```text
48kHz
stereo
float32 または int16
```

初期実装では内部処理は float32、送信時に int16 へ変換するのが扱いやすい。

理由:

```text
- WASAPIからfloat32で来ることが多い
- 後段でリサンプリングや音量調整をするならfloat32が扱いやすい
- ネットワーク送信はint16にすれば帯域が小さい
```

確認項目:

```text
- VB-CABLE Output から音声が取れる
- 無音時はほぼゼロになる
- Windows側で再生した音がsenderに入る
- レベルメーターを表示できる
```

ログ例:

```text
selected_device = "CABLE Output (VB-Audio Virtual Cable)"
input_format = 48000Hz, 2ch, float32
capture_started = true
rms_l = -18.2 dB
rms_r = -17.9 dB
```

---

## Phase 3: パケット形式設計

目的:

```text
sequence number、sample_position、monotonic timestamp、PCM frameを含むUDPパケットを定義する。
```

## UDPパケット構造

初期バージョン:

```text
struct AudioPacketHeader {
    magic:             u32   // "W2MA" など
    version:           u16   // 1
    header_size:       u16
    stream_id:         u64
    sequence:          u32
    flags:             u32
    sample_rate:       u32   // 48000
    channels:          u16   // 2
    sample_format:     u16   // 1 = s16le
    frames:            u16   // 240
    reserved:          u16
    sample_position:   u64   // stream開始からのframe index
    send_time_ns:      u64   // sender側monotonic clock
}
payload:
    int16 stereo PCM
```

サイズ感:

```text
header: 約56 bytes
payload: 960 bytes
total: 約1016 bytes
```

### sequence

パケットごとに +1 する。

用途:

```text
- 欠落検出
- 重複検出
- 順序入れ替わり検出
```

### sample_position

音声ストリーム開始時点を 0 とし、各パケットの先頭フレーム位置を入れる。

5msパケットなら:

```text
packet 0: sample_position = 0
packet 1: sample_position = 240
packet 2: sample_position = 480
```

用途:

```text
- jitter buffer上で正しい位置に並べる
- 欠落時にどの範囲が抜けたか分かる
- 長時間運用時の時間軸の基準にする
```

### send_time_ns

senderの monotonic clock を nanosecond 単位で入れる。

用途:

```text
- 到着ジッターの観測
- sender側生成時刻とreceiver側到着時刻の比較
- 将来のクロックドリフト推定
```

これは絶対時刻である必要はない。

---

## Phase 4: Windows sender の音声送信

目的:

```text
WASAPIで取った音声を5ms単位でUDP送信する。
```

処理パイプライン:

```text
WASAPI callback / capture loop
  ↓
input ring buffer
  ↓
format converter
  ↓
5ms packetizer
  ↓
UDP sender
```

### 実装手順

1. WASAPIから音声を取得する。
2. 入力が48kHzでない場合は48kHzへリサンプリングする。
3. 入力がmonoの場合はstereoへ変換する。
4. 入力がfloat32の場合はint16へ変換する。
5. 240 framesずつ切り出す。
6. AudioPacketHeaderを付ける。
7. UDPでmacOSのIPアドレスとポートへ送る。

### int16変換

float32入力を想定する場合:

```text
float x in [-1.0, 1.0]
int16 y = clamp(x * 32767.0, -32768, 32767)
```

### 送信タイミング

送信は基本的にキャプチャ駆動にする。

```text
WASAPIから音声が来る
  ↓
リングバッファに積む
  ↓
240 frames以上たまったら送る
```

送信スレッド側で5ms sleepして音声を生成するのではなく、実際の音声キャプチャクロックに従う。

理由:

```text
- Windows側の実音声クロックを基準にしたストリームになる
- ダミータイマー駆動よりドリフトが少ない
- VB-CABLEのクロックに追従しやすい
```

---

## Phase 5: macOS receiver の UDP 受信

目的:

```text
UDPで音声パケットを受け取り、sequenceやsample_positionを検査する。
```

処理パイプライン:

```text
UDP receiver thread
  ↓
packet parser
  ↓
sequence checker
  ↓
reorder buffer
  ↓
jitter buffer
```

### 実装手順

1. UDP socketを指定ポートでbindする。
2. 受信バッファサイズを大きめにする。
3. パケットを受け取る。
4. magic / version / header_size を検証する。
5. sample_rate / channels / format を検証する。
6. sequence numberを見て欠落・重複・逆順を検出する。
7. sample_positionをキーにしてjitter bufferへ入れる。

### 受信ログ

最低限このあたりを表示する。

```text
received_packets_per_sec
lost_packets
duplicate_packets
out_of_order_packets
network_jitter_ms
latest_sequence
latest_sample_position
```

---

## Phase 6: jitter buffer 実装

目的:

```text
UDPの到着揺れを吸収し、CoreAudioへ一定速度で音声を渡す。
```

## 基本設計

jitter bufferは sample_position を基準にしたリングバッファにする。

```text
buffer capacity:
  500ms程度から開始

target latency:
  Wi-Fi: 100ms〜200ms
  有線LAN: 20ms〜100ms
```

初期値:

```text
capacity_ms = 1000
target_latency_ms = 100
start_threshold_ms = 100
```

### バッファサイズ換算

```text
100ms = 4800 frames
500ms = 24000 frames
1000ms = 48000 frames
```

stereo int16の場合:

```text
1000ms = 48000 frames × 2ch × 2bytes = 192KB
```

メモリ量は小さいので、1秒分持っても問題ない。

---

## jitter buffer の状態

```text
NotStarted:
  まだ再生開始しない

Priming:
  target latencyぶんパケットを貯めている

Running:
  CoreAudioへ出力中

Underrun:
  バッファが枯渇した

Resync:
  大きな欠落や送信再開により再同期中
```

### 起動時

```text
1. 最初の有効パケットを受信
2. first_sample_positionを記録
3. target_latency_msぶん貯まるまでCoreAudioには無音を出す
4. 十分貯まったらRunningへ移行
```

---

## Phase 7: CoreAudio / BlackHole 出力

目的:

```text
jitter bufferから音声を取り出し、BlackHoleへ出力する。
```

実装方針:

```text
1. macOSの出力デバイス一覧を取得
2. 名前に "BlackHole" を含む出力デバイスを探す
3. 48kHz stereoで出力ストリームを開く
4. audio callback内でjitter bufferから必要frames数をpullする
5. 足りない場合は無音または簡易補間を返す
```

重要:

```text
BlackHoleに「入力する」のではなく、
BlackHoleをCoreAudioの出力デバイスとして開いて音声を書き込む。
```

その結果、別アプリからはBlackHoleが入力デバイスとして見える。

### audio callback の制約

callback内では以下を避ける。

```text
- malloc
- lock待ち
- ファイルI/O
- ログ大量出力
- ネットワークI/O
```

callbackはリアルタイム性が必要なので、事前確保済みリングバッファから読むだけにする。

---

## Phase 8: 最小音出しMVP

目的:

```text
Windowsの音声がmacOSのBlackHoleへ入り、macOS側アプリで認識できる。
```

この時点では高度なクロック補正は不要。

実装内容:

```text
sender:
  - WASAPI capture
  - 48kHz stereo int16変換
  - 5ms UDP送信

receiver:
  - UDP受信
  - 100ms固定jitter buffer
  - 欠落時は無音
  - CoreAudioでBlackHole出力
```

確認手順:

```text
1. WindowsでVB-CABLEを既定の再生デバイスにする
2. Windowsで音楽やYouTubeを再生
3. senderを起動
4. macOSでreceiverを起動
5. macOS側の録音アプリ、OBS、DAWなどでBlackHoleを入力に選択
6. 音が入ることを確認
```

---

# クロックドリフト対策

## 問題

Windows側とmacOS側はどちらも48kHzのつもりでも、実際のクロックは微妙にズレる。

例:

```text
Windows側: 47,999.5 Hz
macOS側:   48,000.8 Hz
```

この場合、長時間でreceiver側jitter bufferが徐々に増えるか、減る。

50ppmのズレがあると:

```text
86,400秒 × 50 / 1,000,000 = 4.32秒
```

24時間で数秒分のズレになる。

したがって、24時間運用には以下が必要。

```text
- バッファ水位の監視
- 送信側sample_positionの追跡
- 受信側再生位置の追跡
- 微小リサンプリング
```

---

## Phase 9: バッファ水位監視

目的:

```text
jitter bufferが増え続けているか、減り続けているかを検出する。
```

定義:

```text
write_position:
  最新受信済みsample_position

read_position:
  CoreAudioへ出力済みsample_position

buffer_level_frames:
  write_position - read_position

buffer_level_ms:
  buffer_level_frames / 48000 * 1000
```

目標:

```text
target_buffer_ms = 100
```

例:

```text
buffer_level_ms = 130
  → 受信側の消費が遅い
  → 少し速く再生する必要がある

buffer_level_ms = 70
  → 受信側の消費が速い
  → 少し遅く再生する必要がある
```

---

## Phase 10: adaptive resampling

目的:

```text
バッファ水位をtarget付近に維持するため、受信側で微小に再生速度を補正する。
```

## 基本方式

receiver側で以下のような補正比率を作る。

```text
resample_ratio = 1.0 + correction
```

例:

```text
bufferが多すぎる:
  resample_ratio = 1.00002
  少し速く消費する

bufferが少なすぎる:
  resample_ratio = 0.99998
  少し遅く消費する
```

補正範囲:

```text
通常時: ±100ppm
異常時: ±500ppm
```

ppm換算:

```text
100ppm = 0.0001
resample_ratio = 1.0001 or 0.9999
```

映画・BGM用途なら、この程度の微小補正はほぼ知覚されにくい。

---

## 制御式

初期実装はP制御でよい。

```text
error_ms = buffer_level_ms - target_buffer_ms
correction = clamp(error_ms * Kp, -max_ppm, +max_ppm)
resample_ratio = 1.0 + correction / 1_000_000
```

初期値:

```text
target_buffer_ms = 100
Kp = 5.0
max_ppm = 100
```

例:

```text
buffer_level_ms = 120
error_ms = +20
correction = +100ppm
resample_ratio = 1.0001
```

```text
buffer_level_ms = 80
error_ms = -20
correction = -100ppm
resample_ratio = 0.9999
```

改善版ではPI制御にする。

```text
integral_error += error_ms * dt
correction = Kp * error_ms + Ki * integral_error
```

ただし積分項は暴走しやすいので、最初はP制御で十分。

---

## Phase 11: リサンプラー実装

目的:

```text
jitter bufferから読み出す速度を微小に変える。
```

初期実装:

```text
linear interpolation
```

高品質版:

```text
sinc-based resampler
polyphase resampler
既存の高品質resamplingライブラリ
```

BGM・映画用途なら、まずlinear interpolationでもMVPとしては動く。

ただし音質を重視するなら、高品質リサンプラーへ差し替える。

### リサンプラーの読み出しモデル

CoreAudio callbackが N frames を要求した場合:

```text
output_frames = N
input_frames_needed = N * resample_ratio
```

内部的に fractional read position を持つ。

```text
read_pos_float += resample_ratio
```

擬似コード:

```text
for each output frame:
    i0 = floor(read_pos_float)
    i1 = i0 + 1
    frac = read_pos_float - i0

    out_l = lerp(input[i0].l, input[i1].l, frac)
    out_r = lerp(input[i0].r, input[i1].r, frac)

    read_pos_float += resample_ratio
```

sample_positionは整数だが、resampler内部では小数位置を持つ。

---

# 欠落・ジッター・再同期

## Phase 12: 欠落処理

UDPなのでパケット欠落はあり得る。

初期実装:

```text
欠落した範囲は無音で埋める
```

改善版:

```text
- 前回サンプルを短く保持
- 短い欠落なら簡易補間
- 欠落直前/直後をクロスフェード
```

欠落検出:

```text
expected_sequence = last_sequence + 1

if sequence > expected_sequence:
    lost = sequence - expected_sequence

if sequence < expected_sequence:
    duplicate or late packet
```

ただし最終的な再生順序は sequence ではなく sample_position を優先する。

---

## Phase 13: 遅延パケット処理

到着が遅すぎたパケットは捨てる。

条件:

```text
packet.sample_position + packet.frames <= current_read_position
```

この場合、すでに再生済みのため使えない。

ログ:

```text
late_packets += 1
```

---

## Phase 14: 大きな欠落時の再同期

以下の場合はResyncへ入る。

```text
- 連続で500ms以上欠落
- bufferが完全に枯渇
- sample_positionが大きくジャンプ
- stream_idが変わった
- senderが再起動した
```

Resync手順:

```text
1. CoreAudioへは無音を出す
2. jitter bufferをクリア
3. 新しいfirst_sample_positionを待つ
4. target latencyぶん貯める
5. Runningへ戻る
```

---

# sender側の詳細設計

## スレッド構成

```text
main thread:
  - config読み込み
  - device選択
  - UDP送信先設定
  - metrics表示

audio capture thread:
  - WASAPIから音声取得
  - input ring bufferへpush

packetizer thread:
  - input ring bufferから240 framesずつpop
  - header付与
  - UDP送信
```

MVPでは capture thread と packetizer thread を統合してもよい。

ただし長期運用では分けた方がよい。

---

## sender config

```toml
[network]
target_host = "192.168.11.20"
target_port = 50000
bind_addr = "0.0.0.0:0"

[audio]
device_name_contains = "CABLE Output"
sample_rate = 48000
channels = 2
packet_ms = 5

[log]
level = "info"
metrics_interval_sec = 1
```

---

## sender metrics

1秒ごとに表示:

```text
capture_frames_per_sec
sent_packets_per_sec
sent_bytes_per_sec
sequence
sample_position
capture_buffer_level_ms
rms_l_db
rms_r_db
underruns
```

表示例:

```text
sender:
  device="CABLE Output"
  format=48000/2ch/f32
  packets=200/s
  bitrate=1.63Mbps
  sample_position=123456789
  capture_buffer=8.2ms
  rms=-18.4/-17.9dB
```

---

# receiver側の詳細設計

## スレッド構成

```text
main thread:
  - config読み込み
  - BlackHole device選択
  - metrics表示

udp receive thread:
  - UDP受信
  - packet parse
  - jitter bufferへpush

audio output callback:
  - jitter bufferからpull
  - adaptive resampling
  - BlackHoleへ出力
```

重要:

```text
audio output callbackからudp socketを読まない。
audio output callbackからログを直接大量に出さない。
audio output callbackでlock待ちをしない。
```

---

## receiver config

```toml
[network]
listen_addr = "0.0.0.0:50000"
socket_recv_buffer_bytes = 1048576

[audio]
output_device_name_contains = "BlackHole"
sample_rate = 48000
channels = 2

[jitter_buffer]
capacity_ms = 1000
target_ms = 100
start_threshold_ms = 100
min_ms = 30
max_ms = 300

[clock]
adaptive_resampling = true
kp = 5.0
max_ppm = 100
emergency_max_ppm = 500

[loss]
late_packet_drop = true
missing_frame_policy = "silence"

[log]
level = "info"
metrics_interval_sec = 1
```

---

## receiver metrics

1秒ごとに表示:

```text
received_packets_per_sec
lost_packets_per_sec
late_packets_per_sec
duplicate_packets_per_sec
out_of_order_packets_per_sec
buffer_level_ms
target_buffer_ms
resample_ratio
estimated_drift_ppm
output_underruns
```

表示例:

```text
receiver:
  packets=200/s
  loss=0/s
  late=0/s
  buffer=101.7ms target=100ms
  ratio=0.999986
  drift=-14ppm
  output=BlackHole 2ch
```

---

# クロック推定

## Phase 15: sample_positionベースのドリフト推定

receiverでは以下を追跡する。

```text
sender_sample_position
receiver_arrival_time_ns
receiver_output_position
```

一定期間、たとえば10秒〜60秒で以下を計算する。

```text
sender_elapsed_frames =
  latest_sender_sample_position - first_sender_sample_position

receiver_elapsed_time_sec =
  latest_receiver_arrival_time - first_receiver_arrival_time

estimated_sender_rate =
  sender_elapsed_frames / receiver_elapsed_time_sec

drift_ppm =
  (estimated_sender_rate / 48000 - 1.0) * 1_000_000
```

ただしUDP到着時刻はジッターを含むので、短時間では信用しすぎない。

初期実装では、実際の制御はbuffer_level_msを優先する。

---

## Phase 16: buffer水位ベース制御を主にする

長時間安定性に一番効くのは、絶対時刻推定ではなくbuffer水位である。

制御の主入力:

```text
buffer_level_ms - target_buffer_ms
```

補助情報:

```text
estimated_drift_ppm
network_jitter_ms
loss_rate
```

考え方:

```text
bufferが増えている → 消費が遅い → resample_ratioを上げる
bufferが減っている → 消費が速い → resample_ratioを下げる
```

---

# CLI設計

## sender

```bash
sender \
  --target 192.168.11.20:50000 \
  --device "CABLE Output" \
  --sample-rate 48000 \
  --channels 2 \
  --packet-ms 5
```

補助コマンド:

```bash
sender --list-devices
sender --dry-run
sender --log-level debug
```

## receiver

```bash
receiver \
  --listen 0.0.0.0:50000 \
  --output-device "BlackHole" \
  --target-buffer-ms 100
```

補助コマンド:

```bash
receiver --list-devices
receiver --log-level debug
receiver --target-buffer-ms 50
receiver --target-buffer-ms 200
```

---

# 動作確認シナリオ

## Scenario 1: UDP疎通

```text
senderからダミーパケット送信
receiverでsequence連番を確認
```

成功条件:

```text
lossがほぼ0
packets/secが約200
```

5ms packetなら:

```text
1000ms / 5ms = 200 packets/sec
```

---

## Scenario 2: Windows音声キャプチャ

```text
WindowsでYouTube再生
senderでRMSメーターが動く
```

成功条件:

```text
CABLE Outputから音声が取れる
左右チャンネルが取得できる
```

---

## Scenario 3: macOS BlackHole出力

```text
receiverでテストトーンをBlackHoleへ出力
macOS側録音アプリで入力確認
```

成功条件:

```text
macOS側でBlackHole入力に信号が来る
```

---

## Scenario 4: end-to-end MVP

```text
Windows YouTube
  → VB-CABLE
  → sender
  → UDP
  → receiver
  → BlackHole
  → macOS側アプリ
```

成功条件:

```text
音が途切れず聞こえる
大きなノイズがない
左右が正しい
```

---

## Scenario 5: 1時間連続再生

確認項目:

```text
buffer_level_msがtarget付近に留まる
resample_ratioが±100ppm程度で収まる
underrunしない
メモリ使用量が増え続けない
```

---

## Scenario 6: 24時間連続再生

確認項目:

```text
receiverが落ちない
bufferが増え続けない
bufferが枯渇し続けない
音切れ回数が許容範囲
ログファイルが肥大化しすぎない
```

---

# エラー処理

## sender側

### VB-CABLEが見つからない

```text
- device一覧を表示
- --device指定を促す
- exit code 1
```

### フォーマットが合わない

```text
- 入力フォーマットをログに出す
- 変換可能なら変換
- 変換不能なら終了
```

### UDP送信失敗

```text
- エラーをカウント
- 一時的エラーなら継続
- 致命的エラーなら終了
```

---

## receiver側

### BlackHoleが見つからない

```text
- 出力デバイス一覧を表示
- --output-device指定を促す
- exit code 1
```

### パケットが来ない

```text
- CoreAudioには無音を出し続ける
- "waiting for stream" を表示
```

### stream_idが変わった

```text
- sender再起動とみなす
- jitter bufferをクリア
- Resyncへ移行
```

### 大量欠落

```text
- 無音補填
- 欠落統計を出す
- 500ms以上続くならResync
```

---

# ログ設計

## 通常ログ

```text
INFO:
  起動、デバイス選択、フォーマット、接続先、統計

WARN:
  欠落増加、underrun、buffer過多、buffer不足、resync

ERROR:
  device open失敗、socket bind失敗、audio callback失敗
```

## メトリクスログ

CSVまたはJSON Linesで保存できるようにする。

例:

```json
{"ts": 1234567890, "buffer_ms": 101.2, "loss": 0, "ratio": 0.999991}
```

24時間テストで後から解析できるようにする。

---

# 実装順序詳細

## Step 1: common crate/module

作るもの:

```text
AudioPacketHeader
AudioPacket parse/serialize
sample format constants
endian conversion
sequence helper
```

テスト:

```text
- header serialize/deserialize
- invalid magic rejection
- invalid version rejection
- payload size validation
```

---

## Step 2: dummy sender / receiver

作るもの:

```text
dummy sender:
  5msごとに無音PCMパケットを送る

dummy receiver:
  受け取ってsequenceを表示
```

テスト:

```text
- 200 packets/secになる
- sequence欠落を検出できる
```

---

## Step 3: receiver test tone output

作るもの:

```text
receiver側だけで440Hz sine waveを生成
BlackHoleへ出力
```

目的:

```text
CoreAudio / BlackHole出力をネットワーク抜きで確認する。
```

---

## Step 4: Windows WASAPI capture standalone

作るもの:

```text
sender側でVB-CABLE Outputをキャプチャ
RMSメーター表示
```

まだUDP送信しない。

---

## Step 5: WASAPI → UDP

作るもの:

```text
キャプチャした音声をUDPパケット化して送る
```

receiverはまだファイル保存でもよい。

確認:

```text
macOS側で受信したPCMをraw/wavに保存
再生して音が正しいか確認
```

---

## Step 6: UDP → jitter buffer → BlackHole

作るもの:

```text
receiverで受信した音声をjitter buffer経由でBlackHoleへ出力
```

ここでend-to-end MVP完成。

---

## Step 7: 欠落検出・無音補填

作るもの:

```text
sequence欠落検出
sample_position欠落検出
missing framesへの無音補填
late packet drop
```

---

## Step 8: buffer metrics

作るもの:

```text
buffer_level_ms
target_buffer_ms
underrun_count
overrun_count
late_packet_count
```

---

## Step 9: adaptive resampling

作るもの:

```text
fractional read position
linear interpolation resampler
P制御によるresample_ratio補正
```

確認:

```text
1時間再生してbuffer_level_msが発散しない
```

---

## Step 10: 長時間運用対策

作るもの:

```text
ログローテーション
自動再同期
sender再起動検出
receiver待機復帰
設定ファイル
```

---

# 実装上の注意

## UDP payloadはMTU未満にする

5msパケットなら安全。

10msの場合:

```text
480 frames × 2ch × 2bytes = 1920 bytes
```

これはEthernet MTU 1500を超えやすく、IPフラグメントが起きる可能性がある。

初期実装では5ms固定が安全。

---

## CoreAudio callbackでロックしない

audio callbackはリアルタイム処理なので、ロック待ちで詰まると音切れする。

推奨:

```text
UDP受信スレッド
  → lock-free ring buffer
  → audio callback
```

最低限:

```text
短時間try_lock
失敗したら無音
```

---

## senderの音声クロックを基準にする

送信パケットの間隔はOSタイマーではなく、WASAPIで実際に取れたサンプル数を基準にする。

悪い例:

```text
5ms sleep
240 frames送る
```

良い例:

```text
WASAPIで取れたframesを蓄積
240 framesたまったら送る
```

---

## 受信側は到着時刻を信用しすぎない

Wi-Fiでは到着時刻が揺れる。

制御の主軸は:

```text
jitter bufferの水位
```

到着時刻は補助情報として使う。

---

# チューニング値

## Wi-Fi向け初期値

```text
packet_ms = 5
jitter_capacity_ms = 1000
target_buffer_ms = 150
start_threshold_ms = 150
max_ppm = 100
emergency_max_ppm = 500
```

## 有線LAN向け初期値

```text
packet_ms = 5
jitter_capacity_ms = 500
target_buffer_ms = 50
start_threshold_ms = 50
max_ppm = 100
emergency_max_ppm = 500
```

## 映画鑑賞・BGM向け

```text
target_buffer_ms = 100〜200
```

## 低遅延実験向け

```text
target_buffer_ms = 20〜50
```

ただしWi-Fiでは音切れしやすくなる。

---

# 将来拡張

## RTP化

独自UDPが安定した後、RTP風またはRTPそのものへ寄せる。

追加するもの:

```text
RTP timestamp
SSRC
payload type
RTCP風sender report
```

---

## 制御用UDP/TCPチャンネル

音声とは別に制御チャンネルを作る。

用途:

```text
- receiverのbuffer状態をsenderへ返す
- receiverのloss率をsenderへ返す
- sender側でpacket_msや送信フォーマットを調整
- ping/pongで疎通確認
```

---

## GUI

最終的には以下を表示するGUIがあると便利。

```text
sender:
  device selection
  level meter
  target IP
  send status

receiver:
  output device selection
  buffer level graph
  loss counter
  resample ratio
  start/stop
```

---

## 圧縮コーデック対応

LAN内ではPCMで十分だが、将来的にはOpus対応もあり得る。

ただし初期実装では不要。

理由:

```text
- PCMでも1.5Mbps程度で軽い
- コーデック遅延が増える
- クロック・ジッター制御の本質から外れる
```

---

# 完了条件

## MVP完了条件

```text
- WindowsのVB-CABLE Outputから音声を取得できる
- macOSへUDPで送れる
- macOSのBlackHoleへ音声を出力できる
- 10分程度、明確な音切れなく動作する
```

## 安定版完了条件

```text
- 1時間連続再生でbuffer_level_msが発散しない
- 欠落時にクラッシュしない
- sender再起動後にreceiverが再同期できる
- receiver起動後にsenderを開始しても動く
- sender停止時にreceiverが無音待機できる
```

## 24時間運用版完了条件

```text
- 24時間連続再生でプロセスが落ちない
- メモリ使用量が増え続けない
- buffer_level_msがtarget付近に維持される
- resample_ratioが異常値に張り付かない
- underrun / resync回数がログで確認できる
```

---

# 最初に作るべき最小コード

最初の実装順はこれでよい。

```text
1. common: AudioPacketHeader
2. dummy sender: 無音PCMをUDP送信
3. dummy receiver: UDP受信して統計表示
4. receiver: BlackHoleへsine wave出力
5. sender: VB-CABLE OutputをWASAPIキャプチャ
6. sender: キャプチャ音声をUDP送信
7. receiver: UDP音声を固定100ms bufferでBlackHole出力
8. receiver: sequence欠落検出
9. receiver: buffer水位表示
10. receiver: adaptive resampling
```

この順で進めると、ネットワーク、macOS音声出力、Windows音声入力、同期制御を分離して検証できる。

---

# 設計判断まとめ

```text
- 5ms packetを基本にする
- UDP payloadをMTU未満に保つ
- sample_positionを音声時間軸の主キーにする
- sequenceは欠落検出用に使う
- send_time_nsは補助的な観測値として使う
- receiver側でjitter bufferを持つ
- 24時間運用にはadaptive resamplingを入れる
- PTP完全実装は不要
- BlackHoleはCoreAudio出力デバイスとして扱う
```

最初からDanteのような高精度同期を再現する必要はない。

今回の用途では、

```text
UDP + sequence + sample_position + jitter buffer + adaptive resampling
```

まで入れれば、かなり実用的なLAN音声転送になる。


# macOSファースト実装方針

## 基本方針

このプロジェクトでは、最初から Windows 実機と macOS 実機を常時接続して開発しない。

まず macOS 1台で、以下の大部分を完成させる。

```text id="7ymos3"
- UDP packet format
- sequence number管理
- sample_position管理
- packet parser / serializer
- UDP sender / receiver
- jitter buffer
- 欠落検出
- 遅延パケット破棄
- buffer水位監視
- adaptive resampling
- BlackHole / CoreAudio 出力
- metrics / logging
- 受信音声のWAV保存
- 長時間再生テスト
```

Windows 実機が必要になるのは、主に以下の部分だけとする。

```text id="cvkwcy"
- WASAPI capture
- VB-CABLE Output デバイス検出
- Windows側音声フォーマット確認
- Windows側ビルド
- 実機での Windows → macOS end-to-end 確認
```

つまり、Windows側は最初から本体ロジックを持たせず、最後に `AudioInput` adapter として差し込む。

---

# 開発構成

## 推奨ディレクトリ構成

```text id="c0dw6s"
audio-lan-bridge/
  common/
    packet.*
    audio_format.*
    sample_convert.*
    ring_buffer.*
    jitter_buffer.*
    resampler.*
    metrics.*
    wav_io.*

  sender/
    main.*
    packetizer.*
    audio_input/
      sine.*
      wav_file.*
      null.*
      wasapi.*        // Windows専用。後半で実装

  receiver/
    main.*
    depacketizer.*
    audio_output/
      coreaudio.*     // macOS専用
      wav_file.*
      null.*

  tools/
    packet_dump.*
    jitter_sim.*
    drift_sim.*
    loss_sim.*

  tests/
    packet_tests.*
    jitter_buffer_tests.*
    resampler_tests.*
    golden_wav_tests.*

  PLAN.md
  README.md
```

## OS依存部分を薄くする

音声I/Oは interface / trait で隠す。

```text id="vg6fmz"
AudioInput:
  - sine wave
  - wav file
  - null
  - WASAPI capture

AudioOutput:
  - CoreAudio / BlackHole
  - wav file
  - null
```

この構成にすると、macOS上では以下の組み合わせで開発できる。

```text id="7y4lyg"
sine sender
  → UDP
  → receiver
  → BlackHole

wav sender
  → UDP
  → receiver
  → BlackHole

wav sender
  → UDP
  → receiver
  → output.wav

dummy sender
  → UDP
  → receiver
  → metrics only
```

Windows側は後から以下に差し替える。

```text id="e3jla4"
WASAPI VB-CABLE Output
  → packetizer
  → UDP
```

---

# macOSだけで作るMVP

## macOS MVP 1: dummy UDP sender / receiver

目的:

```text id="8w06sn"
音声なしでUDP疎通とパケット形式を固める。
```

実装:

```text id="czv7rd"
sender:
  - 5msごとに無音PCMパケットを送る
  - sequence numberを増やす
  - sample_positionを240 framesずつ増やす

receiver:
  - UDPで受信
  - header検証
  - sequence確認
  - sample_position確認
  - metrics表示
```

成功条件:

```text id="g7d7j1"
- 200 packets/sec で受信できる
- sequence gapを検出できる
- sample_positionが正しく進む
```

---

## macOS MVP 2: sine sender → UDP → receiver

目的:

```text id="lwcs8o"
440HzなどのテストトーンをUDPで流し、PCM経路を確認する。
```

構成:

```text id="xln0pf"
sine generator
  → packetizer
  → UDP localhost
  → receiver
  → output.wav
```

成功条件:

```text id="v05oeb"
- output.wav を再生すると正しいテストトーンが鳴る
- ノイズ、左右反転、音量異常がない
```

この時点ではBlackHoleはまだ使わなくてよい。

---

## macOS MVP 3: receiver → BlackHole 出力

目的:

```text id="4iyguc"
CoreAudioでBlackHoleへ音声を書き込めることを確認する。
```

構成:

```text id="6z1b2t"
receiver internal sine generator
  → CoreAudio output
  → BlackHole
  → macOS側録音アプリ / OBS / DAW
```

成功条件:

```text id="fxrw6p"
- macOS側アプリでBlackHole入力に信号が来る
- レベルメーターが動く
- 音声が途切れない
```

重要:

```text id="xh64t9"
BlackHoleは「入力デバイスへ書く」のではなく、
CoreAudioの出力デバイスとして開いて音声を書き込む。
```

---

## macOS MVP 4: wav sender → UDP → receiver → BlackHole

目的:

```text id="30k2cu"
Windows抜きで、実際の音源をLAN転送相当の経路でBlackHoleに流す。
```

構成:

```text id="qjg76m"
test.wav
  → wav sender
  → packetizer
  → UDP localhost
  → receiver
  → jitter buffer
  → CoreAudio
  → BlackHole
```

成功条件:

```text id="cuigq7"
- wavの音がBlackHoleへ入る
- macOS側アプリで録音できる
- 10分程度連続で途切れない
```

この段階で、Windows側が未実装でも receiver の大部分は完成している。

---

# macOSで作り込む安定化機能

## packet loss simulation

macOS senderに、意図的なパケット欠落機能を入れる。

```bash id="yrnflp"
sender --input-file test.wav --target 127.0.0.1:50000 --drop-rate 0.01
```

確認すること:

```text id="r7wxwa"
- sequence欠落を検出できる
- 欠落箇所を無音で埋められる
- receiverがクラッシュしない
- 欠落統計が出る
```

---

## jitter simulation

macOS senderに、意図的な送信揺れを入れる。

```bash id="fjq45l"
sender --input-file test.wav --target 127.0.0.1:50000 --jitter-ms 20
```

確認すること:

```text id="yqp7dc"
- jitter bufferで吸収できる
- late packetを検出できる
- target_buffer_msを増やすと安定する
```

---

## reorder simulation

macOS senderに、意図的な順序入れ替えを入れる。

```bash id="3zs4bk"
sender --input-file test.wav --target 127.0.0.1:50000 --reorder-rate 0.01
```

確認すること:

```text id="v0gq0c"
- sample_position基準で正しく並べ直せる
- 遅すぎたパケットは破棄できる
- 破棄数がmetricsに出る
```

---

## clock drift simulation

macOS senderに、仮想的なクロックドリフトを入れる。

例:

```bash id="m4ptgv"
sender --input-file test.wav --target 127.0.0.1:50000 --drift-ppm 50
```

意味:

```text id="qce7qd"
sender側がreceiverより50ppm速い/遅い状態を擬似的に作る。
```

確認すること:

```text id="26lk0r"
- receiverのbuffer_level_msが増え続けない
- adaptive resamplingが反応する
- resample_ratioが補正方向に動く
- 1時間再生してbufferが発散しない
```

---

# macOSでの長時間テスト

## 1時間テスト

構成:

```text id="1wbwri"
wav sender
  → UDP localhost
  → receiver
  → BlackHole
```

確認項目:

```text id="4i9u70"
- buffer_level_ms が target 付近に留まる
- output_underruns が増え続けない
- メモリ使用量が増え続けない
- resample_ratio が異常値に張り付かない
```

---

## 24時間テスト

Windows連携前に、macOS単体で24時間テストを行う。

構成:

```text id="3bqnfa"
looping wav sender
  → UDP localhost
  → receiver
  → null output または BlackHole
```

確認項目:

```text id="qy9myf"
- receiverが落ちない
- senderが落ちない
- メモリリークがない
- buffer水位が発散しない
- ログファイルが肥大化しすぎない
- resyncが意図せず頻発しない
```

このテストに通ってから Windows WASAPI adapter を足す。

---

# Windows側実装を後半に回す理由

## 理由

Windows側の本質は、以下だけである。

```text id="4lmct2"
VB-CABLE Outputから音声framesを取得し、
既存のpacketizerへ渡す。
```

そのため、以下はWindows実機なしで先に完成できる。

```text id="f6h2it"
- packet format
- UDP送信
- UDP受信
- jitter buffer
- resampler
- CoreAudio output
- BlackHole integration
- metrics
- 長時間安定性
```

Windows側を早く作りすぎると、問題が発生したときに原因が分かれやすい。

```text id="q2vhed"
- WASAPIの問題なのか
- VB-CABLEの問題なのか
- UDPの問題なのか
- jitter bufferの問題なのか
- CoreAudioの問題なのか
- BlackHoleの問題なのか
```

macOS単体でreceiver側を固めておけば、Windows実機テスト時には問題範囲をかなり限定できる。

---

# Windows実機でやること

## Windows Step 1: device list

まず、Windows側で音声入力デバイス一覧を表示する。

```bash id="twg0h2"
sender --list-devices
```

確認対象:

```text id="qnn8vm"
- CABLE Output が見える
- sample rate
- channel count
- sample format
```

---

## Windows Step 2: WASAPI capture standalone

まだUDP送信しない。

```bash id="4lixgh"
sender --device "CABLE Output" --meter-only
```

確認対象:

```text id="45zm0l"
- Windowsで再生した音に応じてRMSメーターが動く
- 無音時はレベルが落ちる
- 左右チャンネルが取れる
```

---

## Windows Step 3: WASAPI → WAV保存

```bash id="pqmg9m"
sender --device "CABLE Output" --output-file capture.wav
```

確認対象:

```text id="cdszf8"
- capture.wav を再生して正しい音が入っている
- 音割れしていない
- サンプルレートが48kHzになっている
```

---

## Windows Step 4: WASAPI → UDP → macOS receiver

ここで初めてend-to-end接続を行う。

```bash id="hgp0q8"
# Windows
sender --device "CABLE Output" --target 192.168.11.20:50000

# macOS
receiver --listen 0.0.0.0:50000 --output-device "BlackHole"
```

確認対象:

```text id="f89t9m"
- macOS側で音が入る
- receiverのbufferがtarget付近に留まる
- packet lossがほぼない
- Windows側のsample_positionが安定して増える
```

---

# 実装順序の最終版

実装順序は以下とする。

```text id="13fuag"
1. common: AudioPacketHeader
2. common: packet serialize / deserialize
3. macOS: dummy sender
4. macOS: dummy receiver
5. macOS: receiver metrics
6. macOS: sine sender
7. macOS: receiver output.wav 保存
8. macOS: CoreAudio / BlackHole 出力
9. macOS: wav sender
10. macOS: wav sender → UDP → receiver → BlackHole
11. common: jitter buffer
12. common: sequence欠落検出
13. common: late packet drop
14. common: buffer水位監視
15. common: linear resampler
16. common: adaptive resampling
17. macOS: packet loss / jitter / reorder / drift simulation
18. macOS: 1時間テスト
19. macOS: 24時間テスト
20. Windows: WASAPI device list
21. Windows: VB-CABLE Output capture
22. Windows: WASAPI → WAV保存
23. Windows: WASAPI → UDP sender
24. Windows → macOS end-to-end test
25. Windows → macOS 1時間テスト
26. Windows → macOS 24時間テスト
```

---

# CLI設計の修正

## macOS sender

```bash id="a4fipo"
sender \
  --input-file test.wav \
  --target 127.0.0.1:50000 \
  --packet-ms 5
```

```bash id="0q0mhp"
sender \
  --input sine \
  --freq 440 \
  --target 127.0.0.1:50000
```

simulation options:

```bash id="xpnfb9"
sender \
  --input-file test.wav \
  --target 127.0.0.1:50000 \
  --drop-rate 0.01 \
  --jitter-ms 20 \
  --reorder-rate 0.01 \
  --drift-ppm 50
```

## macOS receiver

```bash id="3s4rl5"
receiver \
  --listen 0.0.0.0:50000 \
  --output-device "BlackHole" \
  --target-buffer-ms 100
```

WAV保存:

```bash id="j0o552"
receiver \
  --listen 0.0.0.0:50000 \
  --output-file received.wav
```

null output:

```bash id="z96s3w"
receiver \
  --listen 0.0.0.0:50000 \
  --output null
```

## Windows sender

```bash id="85vcg7"
sender.exe \
  --device "CABLE Output" \
  --target 192.168.11.20:50000
```

device確認:

```bash id="4w4csx"
sender.exe --list-devices
```

meter確認:

```bash id="3p8nh8"
sender.exe --device "CABLE Output" --meter-only
```

WAV保存:

```bash id="f3suc7"
sender.exe --device "CABLE Output" --output-file capture.wav
```

---

# 完了条件の修正

## macOS単体MVP完了条件

```text id="tgzvut"
- wav sender → UDP → receiver → output.wav が動く
- wav sender → UDP → receiver → BlackHole が動く
- sequence欠落を検出できる
- jitter bufferが機能している
- 10分程度、音切れなく動作する
```

## macOS安定版完了条件

```text id="tvqkdc"
- packet loss simulationでクラッシュしない
- jitter simulationでbufferが機能する
- drift simulationでadaptive resamplingが効く
- 1時間再生でbuffer_level_msが発散しない
- 24時間再生でプロセスが落ちない
```

## Windows統合版完了条件

```text id="ppx5el"
- VB-CABLE OutputをWASAPIでcaptureできる
- capture.wavに正しく録音できる
- Windows senderからmacOS receiverへUDP送信できる
- macOS BlackHoleに音声が入る
- 1時間再生でbufferが発散しない
```

## 24時間運用版完了条件

```text id="k7ndbc"
- Windows → macOSで24時間連続稼働する
- receiverのbuffer_level_msがtarget付近に維持される
- resample_ratioが異常値に張り付かない
- underrun / resync / loss がmetricsで追跡できる
- sender停止・再開後にreceiverが再同期できる
```

---

# 設計判断の更新

```text id="3lul7g"
- macOSだけでギリギリまで作り込む
- Windows側はWASAPI adapterとして後半に追加する
- sender入力は最初から差し替え可能にする
- receiver出力はBlackHole / WAV / nullを切り替え可能にする
- packet loss / jitter / reorder / driftをmacOS senderで擬似的に発生させる
- Windows実機テスト前にreceiverの安定性を十分に確認する
- Windows統合時に問題範囲をWASAPI/VB-CABLE周辺へ限定できるようにする
```

この方針により、実装難度の高い同期制御とjitter bufferを、Windows実機依存なしで先に固められる。
