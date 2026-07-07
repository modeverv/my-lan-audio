# my-lan-audio

Windows の VB-CABLE 出力音声を LAN/UDP で macOS に送り、macOS 側で BlackHole などの CoreAudio 出力デバイスへ流し込むための音声ブリッジ実装です。

現時点では `PLAN.md` の macOS-first 方針に沿って、Windows 実機なしでも検証できる部分を先に実装しています。UDP パケット形式、sender/receiver、ジッターバッファ、欠落検出、WAV 保存、CoreAudio 出力、シミュレーション機能、cpal 経由の Windows WASAPI capture adapter が入っています。

## できること

- 48 kHz / stereo / signed 16-bit little-endian PCM を UDP で送受信する
- 5 ms 単位の音声パケットを送る
- パケットに `stream_id`、`sequence`、`sample_position`、`send_time_ns` を含める
- sender から dummy / sine / WAV / live capture 入力を送る
- sender 側で packet loss / jitter / reorder / drift を擬似的に発生させる
- receiver で sequence 欠落、重複、遅延、順序入れ替わりを metrics 表示する
- receiver で sample position ベースの jitter buffer を使う
- 欠落部分を無音で埋める
- buffer 水位に応じて adaptive linear resampling する
- receiver から null / WAV / CoreAudio output に出力する
- macOS の BlackHole を CoreAudio output device として開く
- receiver 単体で test tone を出して BlackHole 出力確認をする
- Windows では cpal/WASAPI 経由で `CABLE Output` を capture する

## ディレクトリ構成

```text
.
├── Cargo.toml
├── Cargo.lock
├── mise.toml
├── PLAN.md
├── README.md
├── common/
│   └── src/
│       ├── audio.rs      # sample conversion, RMS
│       ├── jitter.rs     # jitter buffer, loss handling, adaptive resampling control
│       ├── packet.rs     # UDP packet header parse/serialize
│       └── resampler.rs  # linear resampler
├── sender/
│   └── src/main.rs       # dummy/sine/WAV/capture UDP sender
└── receiver/
    └── src/main.rs       # UDP receiver, WAV/null/CoreAudio output
```

## 必要なもの

共通:

- `mise`
- Rust toolchain: `mise.toml` で `rust@1.95.0` を指定

macOS で BlackHole 出力を試す場合:

- BlackHole などの仮想オーディオデバイス
- macOS 側で microphone/audio input として BlackHole を見るアプリ

Windows で VB-CABLE capture を試す場合:

- VB-Audio Virtual Cable
- Windows 側で Rust build できる環境
- `CABLE Output` が入力デバイスとして見える状態

このプロジェクトは LAN 内の実験用途です。暗号化、認証、Dante/AES67/RTP/PTP 互換機能はスコープ外です。

## 初回セットアップ

Apple Silicon Mac では、既存の x86_64 Rust が PATH にいると CoreAudio 関連 crate の build で失敗することがあります。この repo では `mise exec -- ...` を使って、`mise.toml` の toolchain を明示的に使います。

```bash
mise install
mise exec -- rustc -Vv
mise exec -- cargo --version
```

期待する `rustc -Vv` の例:

```text
host: aarch64-apple-darwin
release: 1.95.0
```

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

## テストと静的チェック

```bash
mise exec -- cargo fmt --all -- --check
mise exec -- cargo test
mise exec -- cargo clippy --all-targets -- -D warnings
```

CLI option の確認:

```bash
mise exec -- cargo run -p sender -- --help
mise exec -- cargo run -p receiver -- --help
```

## ローカル疎通確認

まず macOS 1台で `sine sender -> UDP localhost -> receiver -> WAV` を確認します。

Terminal 1:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 127.0.0.1:50000 \
  --output-file /tmp/my-lan-audio-loopback.wav \
  --duration-sec 12 \
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

期待値:

