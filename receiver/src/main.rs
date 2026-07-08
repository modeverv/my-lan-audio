mod audio_ring;

use anyhow::{anyhow, bail, Context, Result};
use audio_ring::SpscF32Ring;
use clap::{Parser, ValueEnum};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig, I24, U24,
};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use lan_audio_common::audio::{f32_to_i16, CHANNELS, SAMPLE_RATE};
use lan_audio_common::jitter::{JitterBuffer, JitterConfig, JitterMetrics};
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

const FIXED_LATENCY_MS: u32 = 500;
const FIXED_LATENCY_MAX_BUFFER_MS: u32 = 550;
const FIXED_LATENCY_CAPACITY_MS: u32 = 1500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum LatencyMode {
    Normal,
    Low,
    #[value(name = "fixed-500ms", alias = "fixed500")]
    Fixed500Ms,
}

impl LatencyMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Low => "low",
            Self::Fixed500Ms => "fixed-500ms",
        }
    }
}

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

    #[arg(long, default_value_t = 1000)]
    capacity_ms: u32,

    #[arg(long, default_value_t = 100)]
    target_buffer_ms: u32,

    #[arg(long, default_value_t = 300)]
    max_buffer_ms: u32,

    #[arg(long, default_value_t = 100)]
    start_threshold_ms: u32,

    #[arg(long, default_value_t = 5.0)]
    kp: f32,

    #[arg(long, default_value_t = 0.2)]
    ki: f32,

    #[arg(long, default_value_t = 0.05)]
    error_filter_alpha: f32,

    #[arg(long, default_value_t = 1000.0)]
    integral_limit_ms_sec: f32,

    #[arg(long, default_value_t = 1000.0)]
    max_ppm: f32,

    #[arg(long, default_value_t = 5000.0)]
    emergency_max_ppm: f32,

    #[arg(long)]
    no_adaptive_resampling: bool,

    #[arg(
        long,
        value_enum,
        default_value = "normal",
        help = "Receiver latency profile"
    )]
    latency_mode: LatencyMode,

    #[arg(long, help = "Alias for --latency-mode low")]
    low_latency: bool,

    #[arg(long, default_value_t = 10)]
    low_latency_trim_margin_ms: u32,

    #[arg(long, default_value_t = 10)]
    low_latency_trim_to_margin_ms: u32,

    #[arg(long, default_value_t = 1.5)]
    trim_crossfade_ms: f32,

    #[arg(long)]
    realtime_renderer: bool,

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
    latency_mode: LatencyMode,
    capacity_ms: u32,
    target_buffer_ms: u32,
    max_buffer_ms: u32,
    start_threshold_ms: u32,
    low_latency: bool,
    low_latency_trim_margin_ms: u32,
    low_latency_trim_to_margin_ms: u32,
    output_ring_ms: u32,
    output_ring_capacity_ms: u32,
    render_chunk_ms: u32,
    realtime_renderer: bool,
    output_buffer_size_frames: Option<u32>,
}

