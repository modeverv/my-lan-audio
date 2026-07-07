.PHONY: help receive receiver sender p-receiver p-sender release receiver-devices sender-devices build test check

AUDIO_ADDR ?= 0.0.0.0:50000
FEEDBACK_ADDR ?= 192.168.11.51:50001

# TODO change to windows pc ip
W_AUDIO_ADDR ?= 192.168.11.65:50000
W_FEEDBACK_ADDR ?= 0.0.0.0:50001

RECEIVER_OUTPUT_DEVICE ?= MacBook Proのスピーカー
SENDER_INPUT_DEVICE ?= BlackHole 2ch

TARGET_BUFFER_MS ?= 60
#Target_BUFFER_MS ?= 15
START_THRESHOLD_MS ?= 80
#START_THRESHOLD_MS ?= 20
MAX_BUFFER_MS ?= 180
#MAX_BUFFER_MS ?= 40

OUTPUT_RING_MS ?= 60
#OUTPUT_RING_MS ?= 10
OUTPUT_RING_CAPACITY_MS ?= 160
RENDER_CHUNK_MS ?= 2
OUTPUT_BUFFER_SIZE_FRAMES ?= 256
#OUTPUT_BUFFER_SIZE_FRAMES ?= 64
PACKET_MS ?= 2.5
METRICS_INTERVAL_SEC ?= 1

help:
	@printf '%s\n' 'Targets:'
	@printf '%s\n' '  make receiver          Start receiver with low-latency localhost settings'
	@printf '%s\n' '  make receive           Alias for make receiver'
	@printf '%s\n' '  make sender            Start capture sender with feedback enabled'
	@printf '%s\n' '  make p-receiver        Start release receiver with feedback enabled'
	@printf '%s\n' '  make p-sender          Start release sender with feedback enabled'
	@printf '%s\n' '  make release           Build release binaries'
	@printf '%s\n' '  make receiver-devices  List receiver output devices'
	@printf '%s\n' '  make sender-devices    List sender input devices'
	@printf '%s\n' '  make build             Build workspace'
	@printf '%s\n' '  make test              Run cargo test'
	@printf '%s\n' '  make check             Run fmt, clippy, and tests'
	@printf '%s\n' ''
	@printf '%s\n' 'Common overrides:'
	@printf '%s\n' '  RECEIVER_OUTPUT_DEVICE="SOUNDPEATS Space"'
	@printf '%s\n' '  SENDER_INPUT_DEVICE="BlackHole"'
	@printf '%s\n' '  TARGET_BUFFER_MS=20 OUTPUT_RING_MS=15 PACKET_MS=2.5'

receiver:
	mise exec -- cargo run -p receiver -- \
	  --listen $(AUDIO_ADDR) \
	  --feedback-target $(FEEDBACK_ADDR) \
	  --low-latency \
	  --low-latency-trim-margin-ms 10 \
	  --low-latency-trim-to-margin-ms 10 \
	  --trim-crossfade-ms 1.5 \
	  --realtime-renderer \
	  --output audio \
	  --output-device "$(RECEIVER_OUTPUT_DEVICE)" \
	  --output-buffer-size-frames $(OUTPUT_BUFFER_SIZE_FRAMES) \
	  --target-buffer-ms $(TARGET_BUFFER_MS) \
	  --start-threshold-ms $(START_THRESHOLD_MS) \
	  --max-buffer-ms $(MAX_BUFFER_MS) \
	  --output-ring-ms $(OUTPUT_RING_MS) \
	  --output-ring-capacity-ms $(OUTPUT_RING_CAPACITY_MS) \
	  --render-chunk-ms $(RENDER_CHUNK_MS) \
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)

p-receiver-tmp:
	mise exec -- cargo build --release -p receiver
	target/release/receiver \
	  --listen $(AUDIO_ADDR) \
	  --feedback-target $(FEEDBACK_ADDR) \
	  --low-latency \
	  --low-latency-trim-margin-ms 10 \
	  --low-latency-trim-to-margin-ms 10 \
	  --trim-crossfade-ms 1.5 \
	  --realtime-renderer \
	  --output audio \
	  --output-device "BlackHole 2ch" \
	  --output-buffer-size-frames $(OUTPUT_BUFFER_SIZE_FRAMES) \
	  --target-buffer-ms $(TARGET_BUFFER_MS) \
	  --start-threshold-ms $(START_THRESHOLD_MS) \
	  --max-buffer-ms $(MAX_BUFFER_MS) \
	  --output-ring-ms $(OUTPUT_RING_MS) \
	  --output-ring-capacity-ms $(OUTPUT_RING_CAPACITY_MS) \
	  --render-chunk-ms $(RENDER_CHUNK_MS) \
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)

sender:
	mise exec -- cargo run -p sender -- \
	  --target $(AUDIO_ADDR) \
	  --feedback-listen $(FEEDBACK_ADDR) \
	  --input capture \
	  --device "$(SENDER_INPUT_DEVICE)" \
	  --packet-ms $(PACKET_MS) \
	  --sender-side-asrc \
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)



p-receiver:
	mise exec -- cargo build --release -p receiver
	target/release/receiver \
	  --listen $(AUDIO_ADDR) \
	  --feedback-target $(FEEDBACK_ADDR) \
	  --low-latency \
	  --low-latency-trim-margin-ms 10 \
	  --low-latency-trim-to-margin-ms 10 \
	  --trim-crossfade-ms 1.5 \
	  --realtime-renderer \
	  --output audio \
	  --output-device "$(RECEIVER_OUTPUT_DEVICE)" \
	  --output-buffer-size-frames $(OUTPUT_BUFFER_SIZE_FRAMES) \
	  --target-buffer-ms $(TARGET_BUFFER_MS) \
	  --start-threshold-ms $(START_THRESHOLD_MS) \
	  --max-buffer-ms $(MAX_BUFFER_MS) \
	  --output-ring-ms $(OUTPUT_RING_MS) \
	  --output-ring-capacity-ms $(OUTPUT_RING_CAPACITY_MS) \
	  --render-chunk-ms $(RENDER_CHUNK_MS) \
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)


p-sender:
	mise exec -- cargo build --release -p sender
	target/release/sender \
	  --target $(AUDIO_ADDR) \
	  --feedback-listen $(FEEDBACK_ADDR) \
	  --input capture \
	  --device "$(SENDER_INPUT_DEVICE)" \
	  --packet-ms $(PACKET_MS) \
	  --sender-side-asrc \
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)

w-sender:
	mise exec -- cargo build --release -p sender
	target/release/sender \
	  --target $(W_AUDIO_ADDR) \
	  --feedback-listen $(W_FEEDBACK_ADDR) \
	  --input capture \
	  --device "$(SENDER_INPUT_DEVICE)" \
	  --packet-ms $(PACKET_MS) \
	  --sender-side-asrc \
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)

receiver-list:
	mise exec -- cargo run -p receiver -- --list-devices

sender-list:
	mise exec -- cargo run -p sender -- --list-devices

build:
	mise exec -- cargo build

release:
	mise exec -- cargo build --release

test:
	mise exec -- cargo test

check:
	mise exec -- cargo fmt --all -- --check
	mise exec -- cargo clippy --all-targets -- -D warnings
	mise exec -- cargo test
