mod audio_ring;

use anyhow::{anyhow, bail, Context, Result};
use audio_ring::SpscF32Ring;
use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig, I24, U24,
};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use lan_audio_common::audio::{f32_to_i16, CHANNELS, SAMPLE_RATE};
use lan_audio_common::jitter::{InsertOutcome, JitterBuffer, JitterConfig, JitterMetrics};
use lan_audio_common::packet::AudioPacket;
use lan_audio_common::status::ReceiverStatus;
use socket2::{Domain, Protocol, Socket, Type};
use std::fs::File;
use std::io::BufWriter;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_FIXED_DELAY_FRAMES: u64 = 14_400;
const FIXED_LATENCY_MIN_CAPACITY_MS: u32 = 600;

#[derive(Parser, Debug)]
#[command(about = "LAN audio UDP receiver")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:50000")]
    listen: SocketAddr,

    #[arg(long)]
    feedback_target: Option<SocketAddr>,

    #[arg(long, default_value = "null", value_parser = ["null", "audio", "wav"])]
    output: String,

    #[arg(long)]
    output_device: Option<String>,

    #[arg(long)]
    output_file: Option<PathBuf>,

    #[arg(long)]
    list_devices: bool,

    #[arg(long)]
    test_tone: bool,

    #[arg(long, default_value_t = SAMPLE_RATE)]
    sample_rate: u32,

    #[arg(long, default_value_t = CHANNELS)]
    channels: u16,

    #[arg(
        long,
        help = "Keep the receiver jitter buffer at the given 48k source-frame depth"
    )]
    fixed_delay_frames: Option<u64>,

    #[arg(
        long,
        help = "Human-friendly alias for --fixed-delay-frames, converted using the packet sample rate"
    )]
    fixed_latency_ms: Option<u32>,

    #[arg(long)]
    output_buffer_size_frames: Option<u32>,

    #[arg(long, default_value_t = 40)]
    output_ring_ms: u32,

    #[arg(long, default_value_t = 200)]
    output_ring_capacity_ms: u32,

    #[arg(long, default_value_t = 5)]
    render_chunk_ms: u32,

    #[arg(long, default_value_t = 1_048_576)]
    socket_recv_buffer_bytes: usize,

    #[arg(long, default_value_t = 2048)]
    packet_queue_capacity: usize,

    #[arg(long)]
    duration_sec: Option<f64>,

    #[arg(long, default_value_t = 1.0)]
    metrics_interval_sec: f64,

    #[arg(long, default_value_t = 440.0)]
    freq: f32,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let timing = ReceiverTiming::from_args(&args)?;
    validate_audio_args(&args, &timing)?;

    if args.list_devices {
        return list_output_devices();
    }

    if args.test_tone {
        return run_test_tone(&args);
    }

    run_receiver(&args, timing)
}

#[derive(Clone, Copy, Debug)]
struct ReceiverTiming {
    capacity_ms: u32,
    target_buffer_ms: u32,
    fixed_delay_frames: u64,
    output_ring_ms: u32,
    output_ring_capacity_ms: u32,
    render_chunk_ms: u32,
    output_buffer_size_frames: Option<u32>,
}

impl ReceiverTiming {
    fn from_args(args: &Args) -> Result<Self> {
        if args.fixed_delay_frames.is_some() && args.fixed_latency_ms.is_some() {
            bail!("--fixed-delay-frames and --fixed-latency-ms cannot be combined");
        }

        let fixed_delay_frames = args
            .fixed_delay_frames
            .or_else(|| {
                args.fixed_latency_ms
                    .map(|ms| ms_to_frames(ms, args.sample_rate))
            })
            .unwrap_or(DEFAULT_FIXED_DELAY_FRAMES);
        let fixed_delay_ms = frames_to_ms_ceil(fixed_delay_frames, args.sample_rate);

        Ok(Self {
            capacity_ms: fixed_delay_capacity_ms(fixed_delay_frames, args.sample_rate),
            target_buffer_ms: fixed_delay_ms,
            fixed_delay_frames,
            output_ring_ms: args.output_ring_ms,
            output_ring_capacity_ms: args.output_ring_capacity_ms,
            render_chunk_ms: args.render_chunk_ms,
            output_buffer_size_frames: args.output_buffer_size_frames,
        })
    }
}

fn ms_to_frames(ms: u32, sample_rate: u32) -> u64 {
    sample_rate as u64 * ms as u64 / 1000
}

fn frames_to_ms_ceil(frames: u64, sample_rate: u32) -> u32 {
    let sample_rate = u64::from(sample_rate.max(1));
    frames
        .saturating_mul(1000)
        .saturating_add(sample_rate - 1)
        .checked_div(sample_rate)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32
}

fn frames_to_ms_f64(frames: u64, sample_rate: u32) -> f64 {
    frames as f64 * 1000.0 / sample_rate.max(1) as f64
}

fn format_fixed_delay(frames: u64, sample_rate: u32) -> String {
    format!("{frames}fr/{:.3}ms", frames_to_ms_f64(frames, sample_rate))
}

fn fixed_delay_capacity_ms(fixed_delay_frames: u64, sample_rate: u32) -> u32 {
    frames_to_ms_ceil(fixed_delay_frames, sample_rate)
        .saturating_mul(3)
        .max(FIXED_LATENCY_MIN_CAPACITY_MS)
}

fn validate_audio_args(args: &Args, timing: &ReceiverTiming) -> Result<()> {
    if args.sample_rate != SAMPLE_RATE {
        bail!("only 48000Hz packets are supported today");
    }
    if args.channels != CHANNELS {
        bail!("only stereo packets are supported today");
    }
    if timing.fixed_delay_frames == 0 {
        bail!("fixed delay must be greater than zero frames");
    }
    if timing.target_buffer_ms == 0 {
        bail!("buffer timing values must be greater than zero");
    }
    if args.output_buffer_size_frames == Some(0) {
        bail!("--output-buffer-size-frames must be greater than zero");
    }
    if timing.output_ring_ms == 0
        || timing.output_ring_capacity_ms == 0
        || timing.render_chunk_ms == 0
    {
        bail!("output ring and render chunk timing values must be greater than zero");
    }
    if timing.output_ring_capacity_ms < timing.output_ring_ms + timing.render_chunk_ms {
        bail!("--output-ring-capacity-ms must be at least --output-ring-ms + --render-chunk-ms");
    }
    if args.packet_queue_capacity == 0 {
        bail!("--packet-queue-capacity must be greater than zero");
    }
    Ok(())
}