impl ReceiverTiming {
    fn from_args(args: &Args) -> Result<Self> {
        if args.low_latency && args.latency_mode != LatencyMode::Normal {
            bail!("--low-latency cannot be combined with --latency-mode");
        }

        let latency_mode = if args.low_latency {
            LatencyMode::Low
        } else {
            args.latency_mode
        };

        let timing = match latency_mode {
            LatencyMode::Normal => Self {
                latency_mode,
                capacity_ms: args.capacity_ms,
                target_buffer_ms: args.target_buffer_ms,
                max_buffer_ms: args.max_buffer_ms,
                start_threshold_ms: args.start_threshold_ms,
                low_latency: false,
                low_latency_trim_margin_ms: args.low_latency_trim_margin_ms,
                low_latency_trim_to_margin_ms: args.low_latency_trim_to_margin_ms,
                output_ring_ms: args.output_ring_ms,
                output_ring_capacity_ms: args.output_ring_capacity_ms,
                render_chunk_ms: args.render_chunk_ms,
                realtime_renderer: args.realtime_renderer,
                output_buffer_size_frames: args.output_buffer_size_frames,
            },
            LatencyMode::Low => Self {
                latency_mode,
                capacity_ms: args.capacity_ms,
                target_buffer_ms: args.target_buffer_ms,
                max_buffer_ms: args.max_buffer_ms,
                start_threshold_ms: args.start_threshold_ms,
                low_latency: true,
                low_latency_trim_margin_ms: args.low_latency_trim_margin_ms,
                low_latency_trim_to_margin_ms: args.low_latency_trim_to_margin_ms,
                output_ring_ms: args.output_ring_ms,
                output_ring_capacity_ms: args.output_ring_capacity_ms,
                render_chunk_ms: args.render_chunk_ms,
                realtime_renderer: true,
                output_buffer_size_frames: args.output_buffer_size_frames,
            },
            LatencyMode::Fixed500Ms => Self {
                latency_mode,
                capacity_ms: args.capacity_ms.max(FIXED_LATENCY_CAPACITY_MS),
                target_buffer_ms: FIXED_LATENCY_MS,
                max_buffer_ms: FIXED_LATENCY_MAX_BUFFER_MS,
                start_threshold_ms: FIXED_LATENCY_MS,
                low_latency: false,
                low_latency_trim_margin_ms: args.low_latency_trim_margin_ms,
                low_latency_trim_to_margin_ms: args.low_latency_trim_to_margin_ms,
                output_ring_ms: args.output_ring_ms,
                output_ring_capacity_ms: args.output_ring_capacity_ms,
                render_chunk_ms: args.render_chunk_ms,
                realtime_renderer: args.realtime_renderer,
                output_buffer_size_frames: args.output_buffer_size_frames,
            },
        };

        Ok(timing)
    }
}

fn validate_audio_args(args: &Args, timing: &ReceiverTiming) -> Result<()> {
    if args.sample_rate != SAMPLE_RATE {
        bail!("only 48000Hz packets are supported today");
    }
    if args.channels != CHANNELS {
        bail!("only stereo packets are supported today");
    }
    if timing.target_buffer_ms == 0 || timing.start_threshold_ms == 0 {
        bail!("buffer timing values must be greater than zero");
    }
    if timing.max_buffer_ms <= timing.target_buffer_ms {
        bail!("--max-buffer-ms must be greater than --target-buffer-ms");
    }
    if args.kp < 0.0 || args.ki < 0.0 {
        bail!("--kp and --ki must be zero or greater");
    }
    if !(0.0..=1.0).contains(&args.error_filter_alpha) {
        bail!("--error-filter-alpha must be between 0.0 and 1.0");
    }
    if args.integral_limit_ms_sec < 0.0 {
        bail!("--integral-limit-ms-sec must be zero or greater");
    }
    if args.output_buffer_size_frames == Some(0) {
        bail!("--output-buffer-size-frames must be greater than zero");
    }
    if timing.low_latency && timing.low_latency_trim_margin_ms == 0 {
        bail!("--low-latency-trim-margin-ms must be greater than zero");
    }
    if timing.low_latency_trim_to_margin_ms > timing.low_latency_trim_margin_ms {
        bail!("--low-latency-trim-to-margin-ms must be less than or equal to --low-latency-trim-margin-ms");
    }
    if args.trim_crossfade_ms < 0.0 {
        bail!("--trim-crossfade-ms must be zero or greater");
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
        "receiver: listening={} output={} output_file={:?} latency_mode={} target_buffer={}ms max_buffer={}ms",
        args.listen,
        output_mode(args),
        args.output_file,
        timing.latency_mode.as_str(),
        timing.target_buffer_ms,
        timing.max_buffer_ms
    );

    let jitter_config = JitterConfig {
        sample_rate: args.sample_rate,
        channels: args.channels,
        capacity_ms: timing.capacity_ms,
        target_ms: timing.target_buffer_ms,
        max_buffer_ms: timing.max_buffer_ms,
        start_threshold_ms: timing.start_threshold_ms,
        adaptive_resampling: !args.no_adaptive_resampling,
        kp: args.kp,
        ki: args.ki,
        error_filter_alpha: args.error_filter_alpha,
        integral_limit_ms_sec: args.integral_limit_ms_sec,
        max_ppm: args.max_ppm,
        emergency_max_ppm: args.emergency_max_ppm,
        low_latency: timing.low_latency,
        low_latency_trim_margin_ms: timing.low_latency_trim_margin_ms,
        low_latency_trim_to_margin_ms: timing.low_latency_trim_to_margin_ms,
        trim_crossfade_ms: args.trim_crossfade_ms,
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
    Packet(AudioPacket, Instant),
    InvalidPacket,
}

#[derive(Default)]
struct IngressMetrics {
    queued_packets: AtomicU64,
    queued_invalid_packets: AtomicU64,
    queue_drops: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default)]