- sender が約 `200 packets/s` で送信する
- bitrate はおおむね `1.6 Mbps`
- receiver が受信中に `Running` へ入る
- `loss` / `late` / `dup` / `ooo` が 0 に近い
- `/tmp/my-lan-audio-loopback.wav` が `2ch / 48000Hz / 16bit` で生成される

WAV メタデータ確認例:

```bash
python3 - <<'PY'
import wave
p = "/tmp/my-lan-audio-loopback.wav"
with wave.open(p) as w:
    print(w.getnchannels(), w.getframerate(), w.getsampwidth(), w.getnframes(), w.getnframes() / w.getframerate())
PY
```

## sender の使い方

### dummy packet を送る

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input dummy
```

### sine wave を送る

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input sine \
  --freq 440
```

### WAV ファイルを送る

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input-file test.wav
```

ループ再生:

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input-file test.wav \
  --loop-input
```

### ネットワーク揺れを擬似発生させる

```bash
mise exec -- cargo run -p sender -- \
  --target 127.0.0.1:50000 \
  --input sine \
  --drop-rate 0.01 \
  --jitter-ms 20 \
  --reorder-rate 0.01 \
  --drift-ppm 50
```

主な simulation option:

- `--drop-rate 0.01`: 1% の packet を意図的に落とす
- `--jitter-ms 20`: 最大 20 ms の送信遅延を入れる
- `--reorder-rate 0.01`: packet 順序入れ替えを発生させる
- `--drift-ppm 50`: sender の送信ペースを 50 ppm 速くする

### 入力デバイス一覧を見る

```bash
mise exec -- cargo run -p sender -- --list-devices
```

Windows では `CABLE Output` を含む入力デバイスが見えることを確認します。

### capture の RMS meter だけ見る

```bash
mise exec -- cargo run -p sender -- \
  --device "CABLE Output" \
  --meter-only
```

### capture を WAV に保存する

```bash
mise exec -- cargo run -p sender -- \
  --device "CABLE Output" \
  --output-file capture.wav \
  --duration-sec 10
```

### capture を UDP 送信する

Windows 側で実行する想定です。

```bash
mise exec -- cargo run -p sender -- \
  --input capture \
  --device "CABLE Output" \
  --target 192.168.11.20:50000
```

`192.168.11.20` は macOS receiver 側の LAN IP に置き換えてください。

## receiver の使い方

### metrics だけ見る

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output null
```

### WAV に保存する

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output-file received.wav
```

### 出力デバイス一覧を見る

```bash
mise exec -- cargo run -p receiver -- --list-devices
```

### BlackHole に出力する

BlackHole は「入力デバイスに書く」のではなく、CoreAudio の output device として開きます。その結果、別アプリから BlackHole が input device として見えます。

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output audio \
  --output-device "BlackHole" \
  --target-buffer-ms 100 \
  --start-threshold-ms 100
```

### receiver 単体で BlackHole に test tone を出す

UDP 経路を使わず、CoreAudio/BlackHole 出力だけを確認します。

```bash
mise exec -- cargo run -p receiver -- \
  --test-tone \
  --output audio \
  --output-device "BlackHole" \
  --duration-sec 10
```

WAV に test tone を書く場合:

```bash
mise exec -- cargo run -p receiver -- \
  --test-tone \
  --output-file /tmp/my-lan-audio-tone.wav \
  --duration-sec 1
```

## Windows -> macOS の想定手順

macOS 側:

```bash
mise exec -- cargo run -p receiver -- \
  --listen 0.0.0.0:50000 \
  --output audio \
  --output-device "BlackHole 2ch" \
  --target-buffer-ms 100
```

Windows 側:

```bash
sender.exe \
  --input capture \
  --device "CABLE Output" \
  --target <macOSのLAN IP>:50000