fn run_receiver(args: &Args, timing: ReceiverTiming) -> Result<()> {
    let socket = bind_socket(args.listen, args.socket_recv_buffer_bytes)?;
    println!(
        "receiver: listening={} output={} output_file={:?} fixed_delay={} capacity={}ms",
        args.listen,
        output_mode(args),
        args.output_file,
        format_fixed_delay(timing.fixed_delay_frames, args.sample_rate),
        timing.capacity_ms
    );

    let jitter_config = JitterConfig {
        sample_rate: args.sample_rate,
        channels: args.channels,
        capacity_ms: timing.capacity_ms,
        target_ms: timing.target_buffer_ms,
        fixed_delay_frames: timing.fixed_delay_frames,
    };
    let (event_tx, event_rx) = sync_channel(args.packet_queue_capacity);
    let ingress_metrics = Arc::new(IngressMetrics::default());
    let receiver_state = Arc::new(ReceiverState::new(timing.target_buffer_ms));

    spawn_udp_receiver(socket, event_tx, Arc::clone(&ingress_metrics));

    match output_mode(args).as_str() {
        "audio" => run_audio_output(
            args,
            timing,
            jitter_config,
            event_rx,
            receiver_state,
            Arc::clone(&ingress_metrics),
        ),
        "wav" => run_timed_pull_output(
            args,
            jitter_config,
            event_rx,
            receiver_state,
            Arc::clone(&ingress_metrics),
            args.output_file.as_deref(),
        ),
        "null" => run_timed_pull_output(
            args,
            jitter_config,
            event_rx,
            receiver_state,
            Arc::clone(&ingress_metrics),
            None,
        ),
        other => bail!("unsupported output mode {other}"),
    }
}

fn output_mode(args: &Args) -> String {
    if args.output_file.is_some() {
        "wav".to_string()
    } else {
        args.output.clone()
    }
}

fn bind_socket(addr: SocketAddr, recv_buffer_bytes: usize) -> Result<UdpSocket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_recv_buffer_size(recv_buffer_bytes)?;
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

enum ReceiverEvent {
    Packet(AudioPacket, SocketAddr, Instant),
    InvalidPacket,
}

#[derive(Default)]
struct IngressMetrics {
    queued_packets: AtomicU64,
    queued_invalid_packets: AtomicU64,
    queue_drops: AtomicU64,
    sources: Mutex<IngressSources>,
}

#[derive(Clone, Copy, Debug, Default)]
struct IngressSnapshot {
    queued_packets: u64,
    queued_invalid_packets: u64,
    queue_drops: u64,
    active_stream_id: Option<u64>,
    active_source: Option<SocketAddr>,
    foreign_stream_id: Option<u64>,
    foreign_source: Option<SocketAddr>,
}

#[derive(Clone, Copy, Debug, Default)]
struct IngressSources {
    active_stream_id: Option<u64>,
    active_source: Option<SocketAddr>,
    foreign_stream_id: Option<u64>,
    foreign_source: Option<SocketAddr>,
}

impl IngressMetrics {
    fn record_packet_queued(&self) {
        self.queued_packets.fetch_add(1, Ordering::Relaxed);
    }

    fn record_invalid_queued(&self) {
        self.queued_invalid_packets.fetch_add(1, Ordering::Relaxed);
    }

    fn record_queue_drop(&self) {
        self.queue_drops.fetch_add(1, Ordering::Relaxed);
    }

    fn record_stream_source(&self, outcome: InsertOutcome, stream_id: u64, source: SocketAddr) {
        let Ok(mut sources) = self.sources.lock() else {
            return;
        };
        match outcome {
            InsertOutcome::ForeignStream => {
                sources.foreign_stream_id = Some(stream_id);
                sources.foreign_source = Some(source);
            }
            InsertOutcome::Accepted | InsertOutcome::Duplicate | InsertOutcome::Late => {
                sources.active_stream_id = Some(stream_id);
                sources.active_source = Some(source);
            }
            InsertOutcome::Resynced => {
                sources.active_stream_id = Some(stream_id);
                sources.active_source = Some(source);
                sources.foreign_stream_id = None;
                sources.foreign_source = None;
            }
        }
    }

    fn snapshot(&self) -> IngressSnapshot {
        let sources = self
            .sources
            .lock()
            .map(|sources| *sources)
            .unwrap_or_default();
        IngressSnapshot {
            queued_packets: self.queued_packets.load(Ordering::Relaxed),
            queued_invalid_packets: self.queued_invalid_packets.load(Ordering::Relaxed),
            queue_drops: self.queue_drops.load(Ordering::Relaxed),
            active_stream_id: sources.active_stream_id,
            active_source: sources.active_source,
            foreign_stream_id: sources.foreign_stream_id,
            foreign_source: sources.foreign_source,
        }
    }
}

#[derive(Clone, Debug)]
struct ReceiverSnapshot {
    metrics: JitterMetrics,
    target_ms: u32,
}

struct ReceiverState {
    snapshot: Mutex<ReceiverSnapshot>,
}

impl ReceiverState {
    fn new(target_ms: u32) -> Self {
        Self {
            snapshot: Mutex::new(ReceiverSnapshot {
                metrics: JitterMetrics::default(),
                target_ms,
            }),
        }
    }

    fn publish(&self, jitter: &JitterBuffer) {
        let Ok(mut snapshot) = self.snapshot.try_lock() else {
            return;
        };
        snapshot.metrics = jitter.metrics();
        snapshot.target_ms = jitter.target_ms();
    }

