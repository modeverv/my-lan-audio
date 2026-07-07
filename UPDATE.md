# UPDATE: localhost underrun / latency stability review

## Context

2026-07-07 の macOS 1台テストでは、以下のような状態が観測された。

```text
receiver:
  packets ~= 200/s
  loss=0/s
  late=0/s
  duplicate=0/s
  out_of_order=0/s
  buffer ~= 190-220ms
  target=100ms
  underruns=1
  resyncs=1

sender:
  packets ~= 200/s
  bitrate ~= 1.6Mbps
  dropped=0
  errors=0
```

このログから見る限り、localhost UDP の packet loss が主原因ではない。sender から receiver へは 5ms packet がほぼ期待通り届いている。

ただし、localhost であっても `underruns=1` や `resyncs=1` が出ることは、長時間安定性の観点では気になる。特に現行実装は MVP としては動くが、リアルタイム音声処理としては構造上まだ弱い。

## Important Interpretation

`underruns=1 resyncs=1` が増え続けていないなら、実行中に継続的に壊れているというより、起動時または stream 切替直後の一回だけを数えている可能性がある。

一方で、現行ログだけでは以下を区別できない。

```text
- 起動時の priming 中に発生した underrun
- callback が mutex を取れず無音を返した drop
- jitter buffer が本当に枯渇した underrun
- stream_id 変更や resync による一時的な無音
- CoreAudio output callback の遅れ
- capture 側 callback / mpsc queue の遅れ
```

したがって、まず metrics を分解しないと、`underruns` の数字だけでは安定性の評価に使いにくい。

## Current Structural Weak Points

### 1. Audio callback が `Mutex<JitterBuffer>` に依存している

現状 receiver の CoreAudio callback は `try_lock()` で jitter buffer を読む。

```text
audio callback
  -> try_lock(Mutex<JitterBuffer>)
  -> pull_f32 / pull_i16
  -> failed lockなら無音
```

これは MVP としては許容できるが、安定版では弱い。UDP receive thread や metrics thread が lock を持つ瞬間に audio callback が負けると、packet loss がなくても無音が出る。

さらに、現状この lock miss は `output_underruns` として正確に数えていない。つまり、無音が出ても metrics に出ない可能性がある。

### 2. Audio callback 内で `BTreeMap` を sample ごとに探索している

現状 `JitterBuffer::sample_at()` は `BTreeMap::range(..=position).next_back()` を使う。callback 内で output frame ごとにこの検索が走る。

これは分かりやすい実装だが、リアルタイム callback には重い。localhost で packet が完璧でも、callback 処理が詰まると underrun / glitch になる。

### 3. Callback 内の allocation path が残っている

`U16` output や `I16` output では callback 内で一時 `Vec<f32>` を作る経路がある。現在の MacBook / BlackHole / SOUNDPEATS は主に `F32` なので踏みにくいが、安定版では callback 内 allocation は避けるべき。

### 4. Startup / steady-state の metrics が混ざっている

起動直後の priming、sender 開始前、stream 再同期、通常運転中の underrun が同じ counter に混ざっている。

ユーザーが見たいのは主に通常運転中の安定性なので、少なくとも以下を分けるべき。

```text
startup_underruns
steady_underruns
callback_lock_misses
missing_sample_frames
resyncs_by_stream_change
resyncs_by_underrun
latency_trims
```

### 5. Current feedback is receiver-local only

現状 sender は receiver の buffer 水位を知らない。receiver だけが buffer 水位を見て ASRC 的に読み出し速度を調整している。

live capture の場合、sender の capture clock と receiver の output clock は別物である。localhost でも以下の clock domain は一致しない。

```text
BlackHole / input device capture clock
sender process scheduling
UDP receive timing
MacBook speaker / Bluetooth / output device clock
CoreAudio callback scheduling
```

そのため、localhost でも clock drift / scheduling jitter は起きる。ネットワーク遅延が小さいことと、音声 clock が一致することは別問題。

## Clock / Control Design Options

### Option A: Receiver-only ASRC refinement

現行方針の延長。receiver が output device clock を master とし、buffer 水位から読み出し比率を制御する。