struct IngressSnapshot {
    queued_packets: u64,
    queued_invalid_packets: u64,
    queue_drops: u64,
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

    fn snapshot(&self) -> IngressSnapshot {
        IngressSnapshot {
            queued_packets: self.queued_packets.load(Ordering::Relaxed),
            queued_invalid_packets: self.queued_invalid_packets.load(Ordering::Relaxed),
            queue_drops: self.queue_drops.load(Ordering::Relaxed),
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
                Ok((len, _addr)) => {
                    let arrival = Instant::now();
                    match AudioPacket::from_bytes(&buf[..len]) {
                        Ok(packet) => {
                            send_receiver_event(
                                &event_tx,
                                ReceiverEvent::Packet(packet, arrival),
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

fn drain_receiver_events(event_rx: &Receiver<ReceiverEvent>, jitter: &mut JitterBuffer) {
    while let Ok(event) = event_rx.try_recv() {
        match event {
            ReceiverEvent::Packet(packet, arrival) => {
                jitter.insert_packet(packet, arrival);
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

        drain_receiver_events(&event_rx, &mut jitter);
        jitter.pull_f32(&mut output);
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
        RingRendererConfig {
            output_sample_rate: config.sample_rate,
            channels,
            output_ring_ms: timing.output_ring_ms,
            render_chunk_ms: timing.render_chunk_ms,
            low_latency: timing.low_latency,
            realtime_priority: timing.low_latency || timing.realtime_renderer,
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
    low_latency: bool,
    realtime_priority: bool,
}

fn spawn_ring_renderer(
    jitter_config: JitterConfig,
    event_rx: Receiver<ReceiverEvent>,
    ring: Arc<SpscF32Ring>,
    metrics: Arc<OutputCallbackMetrics>,
    receiver_state: Arc<ReceiverState>,
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
            let sleep_divisor = if config.low_latency { 4 } else { 2 };
            let min_sleep_us = if config.low_latency { 500 } else { 1_000 };
            let sleep_duration = Duration::from_micros(
                ((config.render_chunk_ms as u64 * 1000) / sleep_divisor).max(min_sleep_us),
            );

            loop {
                drain_receiver_events(&event_rx, &mut jitter);
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

                    jitter.pull_f32_at_sample_rate(scratch, config.output_sample_rate);

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
        let trimmed_frames = metrics.trimmed_frames - previous.trimmed_frames;
        let trimmed_ms_per_sec = trimmed_frames as f64 * 1000.0 / SAMPLE_RATE as f64 / elapsed;
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
        println!(
            "receiver: state={:?} packets={:.1}/s queued={:.1}/s qdrop={:.1}/s qinvalid={:.1}/s loss={:.1}/s late={:.1}/s dup={:.1}/s ooo={:.1}/s latency={:.1}ms target={}ms outq={:.1}ms err={:.1}ms filt={:.1}ms device_ratio={:.6} corr={:.1}ppm int={:.1}ppm ratio={:.6} drift={:.1}ppm startup_under={} steady_under={} missing_calls={:.1}/s missing_frames={:.0}/s cb={:.1}/s out_frames={:.0}/s lock_miss={:.1}/s ring_under={:.1}/s ring_missing={:.0}/s ring_overflow={:.0}/s scratch_overflow={:.1}/s trims={} trim={:.1}ms/s resyncs={} stream_resyncs={} underrun_resyncs={}",
            metrics.state,
            (metrics.received_packets - previous.received_packets) as f64 / elapsed,
            queued_packet_delta as f64 / elapsed,
            queue_drop_delta as f64 / elapsed,
            queued_invalid_delta as f64 / elapsed,
            (metrics.lost_packets - previous.lost_packets) as f64 / elapsed,
            (metrics.late_packets - previous.late_packets) as f64 / elapsed,
            (metrics.duplicate_packets - previous.duplicate_packets) as f64 / elapsed,
            (metrics.out_of_order_packets - previous.out_of_order_packets) as f64 / elapsed,
            metrics.audio_latency_ms,
            target_ms,
            output_queue_ms,
            metrics.buffer_error_ms,
            metrics.filtered_error_ms,
            metrics.device_resample_ratio,
            metrics.correction_ppm,
            metrics.integral_correction_ppm,
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
            metrics.latency_trims,
            trimmed_ms_per_sec,
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
                received_packets: metrics.received_packets,
                steady_underruns: metrics.steady_underruns,
                startup_underruns: metrics.startup_underruns,
                callback_lock_misses: callback_metrics.lock_misses,
                latency_trims: metrics.latency_trims,
                resyncs: metrics.resyncs,
                scratch_overflows: callback_metrics.scratch_overflows,
                ring_underruns: callback_metrics.ring_underruns,
                ring_missing_frames: callback_metrics.ring_missing_frames,
                packet_queue_drops: ingress.queue_drops,
                audio_latency_ms: metrics.audio_latency_ms,
                output_queue_ms: output_queue_ms as f32,
                correction_ppm: metrics.correction_ppm,
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
            capacity_ms: 1000,
            target_buffer_ms: 100,
            max_buffer_ms: 300,
            start_threshold_ms: 100,
            kp: 5.0,
            ki: 0.2,
            error_filter_alpha: 0.05,
            integral_limit_ms_sec: 1000.0,
            max_ppm: 1000.0,
            emergency_max_ppm: 5000.0,
            no_adaptive_resampling: false,
            latency_mode: LatencyMode::Normal,
            low_latency: false,
            low_latency_trim_margin_ms: 10,
            low_latency_trim_to_margin_ms: 10,
            trim_crossfade_ms: 1.5,
            realtime_renderer: false,
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
    fn fixed_500ms_mode_overrides_jitter_buffer_timing() {
        let mut args = default_args();
        args.latency_mode = LatencyMode::Fixed500Ms;
        args.capacity_ms = 1000;
        args.target_buffer_ms = 20;
        args.start_threshold_ms = 20;
        args.max_buffer_ms = 60;
        args.low_latency = false;

        let timing = ReceiverTiming::from_args(&args).unwrap();

        assert_eq!(timing.latency_mode, LatencyMode::Fixed500Ms);
        assert_eq!(timing.capacity_ms, FIXED_LATENCY_CAPACITY_MS);
        assert_eq!(timing.target_buffer_ms, FIXED_LATENCY_MS);
        assert_eq!(timing.start_threshold_ms, FIXED_LATENCY_MS);
        assert_eq!(timing.max_buffer_ms, FIXED_LATENCY_MAX_BUFFER_MS);
        assert!(!timing.low_latency);
        validate_audio_args(&args, &timing).unwrap();
    }

    #[test]
    fn low_latency_flag_still_selects_low_latency_mode() {
        let mut args = default_args();
        args.low_latency = true;

        let timing = ReceiverTiming::from_args(&args).unwrap();

        assert_eq!(timing.latency_mode, LatencyMode::Low);
        assert!(timing.low_latency);
        assert!(timing.realtime_renderer);
    }

    #[test]
    fn low_latency_flag_cannot_be_combined_with_explicit_mode() {
        let mut args = default_args();
        args.low_latency = true;
        args.latency_mode = LatencyMode::Fixed500Ms;

        assert!(ReceiverTiming::from_args(&args).is_err());
    }
}