    fn snapshot(&self) -> Option<ReceiverSnapshot> {
        self.snapshot
            .try_lock()
            .ok()
            .map(|snapshot| snapshot.clone())
    }
}

fn spawn_udp_receiver(
    socket: UdpSocket,
    event_tx: SyncSender<ReceiverEvent>,
    ingress_metrics: Arc<IngressMetrics>,
) {
    thread::spawn(move || {
        let mut buf = vec![0u8; 2048];
        loop {
            match socket.recv_from(&mut buf) {
                Ok((len, source)) => {
                    let arrival = Instant::now();
                    match AudioPacket::from_bytes(&buf[..len]) {
                        Ok(packet) => {
                            send_receiver_event(
                                &event_tx,
                                ReceiverEvent::Packet(packet, source, arrival),
                                &ingress_metrics,
                                false,
                            );
                        }
                        Err(err) => {
                            eprintln!("receiver: invalid packet: {err}");
                            send_receiver_event(
                                &event_tx,
                                ReceiverEvent::InvalidPacket,
                                &ingress_metrics,
                                true,
                            );
                        }
                    }
                }
                Err(err) => eprintln!("receiver: UDP receive error: {err}"),
            }
        }
    });
}

fn send_receiver_event(
    event_tx: &SyncSender<ReceiverEvent>,
    event: ReceiverEvent,
    ingress_metrics: &IngressMetrics,
    invalid: bool,
) {
    match event_tx.try_send(event) {
        Ok(()) => {
            if invalid {
                ingress_metrics.record_invalid_queued();
            } else {
                ingress_metrics.record_packet_queued();
            }
        }
        Err(TrySendError::Full(_)) => ingress_metrics.record_queue_drop(),
        Err(TrySendError::Disconnected(_)) => {}
    }
}

fn drain_receiver_events(
    event_rx: &Receiver<ReceiverEvent>,
    jitter: &mut JitterBuffer,
    ingress_metrics: &IngressMetrics,
) {
    while let Ok(event) = event_rx.try_recv() {
        match event {
            ReceiverEvent::Packet(packet, source, arrival) => {
                let stream_id = packet.header.stream_id;
                let outcome = jitter.insert_packet(packet, arrival);
                ingress_metrics.record_stream_source(outcome, stream_id, source);
            }
            ReceiverEvent::InvalidPacket => jitter.record_invalid_packet(),
        }
    }
}

fn run_timed_pull_output(
    args: &Args,
    jitter_config: JitterConfig,
    event_rx: Receiver<ReceiverEvent>,
    receiver_state: Arc<ReceiverState>,
    ingress_metrics: Arc<IngressMetrics>,
    output_file: Option<&Path>,
) -> Result<()> {
    let mut jitter = JitterBuffer::new(jitter_config);
    let chunk_frames = (args.sample_rate / 100) as usize;
    let channels = args.channels as usize;
    let mut writer = if let Some(path) = output_file {
        Some(
            create_wav_writer(path, args.sample_rate)
                .with_context(|| format!("failed to create {}", path.display()))?,
        )
    } else {
        None
    };
    let feedback = FeedbackSender::new(args.feedback_target)?;
    let mut metrics = MetricsPrinter::new(
        args.metrics_interval_sec,
        None,
        feedback,
        args.sample_rate,
        args.channels,
        Arc::clone(&receiver_state),
        Arc::clone(&ingress_metrics),
    );
    let mut next_tick = Instant::now();
    let start = Instant::now();
    let mut output = vec![0.0f32; chunk_frames * channels];

    loop {
        if duration_elapsed(start, args.duration_sec) {
            if let Some(writer) = writer {
                writer.finalize().context("failed to finalize WAV output")?;
            }
            return Ok(());
        }

        drain_receiver_events(&event_rx, &mut jitter, &ingress_metrics);
        jitter.pull_f32_at_sample_rate_for_playout(&mut output, args.sample_rate, next_tick);
        receiver_state.publish(&jitter);

        if let Some(writer) = writer.as_mut() {
            for sample in &output {
                writer.write_sample(f32_to_i16(*sample))?;
            }
        }

        metrics.maybe_print();
        sleep_until(next_tick);
        next_tick += Duration::from_millis(10);
    }
}

fn run_audio_output(
    args: &Args,
    timing: ReceiverTiming,
    jitter_config: JitterConfig,
    event_rx: Receiver<ReceiverEvent>,
    receiver_state: Arc<ReceiverState>,
    ingress_metrics: Arc<IngressMetrics>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = select_output_device(&host, args.output_device.as_deref())?;
    let name = device.to_string();
    let supported = device
        .default_output_config()
        .context("failed to get default output config")?;
    let sample_format = supported.sample_format();
    let mut config = supported.config();
    config.channels = args.channels;
    if let Some(frames) = timing.output_buffer_size_frames {
        config.buffer_size = BufferSize::Fixed(frames);
    }

    println!(
        "receiver: output_device=\"{}\" output_format={}Hz/{}ch/{:?} buffer={:?}",
        name, config.sample_rate, config.channels, sample_format, config.buffer_size
    );

    let channels = usize::from(config.channels.max(1));
    let ring_capacity_samples =
        samples_from_ms(config.sample_rate, channels, timing.output_ring_capacity_ms);
    let ring = Arc::new(SpscF32Ring::new(ring_capacity_samples));
    let callback_metrics = Arc::new(OutputCallbackMetrics::default());
    spawn_ring_renderer(
        jitter_config,
        event_rx,
        Arc::clone(&ring),
        Arc::clone(&callback_metrics),
        Arc::clone(&receiver_state),
        Arc::clone(&ingress_metrics),
        RingRendererConfig {
            output_sample_rate: config.sample_rate,
            channels,
            output_ring_ms: timing.output_ring_ms,
            render_chunk_ms: timing.render_chunk_ms,
            realtime_priority: true,
        },
    );
    let stream = build_ring_output_stream(
        &device,
        sample_format,
        &config,
        Arc::clone(&ring),
        Arc::clone(&callback_metrics),
    )?;
    stream.play().context("failed to start output stream")?;

    let feedback = FeedbackSender::new(args.feedback_target)?;
    let mut metrics = MetricsPrinter::new(
        args.metrics_interval_sec,
        Some(callback_metrics),
        feedback,
        config.sample_rate,
        config.channels,
        Arc::clone(&receiver_state),
        Arc::clone(&ingress_metrics),
    );
    let start = Instant::now();
    loop {
        if duration_elapsed(start, args.duration_sec) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
        metrics.maybe_print();
    }
}

