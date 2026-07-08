.PHONY: help receive receiver sender p-receiver p-sender release build test check receiver-devices sender-devices mac-ip

AUDIO_PORT ?= 50000
FEEDBACK_PORT ?= 50001

RECEIVER_LISTEN ?= 0.0.0.0:$(AUDIO_PORT)
RECEIVER_OUTPUT ?= audio
RECEIVER_OUTPUT_DEVICE ?= SOUNDPEATS Space
RECEIVER_FEEDBACK_TARGET ?=

SENDER_TARGET ?= 127.0.0.1:$(AUDIO_PORT)
SENDER_INPUT ?= capture
SENDER_INPUT_DEVICE ?= BlackHole 2ch
SENDER_FEEDBACK_LISTEN ?=

FIXED_DELAY_FRAMES ?= 14400
FIXED_LATENCY_MS ?=
PACKET_MS ?= 5

FIXED_DELAY_ARG :=
ifneq ($(strip $(FIXED_DELAY_FRAMES)),)
FIXED_DELAY_ARG := --fixed-delay-frames $(FIXED_DELAY_FRAMES)
endif
ifeq ($(strip $(FIXED_DELAY_ARG)),)
ifneq ($(strip $(FIXED_LATENCY_MS)),)
FIXED_DELAY_ARG := --fixed-latency-ms $(FIXED_LATENCY_MS)
endif
endif

RECEIVER_DEVICE_ARG :=
ifneq ($(strip $(RECEIVER_OUTPUT_DEVICE)),)
RECEIVER_DEVICE_ARG := --output-device "$(RECEIVER_OUTPUT_DEVICE)"
endif

RECEIVER_FEEDBACK_ARG :=
ifneq ($(strip $(RECEIVER_FEEDBACK_TARGET)),)
RECEIVER_FEEDBACK_ARG := --feedback-target $(RECEIVER_FEEDBACK_TARGET)
endif

SENDER_DEVICE_ARG :=
ifneq ($(strip $(SENDER_INPUT_DEVICE)),)
SENDER_DEVICE_ARG := --device "$(SENDER_INPUT_DEVICE)"
endif

SENDER_FEEDBACK_ARGS :=
ifneq ($(strip $(SENDER_FEEDBACK_LISTEN)),)
SENDER_FEEDBACK_ARGS := --feedback-listen $(SENDER_FEEDBACK_LISTEN) --sender-side-asrc
endif

RECEIVER_ARGS = --listen $(RECEIVER_LISTEN) $(RECEIVER_FEEDBACK_ARG) --output $(RECEIVER_OUTPUT) $(RECEIVER_DEVICE_ARG) $(FIXED_DELAY_ARG)
SENDER_ARGS = --target $(SENDER_TARGET) $(SENDER_FEEDBACK_ARGS) --input $(SENDER_INPUT) $(SENDER_DEVICE_ARG) --packet-ms $(PACKET_MS)

help:
	@printf '%s\n' 'Targets:'
	@printf '%s\n' '  make receiver           Run debug receiver'
	@printf '%s\n' '  make p-receiver         Build and run release receiver'
	@printf '%s\n' '  make sender             Run debug sender'
	@printf '%s\n' '  make p-sender           Build and run release sender'
	@printf '%s\n' '  make mac-ip             Print local LAN addresses for Windows sender target'
	@printf '%s\n' '  make receiver-devices   List receiver output devices'
	@printf '%s\n' '  make sender-devices     List sender input devices'
	@printf '%s\n' '  make check              Run fmt, clippy, and tests'
	@printf '%s\n' ''
	@printf '%s\n' 'Windows -> macOS:'
	@printf '%s\n' '  1. make mac-ip'
	@printf '%s\n' '  2. make p-receiver'
	@printf '%s\n' '  3. Windows sender target: <mac-ip>:$(AUDIO_PORT)'
	@printf '%s\n' '  feedback: make p-receiver RECEIVER_FEEDBACK_TARGET=<windows-ip>:$(FEEDBACK_PORT)'
	@printf '%s\n' ''
	@printf '%s\n' 'Common overrides:'
	@printf '%s\n' '  RECEIVER_OUTPUT_DEVICE="BlackHole 2ch"'
	@printf '%s\n' '  FIXED_DELAY_FRAMES=14400'
	@printf '%s\n' '  FIXED_DELAY_FRAMES= FIXED_LATENCY_MS=300'
	@printf '%s\n' '  SENDER_TARGET=<remote-ip>:$(AUDIO_PORT)'

receiver:
	mise exec -- cargo run -p receiver -- $(RECEIVER_ARGS)

receive: receiver

p-receiver:
	mise exec -- cargo build --release -p receiver
	target/release/receiver $(RECEIVER_ARGS)

sender:
	mise exec -- cargo run -p sender -- $(SENDER_ARGS)

p-sender:
	mise exec -- cargo build --release -p sender
	target/release/sender $(SENDER_ARGS)

mac-ip:
	@for iface in en0 en8 en7 en6 en5 en4; do \
	  ip=$$(ipconfig getifaddr $$iface 2>/dev/null || true); \
	  if [ -n "$$ip" ]; then printf '%s %s:%s\n' "$$iface" "$$ip" "$(AUDIO_PORT)"; fi; \
	done

receiver-devices:
	mise exec -- cargo run -p receiver -- --list-devices

sender-devices:
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
