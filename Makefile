.PHONY: help receiver sender receiver-devices sender-devices build test check

AUDIO_ADDR ?= 127.0.0.1:50000
FEEDBACK_ADDR ?= 127.0.0.1:50001

RECEIVER_OUTPUT_DEVICE ?= MacBook
SENDER_INPUT_DEVICE ?= BlackHole

TARGET_BUFFER_MS ?= 30
START_THRESHOLD_MS ?= $(TARGET_BUFFER_MS)
MAX_BUFFER_MS ?= 90

OUTPUT_RING_MS ?= 20
OUTPUT_RING_CAPACITY_MS ?= 120
RENDER_CHUNK_MS ?= 5
METRICS_INTERVAL_SEC ?= 1

help:
	@printf '%s\n' 'Targets:'
	@printf '%s\n' '  make receiver          Start receiver with low-latency localhost settings'
	@printf '%s\n' '  make sender            Start capture sender with feedback enabled'
	@printf '%s\n' '  make receiver-devices  List receiver output devices'
	@printf '%s\n' '  make sender-devices    List sender input devices'
	@printf '%s\n' '  make build             Build workspace'
	@printf '%s\n' '  make test              Run cargo test'
	@printf '%s\n' '  make check             Run fmt, clippy, and tests'
	@printf '%s\n' ''
	@printf '%s\n' 'Common overrides:'
	@printf '%s\n' '  RECEIVER_OUTPUT_DEVICE="SOUNDPEATS Space"'
	@printf '%s\n' '  SENDER_INPUT_DEVICE="BlackHole"'
	@printf '%s\n' '  TARGET_BUFFER_MS=30 OUTPUT_RING_MS=20'

receiver:
	mise exec -- cargo run -p receiver -- \
	  --listen $(AUDIO_ADDR) \
	  --feedback-target $(FEEDBACK_ADDR) \
	  --output audio \
	  --output-device "$(RECEIVER_OUTPUT_DEVICE)" \
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
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)

receiver-devices:
	mise exec -- cargo run -p receiver -- --list-devices

sender-devices:
	mise exec -- cargo run -p sender -- --list-devices

build:
	mise exec -- cargo build

test:
	mise exec -- cargo test

check:
	mise exec -- cargo fmt --all -- --check
	mise exec -- cargo clippy --all-targets -- -D warnings
	mise exec -- cargo test