fn build_ring_output_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    ring: Arc<SpscF32Ring>,
    callback_metrics: Arc<OutputCallbackMetrics>,
) -> Result<Stream> {
    let stream = match sample_format {
        SampleFormat::I8 => {
            build_ring_output_stream_for::<i8>(device, config, ring, callback_metrics)?
        }
        SampleFormat::I16 => {
            build_ring_output_stream_for::<i16>(device, config, ring, callback_metrics)?
        }
        SampleFormat::I24 => {
            build_ring_output_stream_for::<I24>(device, config, ring, callback_metrics)?
        }
        SampleFormat::I32 => {
            build_ring_output_stream_for::<i32>(device, config, ring, callback_metrics)?
        }
        SampleFormat::I64 => {
            build_ring_output_stream_for::<i64>(device, config, ring, callback_metrics)?
        }
        SampleFormat::U8 => {
            build_ring_output_stream_for::<u8>(device, config, ring, callback_metrics)?
        }
        SampleFormat::U16 => {
            build_ring_output_stream_for::<u16>(device, config, ring, callback_metrics)?
        }
        SampleFormat::U24 => {
            build_ring_output_stream_for::<U24>(device, config, ring, callback_metrics)?
        }
        SampleFormat::U32 => {
            build_ring_output_stream_for::<u32>(device, config, ring, callback_metrics)?
        }
        SampleFormat::U64 => {
            build_ring_output_stream_for::<u64>(device, config, ring, callback_metrics)?
        }
        SampleFormat::F32 => {
            build_ring_output_stream_for::<f32>(device, config, ring, callback_metrics)?
        }
        SampleFormat::F64 => {
            build_ring_output_stream_for::<f64>(device, config, ring, callback_metrics)?
        }
        SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 => {
            bail!("DSD output sample format {sample_format:?} is not supported")
        }
        other => bail!("unsupported output sample format {other:?}"),
    };
    Ok(stream)
}

fn build_ring_output_stream_for<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    ring: Arc<SpscF32Ring>,
    callback_metrics: Arc<OutputCallbackMetrics>,
) -> Result<Stream>
where
    T: Sample + SizedSample + FromSample<f32> + Send + 'static,
{
    let err_fn = |err| eprintln!("audio output stream error: {err}");
    let channels = usize::from(config.channels.max(1));
    let mut scratch = vec![0.0f32; scratch_len_for_stream(config)];
    let mut last_frame = vec![0.0f32; channels];

    Ok(device.build_output_stream(
        *config,
        move |data: &mut [T], _| {
            callback_metrics.record_callback(data.len() / channels);
            if data.len() > scratch.len() {
                callback_metrics.record_scratch_overflow();
                fill_with_last_frame(data, channels, &last_frame);
                return;
            }

            let scratch = &mut scratch[..data.len()];
            let popped = ring.pop_interleaved(scratch, channels);
            callback_metrics.record_output_queue_samples(ring.len_samples());
            if popped > 0 {
                update_last_frame(&scratch[..popped], channels, &mut last_frame);
            }
            if popped < scratch.len() {
                callback_metrics.record_ring_underrun((scratch.len() - popped) / channels);
                fill_scratch_with_last_frame(&mut scratch[popped..], channels, &last_frame);
            }
            for (dst, src) in data.iter_mut().zip(scratch.iter().copied()) {
                *dst = output_sample(src);
            }
        },
        err_fn,
        None,
    )?)
}

#[derive(Clone, Copy, Debug)]
struct RingRendererConfig {
    output_sample_rate: u32,
    channels: usize,
    output_ring_ms: u32,
    render_chunk_ms: u32,
    realtime_priority: bool,
}

fn spawn_ring_renderer(
    jitter_config: JitterConfig,
    event_rx: Receiver<ReceiverEvent>,
    ring: Arc<SpscF32Ring>,
    metrics: Arc<OutputCallbackMetrics>,
    receiver_state: Arc<ReceiverState>,
    ingress_metrics: Arc<IngressMetrics>,
    config: RingRendererConfig,
) {
    let spawn_result = thread::Builder::new()
        .name("receiver-ring-renderer".to_string())
        .spawn(move || {
            if config.realtime_priority {
                raise_renderer_thread_priority();
            }
            let mut jitter = JitterBuffer::new(jitter_config);
            let channels = config.channels.max(1);
            let target_samples =
                samples_from_ms(config.output_sample_rate, channels, config.output_ring_ms);
            let chunk_frames =
                (config.output_sample_rate as usize * config.render_chunk_ms as usize / 1000)
                    .max(1);
            let mut render_scratch = vec![0.0f32; chunk_frames * channels];
            let sleep_divisor = 2;
            let min_sleep_us = 1_000;
            let sleep_duration = Duration::from_micros(
                ((config.render_chunk_ms as u64 * 1000) / sleep_divisor).max(min_sleep_us),
            );

            loop {
                drain_receiver_events(&event_rx, &mut jitter, &ingress_metrics);
                loop {
                    let queued = ring.len_samples();
                    metrics.record_output_queue_samples(queued);
                    if queued >= target_samples {
                        metrics.mark_ring_ready();
                        break;
                    }

                    let missing_samples = target_samples - queued;
                    let frames_to_render = (missing_samples / channels).min(chunk_frames);
                    if frames_to_render == 0 {
                        break;
                    }
                    let sample_count = frames_to_render * channels;
                    let scratch = &mut render_scratch[..sample_count];

                    let missing_frames_after_render =
                        (missing_samples / channels).saturating_sub(frames_to_render);
                    let buffered_after_pull_frames = ((missing_frames_after_render as f64
                        * SAMPLE_RATE as f64)
                        / config.output_sample_rate.max(1) as f64)
                        .ceil() as u64;
                    jitter.pull_f32_at_sample_rate_with_buffer_target(
                        scratch,
                        config.output_sample_rate,
                        buffered_after_pull_frames,
                    );

                    let pushed = ring.push_interleaved(scratch, channels);
                    if pushed < sample_count {
                        metrics.record_ring_overflow((sample_count - pushed) / channels);
                        break;
                    }
                }

                receiver_state.publish(&jitter);
                thread::sleep(sleep_duration);
            }
        });

    if let Err(err) = spawn_result {
        eprintln!("receiver: failed to spawn ring renderer thread: {err}");
    }
}

