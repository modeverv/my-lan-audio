.PHONY: help d-receiver d-sender

AUDIO_PORT ?= 50000
FEEDBACK_PORT ?= 50001
LOG_DIR ?= logs

RECEIVER_LISTEN ?= 0.0.0.0:$(AUDIO_PORT)
RECEIVER_OUTPUT_DEVICE ?= BlackHole 2ch
RECEIVER_FEEDBACK_TARGET ?=

RECEIVER_FEEDBACK_TARGET ?=192.168.11.96:50001
# 480-10ms 4800-100ms
# 960-20 1440-30ms 2880-60ms
DIRECT_FIXED_DELAY_FRAMES ?= 1440

DIRECT_OUTPUT_SAMPLE_RATE ?= 48000
DIRECT_OUTPUT_BUFFER_SIZE_FRAMES ?= 32
DIRECT_CLOCK_SYNC ?= on

SENDER_TARGET ?= 127.0.0.1:$(AUDIO_PORT)
SENDER_INPUT_DEVICE ?= BlackHole 2ch
SENDER_FEEDBACK_LISTEN ?=
DIRECT_PACKET_MS ?= 1.0
DIRECT_CAPTURE_QUEUE_CAPACITY ?= 64

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

DIRECT_RECEIVER_ARGS = --listen $(RECEIVER_LISTEN) $(RECEIVER_FEEDBACK_ARG) $(RECEIVER_DEVICE_ARG) --fixed-delay-frames $(DIRECT_FIXED_DELAY_FRAMES) --clock-sync $(DIRECT_CLOCK_SYNC) --output-sample-rate $(DIRECT_OUTPUT_SAMPLE_RATE) --output-buffer-size-frames $(DIRECT_OUTPUT_BUFFER_SIZE_FRAMES)
DIRECT_SENDER_ARGS = --target $(SENDER_TARGET) $(SENDER_FEEDBACK_ARGS) $(SENDER_DEVICE_ARG) --packet-ms $(DIRECT_PACKET_MS) --capture-queue-capacity $(DIRECT_CAPTURE_QUEUE_CAPACITY) RECEIVER_FEEDBACK_TARGET=192.168.11.96:50001

help:
	@printf '%s\n' 'Targets:'
	@printf '%s\n' '  make d-receiver         Build and run logged direct receiver'
	@printf '%s\n' '  make d-sender           Build and run logged capture sender'
	@printf '%s\n' ''
	@printf '%s\n' 'Common overrides:'
	@printf '%s\n' '  RECEIVER_OUTPUT_DEVICE="BlackHole 2ch"'
	@printf '%s\n' '  SENDER_INPUT_DEVICE="BlackHole 2ch"'
	@printf '%s\n' '  SENDER_TARGET=<remote-ip>:$(AUDIO_PORT)'
	@printf '%s\n' '  DIRECT_FIXED_DELAY_FRAMES=4800'
	@printf '%s\n' '  DIRECT_PACKET_MS=1.0'
	@printf '%s\n' '  DIRECT_CAPTURE_QUEUE_CAPACITY=64'
	@printf '%s\n' '  DIRECT_CLOCK_SYNC=on'
	@printf '%s\n' '  LOG_DIR=logs'

d-receiver:
	@mkdir -p "$(LOG_DIR)"
	@log="$(LOG_DIR)/d-receiver-$$(date +%Y%m%d-%H%M%S).log"; \
	  printf 'logging to %s\n' "$$log"; \
	  bash -o pipefail -c '(mise exec -- cargo build --release -p receiver && target/release/receiver $(DIRECT_RECEIVER_ARGS)) 2>&1 | tee "$$1"' _ "$$log"

d-sender:
	@mkdir -p "$(LOG_DIR)"
	@log="$(LOG_DIR)/d-sender-$$(date +%Y%m%d-%H%M%S).log"; \
	  printf 'logging to %s\n' "$$log"; \
	  bash -o pipefail -c '(mise exec -- cargo build --release -p sender && target/release/sender $(DIRECT_SENDER_ARGS)) 2>&1 | tee "$$1"' _ "$$log"