```

確認ポイント:

- Windows 側 sender の RMS meter が音に応じて動く
- receiver の `packets` が約 200/s になる
- receiver の `buffer` が target 付近に留まる
- macOS 側の録音アプリ、OBS、DAW などで BlackHole 入力に信号が来る

## 主な CLI option

sender:

```text
--target <ADDR>             UDP送信先。例: 127.0.0.1:50000
--bind <ADDR>               UDP bind address。通常は 0.0.0.0:0
--input dummy|sine|capture  入力種別
--input-file <PATH>         WAV入力
--device <NAME_PART>        capture device name filter
--list-devices              入力デバイス一覧
--meter-only                capture meterのみ
--output-file <PATH>        captureをWAV保存
--packet-ms <MS>            packet duration。初期値 5
--duration-sec <SEC>        指定秒数で終了
--loop-input                WAV入力をループ
--drop-rate <RATE>          packet drop simulation
--jitter-ms <MS>            jitter simulation
--reorder-rate <RATE>       reorder simulation
--drift-ppm <PPM>           clock drift simulation
```

receiver:

```text
--listen <ADDR>             UDP listen address。例: 0.0.0.0:50000
--output null|audio|wav     出力先
--output-device <NAME_PART> CoreAudio output device name filter
--output-file <PATH>        WAV保存
--list-devices              出力デバイス一覧
--test-tone                 receiver単体でtest tone出力
--capacity-ms <MS>          jitter buffer capacity
--target-buffer-ms <MS>     目標buffer水位
--start-threshold-ms <MS>   再生開始まで貯める量
--no-adaptive-resampling    adaptive resamplingを無効化
--duration-sec <SEC>        指定秒数で終了
```

## 実装上の固定仕様

現時点の packet format は固定です。

```text
sample rate: 48000 Hz
channels: 2
sample format: signed 16-bit little endian PCM
packet duration: default 5 ms
frames per packet: 240
payload bytes per packet: 960
```

UDP payload はヘッダ込みでも通常の Ethernet MTU 1500 bytes 未満に収まる設計です。

## 現在の検証状況

ローカルで検証済み:

```text
mise exec -- cargo fmt --all -- --check
mise exec -- cargo test
mise exec -- cargo clippy --all-targets -- -D warnings
mise exec -- cargo run -p sender -- --help
mise exec -- cargo run -p receiver -- --help
mise exec -- cargo run -p receiver -- --test-tone --output-file /tmp/my-lan-audio-tone.wav --duration-sec 1
sine sender -> UDP localhost -> receiver -> WAV output
```

未検証、または実機が必要:

```text
BlackHoleを実出力デバイスとして開き、別アプリで入力確認
Windows CABLE Output の device list 確認
Windows VB-CABLE capture meter 確認
Windows VB-CABLE capture.wav 確認
Windows -> macOS end-to-end 確認
1時間連続テスト
24時間連続テスト
```

## トラブルシュート

### CoreAudio build で libclang architecture mismatch が出る

Apple Silicon Mac で x86_64 Rust が使われている可能性があります。

```bash
mise exec -- rustc -Vv
```

`host: aarch64-apple-darwin` になっていることを確認してください。通常の `cargo` ではなく、`mise exec -- cargo ...` を使います。

### receiver に packet が届かない

- receiver の `--listen` が `0.0.0.0:50000` または正しい IP になっているか確認する
- sender の `--target` が receiver 側の LAN IP と port を指しているか確認する
- macOS Firewall / Windows Defender Firewall を確認する
- まず `127.0.0.1` のローカル疎通確認を通す

### BlackHole に音が入らない

- `receiver --list-devices` で BlackHole が output device として見えるか確認する
- `receiver --test-tone --output audio --output-device "BlackHole"` で UDP 抜きの音出しを確認する
- 別アプリ側では BlackHole を input device として選ぶ

### sender が CABLE Output を見つけない

```bash
sender.exe --list-devices
```

実際に表示されたデバイス名に合わせて `--device` の文字列を短めに指定してください。

例:

```bash
sender.exe --input capture --device "CABLE Output" --target 192.168.11.20:50000
```

## 開発メモ

詳細な計画と残タスクは `PLAN.md` を参照してください。

この README は現在の実装状態に合わせています。長時間テストや Windows 実機確認が進んだら、`PLAN.md` とあわせて検証済み項目を更新してください。