#[cfg(target_os = "macos")]
fn raise_renderer_thread_priority() {
    unsafe {
        let result =
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
        if result != 0 {
            eprintln!("receiver: failed to raise renderer thread QoS: {result}");
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn raise_renderer_thread_priority() {}

fn update_last_frame(samples: &[f32], channels: usize, last_frame: &mut [f32]) {
    if samples.len() < channels || last_frame.len() < channels {
        return;
    }
    let start = samples.len() - channels;
    last_frame[..channels].copy_from_slice(&samples[start..start + channels]);
}

fn fill_scratch_with_last_frame(data: &mut [f32], channels: usize, last_frame: &[f32]) {
    for frame in data.chunks_exact_mut(channels) {
        for (dst, src) in frame.iter_mut().zip(last_frame.iter().copied()) {
            *dst = src;
        }
    }
}

fn fill_with_last_frame<T>(data: &mut [T], channels: usize, last_frame: &[f32])
where
    T: Sample + FromSample<f32>,
{
    for frame in data.chunks_exact_mut(channels) {
        for (dst, src) in frame.iter_mut().zip(last_frame.iter().copied()) {
            *dst = output_sample(src);
        }
    }
}

fn output_sample<T>(sample: f32) -> T
where
    T: Sample + FromSample<f32>,
{
    T::from_sample(sample.clamp(-1.0, 1.0))
}

fn samples_from_ms(sample_rate: u32, channels: usize, ms: u32) -> usize {
    let channels = channels.max(1);
    ((sample_rate as usize * ms as usize / 1000).max(1)).saturating_mul(channels)
}

fn scratch_len_for_stream(config: &StreamConfig) -> usize {
    let channels = usize::from(config.channels.max(1));
    let frames = match config.buffer_size {
        BufferSize::Fixed(frames) => usize::try_from(frames).unwrap_or(usize::MAX / channels),
        BufferSize::Default => {
            (usize::try_from(config.sample_rate).unwrap_or(48_000) / 2).max(2048)
        }
    };
    frames.saturating_mul(channels)
}

fn run_test_tone(args: &Args) -> Result<()> {
    if let Some(path) = &args.output_file {
        let duration = args.duration_sec.unwrap_or(3.0);
        let mut writer = create_wav_writer(path, args.sample_rate)?;
        let total_frames = (args.sample_rate as f64 * duration) as usize;
        let mut phase = 0.0f32;
        let step = args.freq * std::f32::consts::TAU / args.sample_rate as f32;
        for _ in 0..total_frames {
            let sample = phase.sin() * 0.2;
            phase = (phase + step) % std::f32::consts::TAU;
            writer.write_sample(f32_to_i16(sample))?;
            writer.write_sample(f32_to_i16(sample))?;
        }
        writer.finalize()?;
        return Ok(());
    }

    let host = cpal::default_host();
    let device = select_output_device(&host, args.output_device.as_deref())?;
    let name = device.to_string();
    let supported = device
        .default_output_config()
        .context("failed to get default output config")?;
    let sample_format = supported.sample_format();
    let mut config = supported.config();
    config.channels = args.channels;
    let phase = Arc::new(Mutex::new(0.0f32));

    println!(
        "receiver: test_tone output_device=\"{}\" output_format={}Hz/{}ch/{:?}",
        name, config.sample_rate, config.channels, sample_format
    );
    let stream = build_tone_output_stream(&device, sample_format, &config, phase, args.freq)?;
    stream.play().context("failed to start output stream")?;

    let start = Instant::now();
    loop {
        if duration_elapsed(start, args.duration_sec) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn build_tone_output_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    phase: Arc<Mutex<f32>>,
    freq: f32,
) -> Result<Stream> {
    let stream = match sample_format {
        SampleFormat::I8 => build_tone_output_stream_for::<i8>(device, config, phase, freq)?,
        SampleFormat::I16 => build_tone_output_stream_for::<i16>(device, config, phase, freq)?,
        SampleFormat::I24 => build_tone_output_stream_for::<I24>(device, config, phase, freq)?,
        SampleFormat::I32 => build_tone_output_stream_for::<i32>(device, config, phase, freq)?,
        SampleFormat::I64 => build_tone_output_stream_for::<i64>(device, config, phase, freq)?,
        SampleFormat::U8 => build_tone_output_stream_for::<u8>(device, config, phase, freq)?,
        SampleFormat::U16 => build_tone_output_stream_for::<u16>(device, config, phase, freq)?,
        SampleFormat::U24 => build_tone_output_stream_for::<U24>(device, config, phase, freq)?,
        SampleFormat::U32 => build_tone_output_stream_for::<u32>(device, config, phase, freq)?,
        SampleFormat::U64 => build_tone_output_stream_for::<u64>(device, config, phase, freq)?,
        SampleFormat::F32 => build_tone_output_stream_for::<f32>(device, config, phase, freq)?,
        SampleFormat::F64 => build_tone_output_stream_for::<f64>(device, config, phase, freq)?,
        SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 => {
            bail!("DSD output sample format {sample_format:?} is not supported")
        }
        other => bail!("unsupported output sample format {other:?}"),
    };
    Ok(stream)
}

fn build_tone_output_stream_for<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    phase: Arc<Mutex<f32>>,
    freq: f32,
) -> Result<Stream>
where
    T: Sample + SizedSample + FromSample<f32> + Send + 'static,
{
    let sample_rate = config.sample_rate;
    let channels = usize::from(config.channels.max(1));
    let err_fn = |err| eprintln!("audio output stream error: {err}");

    Ok(device.build_output_stream(
        *config,
        move |data: &mut [T], _| fill_tone(data, channels, sample_rate, freq, &phase),
        err_fn,
        None,
    )?)
}

fn fill_tone<T>(
    data: &mut [T],
    channels: usize,
    sample_rate: u32,
    freq: f32,
    phase: &Arc<Mutex<f32>>,
) where
    T: Sample + FromSample<f32>,
{
    let mut phase = match phase.try_lock() {
        Ok(phase) => phase,
        Err(_) => {
            data.fill(T::EQUILIBRIUM);
            return;
        }
    };
    let step = freq * std::f32::consts::TAU / sample_rate as f32;
    for frame in data.chunks_exact_mut(channels) {
        let sample = output_sample(phase.sin() * 0.2);
        *phase = (*phase + step) % std::f32::consts::TAU;
        frame[0] = sample;
        if channels > 1 {
            frame[1] = sample;
        }
        for extra in frame.iter_mut().skip(2) {
            *extra = T::EQUILIBRIUM;
        }
    }
}

#[derive(Default)]
struct OutputCallbackMetrics {
    callbacks: AtomicU64,
    frames: AtomicU64,
    lock_misses: AtomicU64,
    scratch_overflows: AtomicU64,
    ring_underruns: AtomicU64,
    ring_missing_frames: AtomicU64,
    ring_overflows: AtomicU64,
    output_queue_samples: AtomicU64,
    ring_ready: AtomicBool,
}

#[derive(Clone, Copy, Debug, Default)]
struct OutputCallbackSnapshot {
    callbacks: u64,
    frames: u64,
    lock_misses: u64,
    scratch_overflows: u64,
    ring_underruns: u64,
    ring_missing_frames: u64,
    ring_overflows: u64,
    output_queue_samples: u64,
}

impl OutputCallbackMetrics {
    fn record_callback(&self, frames: usize) {
        self.callbacks.fetch_add(1, Ordering::Relaxed);
        self.frames.fetch_add(frames as u64, Ordering::Relaxed);
    }

    fn record_scratch_overflow(&self) {
        self.scratch_overflows.fetch_add(1, Ordering::Relaxed);
    }

    fn record_ring_underrun(&self, missing_frames: usize) {
        if !self.ring_ready.load(Ordering::Relaxed) {
            return;
        }
        self.ring_underruns.fetch_add(1, Ordering::Relaxed);
        self.ring_missing_frames
            .fetch_add(missing_frames as u64, Ordering::Relaxed);
    }

    fn record_ring_overflow(&self, dropped_frames: usize) {
        self.ring_overflows
            .fetch_add(dropped_frames as u64, Ordering::Relaxed);
    }

    fn record_output_queue_samples(&self, samples: usize) {
        self.output_queue_samples
            .store(samples as u64, Ordering::Relaxed);
    }

    fn mark_ring_ready(&self) {
        self.ring_ready.store(true, Ordering::Relaxed);
    }

    fn snapshot(&self) -> OutputCallbackSnapshot {
        OutputCallbackSnapshot {
            callbacks: self.callbacks.load(Ordering::Relaxed),
            frames: self.frames.load(Ordering::Relaxed),
            lock_misses: self.lock_misses.load(Ordering::Relaxed),
            scratch_overflows: self.scratch_overflows.load(Ordering::Relaxed),
            ring_underruns: self.ring_underruns.load(Ordering::Relaxed),
            ring_missing_frames: self.ring_missing_frames.load(Ordering::Relaxed),
            ring_overflows: self.ring_overflows.load(Ordering::Relaxed),
            output_queue_samples: self.output_queue_samples.load(Ordering::Relaxed),
        }
    }
}

struct FeedbackSender {
    socket: UdpSocket,
    target: SocketAddr,
}

impl FeedbackSender {
    fn new(target: Option<SocketAddr>) -> Result<Option<Self>> {
        let Some(target) = target else {
            return Ok(None);
        };
        let bind_addr = if target.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = UdpSocket::bind(bind_addr)
            .with_context(|| format!("failed to bind feedback UDP socket to {bind_addr}"))?;
        println!("receiver: feedback_target={target}");
        Ok(Some(Self { socket, target }))
    }

    fn send(&self, status: &ReceiverStatus) {
        let bytes = status.to_bytes();
        if let Err(err) = self.socket.send_to(&bytes, self.target) {
            eprintln!("receiver: failed to send feedback status: {err}");
        }
    }
}

struct MetricsPrinter {
    interval: Duration,
    last: Instant,
    last_metrics: Option<JitterMetrics>,
    last_ingress: IngressSnapshot,
    callback_metrics: Option<Arc<OutputCallbackMetrics>>,
    last_callback_metrics: OutputCallbackSnapshot,
    feedback: Option<FeedbackSender>,
    output_sample_rate: u32,
    output_channels: u16,
    receiver_state: Arc<ReceiverState>,
    ingress_metrics: Arc<IngressMetrics>,
}

impl MetricsPrinter {
    fn new(
        interval_sec: f64,
        callback_metrics: Option<Arc<OutputCallbackMetrics>>,
        feedback: Option<FeedbackSender>,
        output_sample_rate: u32,
        output_channels: u16,
        receiver_state: Arc<ReceiverState>,
        ingress_metrics: Arc<IngressMetrics>,
    ) -> Self {
        Self {
            interval: Duration::from_secs_f64(interval_sec.max(0.1)),
            last: Instant::now(),
            last_metrics: None,
            last_ingress: IngressSnapshot::default(),
            callback_metrics,
            last_callback_metrics: OutputCallbackSnapshot::default(),
            feedback,
            output_sample_rate,
            output_channels,
            receiver_state,
            ingress_metrics,
        }
    }

    fn maybe_print(&mut self) {
        if self.last.elapsed() < self.interval {
            return;
        }
        let Some(snapshot) = self.receiver_state.snapshot() else {
            return;
        };
        let metrics = snapshot.metrics;
        let target_ms = snapshot.target_ms;
        let ingress = self.ingress_metrics.snapshot();

        let elapsed = self.last.elapsed().as_secs_f64().max(0.001);
        let previous = self.last_metrics.clone().unwrap_or_default();
        let callback_metrics = self
            .callback_metrics
            .as_ref()
            .map(|metrics| metrics.snapshot())
            .unwrap_or_default();
        let queue_drop_delta = ingress
            .queue_drops
            .saturating_sub(self.last_ingress.queue_drops);
        let queued_packet_delta = ingress
            .queued_packets
            .saturating_sub(self.last_ingress.queued_packets);
        let queued_invalid_delta = ingress
            .queued_invalid_packets
            .saturating_sub(self.last_ingress.queued_invalid_packets);
        let callback_delta = callback_metrics
            .callbacks
            .saturating_sub(self.last_callback_metrics.callbacks);
        let callback_frames_delta = callback_metrics
            .frames
            .saturating_sub(self.last_callback_metrics.frames);
        let lock_miss_delta = callback_metrics
            .lock_misses
            .saturating_sub(self.last_callback_metrics.lock_misses);
        let scratch_overflow_delta = callback_metrics
            .scratch_overflows
            .saturating_sub(self.last_callback_metrics.scratch_overflows);
        let ring_underrun_delta = callback_metrics
            .ring_underruns
            .saturating_sub(self.last_callback_metrics.ring_underruns);
        let ring_missing_delta = callback_metrics
            .ring_missing_frames
            .saturating_sub(self.last_callback_metrics.ring_missing_frames);
        let ring_overflow_delta = callback_metrics
            .ring_overflows
            .saturating_sub(self.last_callback_metrics.ring_overflows);
        let output_queue_ms = callback_metrics.output_queue_samples as f64 * 1000.0
            / self.output_sample_rate.max(1) as f64
            / self.output_channels.max(1) as f64;
        let output_queue_frames =
            callback_metrics.output_queue_samples / u64::from(self.output_channels.max(1));
        let output_queue_source_frames = ((output_queue_frames as f64 * SAMPLE_RATE as f64)
            / self.output_sample_rate.max(1) as f64)
            .round() as u64;
        let total_buffered_frames = metrics
            .buffer_level_frames
            .saturating_add(output_queue_source_frames);
        let total_buffered_ms = frames_to_ms_f64(total_buffered_frames, SAMPLE_RATE);
        let fixed_delay_frames = metrics.fixed_delay_frames;
        let fixed_delay_ms = frames_to_ms_f64(fixed_delay_frames, SAMPLE_RATE);
        let active_source = format_source(ingress.active_source);
        let active_stream = format_stream_id(ingress.active_stream_id);
        let foreign_source = format_source(ingress.foreign_source);
        let foreign_stream = format_stream_id(ingress.foreign_stream_id);
        println!(
            "receiver: state={:?} packets={:.1}/s queued={:.1}/s qdrop={:.1}/s qinvalid={:.1}/s loss={:.1}/s late={:.1}/s dup={:.1}/s ooo={:.1}/s foreign={:.1}/s src={} stream={} foreign_src={} foreign_stream={} buf={}fr/{:.1}ms fixed={}fr/{:.1}ms outq={}fr/{:.1}ms total_buf={}fr/{:.1}ms device_ratio={:.6} ratio={:.6} drift={:.1}ppm startup_under={} steady_under={} missing_calls={:.1}/s missing_frames={:.0}/s cb={:.1}/s out_frames={:.0}/s lock_miss={:.1}/s ring_under={:.1}/s ring_missing={:.0}/s ring_overflow={:.0}/s scratch_overflow={:.1}/s resyncs={} stream_resyncs={} underrun_resyncs={}",
            metrics.state,
            (metrics.received_packets - previous.received_packets) as f64 / elapsed,
            queued_packet_delta as f64 / elapsed,
            queue_drop_delta as f64 / elapsed,
            queued_invalid_delta as f64 / elapsed,
            (metrics.lost_packets - previous.lost_packets) as f64 / elapsed,
            (metrics.late_packets - previous.late_packets) as f64 / elapsed,
            (metrics.duplicate_packets - previous.duplicate_packets) as f64 / elapsed,
            (metrics.out_of_order_packets - previous.out_of_order_packets) as f64 / elapsed,
            (metrics.foreign_stream_packets - previous.foreign_stream_packets) as f64 / elapsed,
            active_source,
            active_stream,
            foreign_source,
            foreign_stream,
            metrics.buffer_level_frames,
            metrics.audio_latency_ms,
            fixed_delay_frames,
            fixed_delay_ms,
            output_queue_frames,
            output_queue_ms,
            total_buffered_frames,
            total_buffered_ms,
            metrics.device_resample_ratio,
            metrics.effective_resample_ratio,
            metrics.estimated_drift_ppm,
            metrics.startup_underruns,
            metrics.steady_underruns,
            (metrics.missing_frame_calls - previous.missing_frame_calls) as f64 / elapsed,
            (metrics.missing_frames - previous.missing_frames) as f64 / elapsed,
            callback_delta as f64 / elapsed,
            callback_frames_delta as f64 / elapsed,
            lock_miss_delta as f64 / elapsed,
            ring_underrun_delta as f64 / elapsed,
            ring_missing_delta as f64 / elapsed,
            ring_overflow_delta as f64 / elapsed,
            scratch_overflow_delta as f64 / elapsed,
            metrics.resyncs,
            metrics.resyncs_by_stream_change,
            metrics.resyncs_by_underrun
        );

        if let Some(feedback) = &self.feedback {
            feedback.send(&ReceiverStatus {
                stream_id: metrics.stream_id.unwrap_or_default(),
                latest_sequence: metrics.latest_sequence.unwrap_or_default(),
                target_ms,
                output_sample_rate: self.output_sample_rate,
                target_frames: fixed_delay_frames,
                fixed_delay_frames,
                received_packets: metrics.received_packets,
                steady_underruns: metrics.steady_underruns,
                startup_underruns: metrics.startup_underruns,
                callback_lock_misses: callback_metrics.lock_misses,
                resyncs: metrics.resyncs,
                scratch_overflows: callback_metrics.scratch_overflows,
                ring_underruns: callback_metrics.ring_underruns,
                ring_missing_frames: callback_metrics.ring_missing_frames,
                packet_queue_drops: ingress.queue_drops,
                audio_latency_frames: metrics.buffer_level_frames,
                output_queue_frames,
                total_buffered_frames,
                audio_latency_ms: metrics.audio_latency_ms,
                output_queue_ms: output_queue_ms as f32,
                total_buffered_ms: total_buffered_ms as f32,
                effective_ratio: metrics.effective_resample_ratio,
                receiver_time_ns: unix_time_ns(),
            });
        }

        self.last = Instant::now();
        self.last_metrics = Some(metrics);
        self.last_ingress = ingress;
        self.last_callback_metrics = callback_metrics;
    }
}

fn unix_time_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn format_source(source: Option<SocketAddr>) -> String {
    source
        .map(|source| source.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn format_stream_id(stream_id: Option<u64>) -> String {
    stream_id
        .map(|stream_id| format!("{stream_id:016x}"))
        .unwrap_or_else(|| "-".to_string())
}

fn select_output_device(host: &cpal::Host, filter: Option<&str>) -> Result<cpal::Device> {
    if let Some(filter) = filter {
        let filter = filter.to_lowercase();
        for device in host.output_devices()? {
            let name = device.to_string();
            if name.to_lowercase().contains(&filter) {
                return Ok(device);
            }
        }
        bail!("output device containing {filter:?} was not found");
    }

    host.default_output_device()
        .ok_or_else(|| anyhow!("default output device was not found"))
}

fn list_output_devices() -> Result<()> {
    let host = cpal::default_host();
    for (index, device) in host.output_devices()?.enumerate() {
        let name = device.to_string();
        let default = device
            .default_output_config()
            .map(|config| {
                format!(
                    "{}Hz/{}ch/{:?}",
                    config.sample_rate(),
                    config.channels(),
                    config.sample_format()
                )
            })
            .unwrap_or_else(|err| format!("no default config: {err}"));
        println!("{index}: {name} [{default}]");
    }
    Ok(())
}

fn create_wav_writer(path: &Path, sample_rate: u32) -> Result<WavWriter<BufWriter<File>>> {
    let spec = WavSpec {
        channels: CHANNELS,
        sample_rate,
        bits_per_sample: 16,
        sample_format: WavSampleFormat::Int,
    };
    Ok(WavWriter::create(path, spec)?)
}

fn duration_elapsed(start: Instant, duration_sec: Option<f64>) -> bool {
    duration_sec
        .map(|duration| start.elapsed() >= Duration::from_secs_f64(duration.max(0.0)))
        .unwrap_or(false)
}

fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        thread::sleep(deadline - now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_args() -> Args {
        Args {
            listen: "0.0.0.0:50000".parse().unwrap(),
            feedback_target: None,
            output: "null".to_string(),
            output_device: None,
            output_file: None,
            list_devices: false,
            test_tone: false,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            fixed_delay_frames: None,
            fixed_latency_ms: None,
            output_buffer_size_frames: None,
            output_ring_ms: 40,
            output_ring_capacity_ms: 200,
            render_chunk_ms: 5,
            socket_recv_buffer_bytes: 1_048_576,
            packet_queue_capacity: 2048,
            duration_sec: None,
            metrics_interval_sec: 1.0,
            freq: 440.0,
        }
    }

    #[test]
    fn default_fixed_delay_is_300ms_at_48k() {
        let args = default_args();
        let timing = ReceiverTiming::from_args(&args).unwrap();

        assert_eq!(timing.fixed_delay_frames, DEFAULT_FIXED_DELAY_FRAMES);
        assert_eq!(timing.target_buffer_ms, 300);
        assert_eq!(
            timing.capacity_ms,
            fixed_delay_capacity_ms(DEFAULT_FIXED_DELAY_FRAMES, SAMPLE_RATE)
        );
        validate_audio_args(&args, &timing).unwrap();
    }

    #[test]
    fn fixed_delay_frames_is_primary() {
        let mut args = default_args();
        args.fixed_delay_frames = Some(9_600);

        let timing = ReceiverTiming::from_args(&args).unwrap();

        assert_eq!(timing.fixed_delay_frames, 9_600);
        assert_eq!(timing.target_buffer_ms, 200);
        validate_audio_args(&args, &timing).unwrap();
    }

    #[test]
    fn fixed_latency_ms_is_alias_for_frames() {
        let mut args = default_args();
        args.fixed_latency_ms = Some(250);

        let timing = ReceiverTiming::from_args(&args).unwrap();

        assert_eq!(timing.fixed_delay_frames, 12_000);
        assert_eq!(timing.target_buffer_ms, 250);
        validate_audio_args(&args, &timing).unwrap();
    }

    #[test]
    fn fixed_delay_frames_and_ms_alias_cannot_be_combined() {
        let mut args = default_args();
        args.fixed_delay_frames = Some(9_600);
        args.fixed_latency_ms = Some(200);

        assert!(ReceiverTiming::from_args(&args).is_err());
    }
}