必要な改善:

```text
- device sample-rate ratio と drift correction ratio を分けて metrics 表示する
- P制御だけでなく低速PI制御を入れる
- buffer level を low-pass filter して jitter に過反応しない
- max_ppm / emergency_max_ppm を現実的に調整する
- max_buffer_ms 超過時は古い音声を明示的に trim する
```

利点:

```text
- 単純
- senderを変えなくてよい
- UDP one-wayのまま動く
```

欠点:

```text
- senderはreceiver状態を知らない
- capture clock差が大きいとreceiver側だけで苦しくなる
- latencyが増えた理由をsender側から観測できない
```

### Option B: Receiver -> Sender status feedback

音声UDPとは別に、小さい control packet を receiver から sender へ返す。

例:

```text
ReceiverStatus {
  stream_id
  latest_received_sequence
  latest_received_sample_position
  receiver_read_position
  buffer_level_frames
  buffer_level_ms
  target_ms
  output_sample_rate
  output_callback_frames_total
  underruns
  callback_lock_misses
  latency_trims
  receiver_time_ns
}
```

sender はこれを受けて metrics に出す。

利点:

```text
- receiver bufferが増えている/減っていることをsender側でも見える
- end-to-end状態のログが取りやすい
- 将来 sender-side resampling / pacing の材料になる
```

欠点:

```text
- live capture clock自体は直接制御できない
- control packetが来なくても動くfallbackが必要
```

### Option C: Sender-side ASRC / pacing

receiver status を使い、sender が network stream に載せる sample rate を微調整する。capture device から来た音声を sender 内部で resample し、receiver の buffer level が target に近づくように packetize する。

考え方:

```text
capture clock audio
  -> sender ASRC
  -> network nominal 48k stream
  -> receiver jitter buffer
  -> output clock ASRC
```

または receiver を master として:

```text
receiver buffer high -> senderが送るサンプルをわずかに減らす
receiver buffer low  -> senderが送るサンプルをわずかに増やす
```

利点:

```text
- receiverだけに補正を押し付けない
- buffer水位の長期安定性が上がる可能性がある
```

欠点:

```text
- live captureのサンプルを捨てる/補間することになる
- sender/receiver双方の制御が絡み、デバッグが難しくなる
- まずreceiver単体のRT安全性を直してからでよい
```

### Option D: Pull model / receiver clock master

receiver が必要な音声量を sender に要求する pull 型に近づける。

これは LAN audio bridge としてはかなり大きな設計変更になる。低遅延・安定運用には理屈上良いが、MVPからは遠い。

初期段階では採用しない。

## Recommended Direction

今すぐやるべき順序は以下。

### Step 1: Metricsを分解する

まず本当に通常運転中の underrun なのかを見えるようにする。

追加したいログ:

```text
startup_underruns
steady_underruns
callback_lock_misses
missing_frame_calls
missing_frames
resync_reason
output_callback_hz
output_callback_frames
output_sample_rate
device_ratio
drift_correction_ppm
effective_ratio
audio_latency_ms = latest_received_end - read_pos
```

現在の `buffer=...ms` は実質的な receiver 内部 latency なので、名前を `audio_latency_ms` に寄せた方がよい。

### Step 2: Audio callbackからMutex/BTreeMapを外す

安定版の最優先。

推奨構造:

```text
UDP receive thread
  -> packet parse
  -> reorder / late drop
  -> preallocated jitter ring write
  -> atomics metrics update

audio callback
  -> lock-free or wait-free ring read
  -> fractional read position
  -> linear interpolation
  -> no malloc
  -> no log
  -> no mutex wait
```

まず完全lock-freeでなくてもよい。最低ラインは以下。

```text
- callback内でBTreeMapを触らない
- callback内でVecを作らない
- callback内でMutexを待たない
- callback lock missを正確にcounterへ出す
```

実装案:

```text
JitterRing {
  base_sample_position: AtomicU64
  write_end_position: AtomicU64
  read_position: AtomicU64 / f64はcallback専有
  frames: Vec<AtomicI16 pair> または Mutex外で事前確保されたVec<StereoFrame>
  valid_generation: Vec<AtomicU32>
}
```

