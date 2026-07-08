.PHONY: help receive receiver sender fixed-receiver fixed-sender p-receiver p-receiver-tmp p-sender p-fixed-receiver p-fixed-sender w-sender release receiver-list sender-list receiver-devices sender-devices build test check

AUDIO_ADDR ?= 0.0.0.0:50000
FEEDBACK_ADDR ?= 192.168.11.28:50001

# TODO change to windows pc ip
W_AUDIO_ADDR ?= 192.168.11.65:50000
W_FEEDBACK_ADDR ?= 0.0.0.0:50001

#RECEIVER_OUTPUT_DEVICE ?= MacBook Proのスピーカー
#RECEIVER_OUTPUT_DEVICE ?= BlackHole 2ch
RECEIVER_OUTPUT_DEVICE ?= SOUNDPEATS Space

SENDER_INPUT_DEVICE ?= BlackHole 2ch

FIXED_DELAY_FRAMES ?= 14400
FIXED_LATENCY_MS ?=

OUTPUT_RING_MS ?= 60
OUTPUT_RING_CAPACITY_MS ?= 160
RENDER_CHUNK_MS ?= 2
OUTPUT_BUFFER_SIZE_FRAMES ?= 256
PACKET_MS ?= 5
METRICS_INTERVAL_SEC ?= 1

FIXED_DELAY_ARGS :=
ifneq ($(strip $(FIXED_DELAY_FRAMES)),)
FIXED_DELAY_ARGS := --fixed-delay-frames $(FIXED_DELAY_FRAMES)
endif
ifeq ($(strip $(FIXED_DELAY_ARGS)),)
ifneq ($(strip $(FIXED_LATENCY_MS)),)
FIXED_DELAY_ARGS := --fixed-latency-ms $(FIXED_LATENCY_MS)
endif
endif

help:
	@printf '%s\n' 'Targets:'
	@printf '%s\n' '  make receiver          Start fixed-buffer receiver'
	@printf '%s\n' '  make receive           Alias for make receiver'
	@printf '%s\n' '  make sender            Start capture sender with feedback enabled'
	@printf '%s\n' '  make fixed-receiver    Start receiver with explicit fixed-buffer settings'
	@printf '%s\n' '  make fixed-sender      Start sender with fixed-buffer packet settings'
	@printf '%s\n' '  make p-receiver        Start release receiver with feedback enabled'
	@printf '%s\n' '  make p-sender          Start release sender with feedback enabled'
	@printf '%s\n' '  make p-fixed-receiver  Start release receiver with explicit fixed-buffer settings'
	@printf '%s\n' '  make p-fixed-sender    Start release sender with fixed-buffer packet settings'
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
	@printf '%s\n' '  FIXED_DELAY_FRAMES=14400'
	@printf '%s\n' '  FIXED_DELAY_FRAMES= FIXED_LATENCY_MS=300'

receiver:
	mise exec -- cargo run -p receiver -- \
	  --listen $(AUDIO_ADDR) \
	  --feedback-target $(FEEDBACK_ADDR) \
	  $(FIXED_DELAY_ARGS) \
	  --output audio \
	  --output-device "$(RECEIVER_OUTPUT_DEVICE)" \
	  --output-buffer-size-frames $(OUTPUT_BUFFER_SIZE_FRAMES) \
	  --output-ring-ms $(OUTPUT_RING_MS) \
	  --output-ring-capacity-ms $(OUTPUT_RING_CAPACITY_MS) \
	  --render-chunk-ms $(RENDER_CHUNK_MS) \
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)

receive: receiver

fixed-receiver: FIXED_DELAY_FRAMES := 14400
fixed-receiver: OUTPUT_RING_MS := 60
fixed-receiver: OUTPUT_RING_CAPACITY_MS := 160
fixed-receiver: RENDER_CHUNK_MS := 2
fixed-receiver: OUTPUT_BUFFER_SIZE_FRAMES := 256
fixed-receiver: receiver

sender:
	mise exec -- cargo run -p sender -- \
	  --target $(AUDIO_ADDR) \
	  --feedback-listen $(FEEDBACK_ADDR) \
	  --input capture \
	  --device "$(SENDER_INPUT_DEVICE)" \
	  --packet-ms $(PACKET_MS) \
	  --sender-side-asrc \
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)

fixed-sender: PACKET_MS := 5
fixed-sender: sender


p-receiver:
	mise exec -- cargo build --release -p receiver
	target/release/receiver \
	  --listen $(AUDIO_ADDR) \
	  --feedback-target $(FEEDBACK_ADDR) \
	  $(FIXED_DELAY_ARGS) \
	  --output audio \
	  --output-device "$(RECEIVER_OUTPUT_DEVICE)" \
	  --output-buffer-size-frames $(OUTPUT_BUFFER_SIZE_FRAMES) \
	  --output-ring-ms $(OUTPUT_RING_MS) \
	  --output-ring-capacity-ms $(OUTPUT_RING_CAPACITY_MS) \
	  --render-chunk-ms $(RENDER_CHUNK_MS) \
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)

p-fixed-receiver: FIXED_DELAY_FRAMES := 14400
p-fixed-receiver: OUTPUT_RING_MS := 60
p-fixed-receiver: OUTPUT_RING_CAPACITY_MS := 160
p-fixed-receiver: RENDER_CHUNK_MS := 2
p-fixed-receiver: OUTPUT_BUFFER_SIZE_FRAMES := 256
p-fixed-receiver: p-receiver


p-sender:
	mise exec -- cargo build --release -p sender
	target/release/sender \
	  --target $(W_AUDIO_ADDR) \
	  --feedback-listen $(W_FEEDBACK_ADDR) \
	  --input capture \
	  --device "$(SENDER_INPUT_DEVICE)" \
	  --packet-ms $(PACKET_MS) \
	  --sender-side-asrc \
	  --metrics-interval-sec $(METRICS_INTERVAL_SEC)

p-fixed-sender: PACKET_MS := 5
p-fixed-sender: p-sender

receiver-list:
	mise exec -- cargo run -p receiver -- --list-devices

sender-list:
	mise exec -- cargo run -p sender -- --list-devices

receiver-devices: receiver-list

sender-devices: sender-list

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