より簡単な移行案:

```text
UDP thread:
  BTreeMapでreorder
  連続化できたPCMをSPSC ringへpush

audio callback:
  SPSC ringからpop
  足りなければ無音
```

この簡易案は sample_position ベースの遅延packet復帰には弱いが、callbackのRT安全性はかなり上がる。

### Step 3: Startupとsteady-stateを分ける

`underruns=1 resyncs=1` が起動時だけなら、通常運転の評価から除外できるようにする。

状態遷移を明示する。

```text
WaitingForStream
Priming
Running
Recovering
Stopped
```

`Running` に入る前の無音出力は underrun と数えない。

### Step 4: Receiver-only ASRCを整理する

今の `ratio` は以下が混ざっている。

```text
device sample-rate conversion: 48000 -> 44100
buffer drift correction: ppm補正
```

ログでは分ける。

```text
device_ratio=1.000000 or 1.088435
correction_ppm=+500
effective_ratio=1.000500 or 1.088980
```

MacBook speaker 48k 出力なら `device_ratio=1.000000` になるはず。

### Step 5: Receiver -> Sender status feedback

構造改善の第二段階として control UDP を足す。

初期は制御せず、観測だけでよい。

```text
receiver --feedback-target 127.0.0.1:50001
sender   --feedback-listen 127.0.0.1:50001
```

sender 側で以下を表示する。

```text
remote_buffer_ms
remote_underruns
remote_trims
remote_drift_ppm
remote_output_rate
```

これにより、sender log と receiver log を人間が並べて読まなくてもよくなる。

## Practical Tuning For Current Code

現行構造のまま試すなら:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 127.0.0.1:50000 \
  --output audio \
  --output-device "MacBook Proのスピーカー" \
  --target-buffer-ms 120 \
  --max-buffer-ms 240 \
  --start-threshold-ms 120 \
  --max-ppm 1000 \
  --emergency-max-ppm 5000
```

低遅延を試すなら:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 127.0.0.1:50000 \
  --output audio \
  --output-device "MacBook Proのスピーカー" \
  --target-buffer-ms 60 \
  --max-buffer-ms 140 \
  --start-threshold-ms 80 \
  --max-ppm 1000 \
  --emergency-max-ppm 5000
```

ただし、これは根本解決ではない。根本的には callback 経路をRT安全にする必要がある。

## Notes On "Clock Exchange"

clock exchange は有効。ただし、sender の `send_time_ns` と receiver の `Instant` は別clockなので、そのまま差し引いて絶対latencyにはできない。

実装候補:

```text
1. receiver status packet:
   receiverが read_position / latest_received_position をsenderへ返す

2. ping-pong clock sample:
   sender_time_1
   receiver_time
   sender_time_2
   からNTP風にoffset / RTTを推定

3. sample_position based latency:
   receiver側で latest_received_end - read_pos をaudio latencyとして扱う
```

当面は 3 が最も信頼しやすい。これは同じ sample timeline 上の差分なので、wall clock の同期を必要としない。

## Proposed Implementation Order

```text
P0:
  - callback_lock_misses をmetrics化
  - startup / steady-state underrunを分離
  - logに device_ratio / correction_ppm / effective_ratio を出す
  - logに audio_latency_ms 名を追加

P1:
  - audio callbackからBTreeMap探索を外す
  - callback内allocationをゼロにする
  - preallocated ring bufferへ移行

P2:
  - receiver -> sender status UDPを追加
  - sender側でremote receiver metricsを表示

P3:
  - PI制御またはより安定したASRC制御
  - sender-side ASRCを検討
  - 短い欠落のclick低減、crossfade/hold-last-sample
```

## Conclusion

localhostで packet loss がないのに underrun / resync が見える場合、主原因はネットワークではなく receiver のリアルタイム処理構造か、startup metrics の混在である可能性が高い。

次に実装するなら、いきなり clock exchange で制御を複雑にするより、まず receiver の audio callback をRT安全にし、metricsを分解するのが最も効果が高い。その上で receiver status feedback を足すと、clock drift と latency の制御に進める。
