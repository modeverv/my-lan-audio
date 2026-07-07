use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, Stream, StreamConfig};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use lan_audio_common::audio::{f32_to_i16, CHANNELS, SAMPLE_RATE};
use lan_audio_common::jitter::{JitterBuffer, JitterConfig, JitterMetrics};
use lan_audio_common::packet::AudioPacket;
use socket2::{Domain, Protocol, Socket, Type};
use std::fs::File;
use std::io::BufWriter;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(about = "LAN audio UDP receiver")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:50000")]
    listen: SocketAddr,

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

    #[arg(long, default_value_t = 100.0)]
    max_ppm: f32,

    #[arg(long, default_value_t = 500.0)]
    emergency_max_ppm: f32,

    #[arg(long)]
    no_adaptive_resampling: bool,

    #[arg(long, default_value_t = 1_048_576)]
    socket_recv_buffer_bytes: usize,

    #[arg(long)]
    duration_sec: Option<f64>,

    #[arg(long, default_value_t = 1.0)]
    metrics_interval_sec: f64,

    #[arg(long, default_value_t = 440.0)]
    freq: f32,
}

fn main() -> Result<()> {
    let args = Args::parse();
    validate_audio_args(&args)?;

    if args.list_devices {
        return list_output_devices();
    }

    if args.test_tone {
        return run_test_tone(&args);
    }

    run_receiver(&args)
}

fn validate_audio_args(args: &Args) -> Result<()> {
    if args.sample_rate != SAMPLE_RATE {
        bail!("only 48000Hz packets are supported today");
    }
    if args.channels != CHANNELS {
        bail!("only stereo packets are supported today");
    }
    if args.target_buffer_ms == 0 || args.start_threshold_ms == 0 {
        bail!("buffer timing values must be greater than zero");
    }
    if args.max_buffer_ms <= args.target_buffer_ms {
        bail!("--max-buffer-ms must be greater than --target-buffer-ms");
    }
    Ok(())
}

fn run_receiver(args: &Args) -> Result<()> {
    let socket = bind_socket(args.listen, args.socket_recv_buffer_bytes)?;
    println!(
        "receiver: listening={} output={} output_file={:?} target_buffer={}ms",
        args.listen,
        output_mode(args),
        args.output_file,
        args.target_buffer_ms
    );

    let jitter = Arc::new(Mutex::new(JitterBuffer::new(JitterConfig {
        sample_rate: args.sample_rate,
        channels: args.channels,
        capacity_ms: args.capacity_ms,
        target_ms: args.target_buffer_ms,
        max_buffer_ms: args.max_buffer_ms,
        start_threshold_ms: args.start_threshold_ms,
        adaptive_resampling: !args.no_adaptive_resampling,
        kp: args.kp,
        max_ppm: args.max_ppm,
        emergency_max_ppm: args.emergency_max_ppm,
    })));

    spawn_udp_receiver(socket, Arc::clone(&jitter));

    match output_mode(args).as_str() {
        "audio" => run_audio_output(args, jitter),
        "wav" => run_timed_pull_output(args, jitter, args.output_file.as_deref()),
        "null" => run_timed_pull_output(args, jitter, None),
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

fn spawn_udp_receiver(socket: UdpSocket, jitter: Arc<Mutex<JitterBuffer>>) {
    thread::spawn(move || {
        let mut buf = vec![0u8; 2048];
        loop {
            match socket.recv_from(&mut buf) {
                Ok((len, _addr)) => {
                    let arrival = Instant::now();
                    match AudioPacket::from_bytes(&buf[..len]) {
                        Ok(packet) => {
                            if let Ok(mut jitter) = jitter.lock() {
                                jitter.insert_packet(packet, arrival);
                            }
                        }
                        Err(err) => {
                            eprintln!("receiver: invalid packet: {err}");
                            if let Ok(mut jitter) = jitter.lock() {
                                jitter.record_invalid_packet();
                            }
                        }
                    }
                }
                Err(err) => eprintln!("receiver: UDP receive error: {err}"),
            }
        }
    });
}

fn run_timed_pull_output(
    args: &Args,
    jitter: Arc<Mutex<JitterBuffer>>,
    output_file: Option<&Path>,
) -> Result<()> {
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
    let mut metrics = MetricsPrinter::new(args.metrics_interval_sec, None);
    let mut next_tick = Instant::now();
    let start = Instant::now();

    loop {
        if duration_elapsed(start, args.duration_sec) {
            if let Some(writer) = writer {
                writer.finalize().context("failed to finalize WAV output")?;
            }
            return Ok(());
        }

        let mut output = vec![0.0f32; chunk_frames * channels];
        if let Ok(mut jitter) = jitter.lock() {
            jitter.pull_f32(&mut output);
        }

        if let Some(writer) = writer.as_mut() {
            for sample in &output {
                writer.write_sample(f32_to_i16(*sample))?;
            }
        }

        metrics.maybe_print(&jitter);
        sleep_until(next_tick);
        next_tick += Duration::from_millis(10);
    }
}

fn run_audio_output(args: &Args, jitter: Arc<Mutex<JitterBuffer>>) -> Result<()> {
    let host = cpal::default_host();
    let device = select_output_device(&host, args.output_device.as_deref())?;
    let name = device.to_string();
    let supported = device
        .default_output_config()
        .context("failed to get default output config")?;
    let sample_format = supported.sample_format();
    let mut config = supported.config();
    config.channels = args.channels;

    println!(
        "receiver: output_device=\"{}\" output_format={}Hz/{}ch/{:?}",
        name, config.sample_rate, config.channels, sample_format
    );

    let callback_metrics = Arc::new(OutputCallbackMetrics::default());
    let stream = build_jitter_output_stream(
        &device,
        sample_format,
        &config,
        jitter.clone(),
        Arc::clone(&callback_metrics),
    )?;
    stream.play().context("failed to start output stream")?;

    let mut metrics = MetricsPrinter::new(args.metrics_interval_sec, Some(callback_metrics));
    let start = Instant::now();
    loop {
        if duration_elapsed(start, args.duration_sec) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
        metrics.maybe_print(&jitter);
    }
}

fn build_jitter_output_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    jitter: Arc<Mutex<JitterBuffer>>,
    callback_metrics: Arc<OutputCallbackMetrics>,
) -> Result<Stream> {
    let err_fn = |err| eprintln!("audio output stream error: {err}");
    let output_sample_rate = config.sample_rate;
    let channels = usize::from(config.channels.max(1));
    let stream = match sample_format {
        SampleFormat::F32 => {
            let jitter = jitter.clone();
            let callback_metrics = Arc::clone(&callback_metrics);
            device.build_output_stream(
                *config,
                move |data: &mut [f32], _| {
                    callback_metrics.record_callback(data.len() / channels);
                    if let Ok(mut jitter) = jitter.try_lock() {
                        jitter.pull_f32_at_sample_rate(data, output_sample_rate);
                    } else {
                        callback_metrics.record_lock_miss();
                        data.fill(0.0);
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let jitter = jitter.clone();
            let callback_metrics = Arc::clone(&callback_metrics);
            let mut scratch = vec![0.0f32; scratch_len_for_stream(config)];
            device.build_output_stream(
                *config,
                move |data: &mut [i16], _| {
                    callback_metrics.record_callback(data.len() / channels);
                    if let Ok(mut jitter) = jitter.try_lock() {
                        if data.len() > scratch.len() {
                            callback_metrics.record_scratch_overflow();
                            data.fill(0);
                            return;
                        }
                        let scratch = &mut scratch[..data.len()];
                        jitter.pull_f32_at_sample_rate(scratch, output_sample_rate);
                        for (dst, src) in data.iter_mut().zip(scratch.iter().copied()) {
                            *dst = f32_to_i16(src);
                        }
                    } else {
                        callback_metrics.record_lock_miss();
                        data.fill(0);
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let jitter = jitter.clone();
            let callback_metrics = Arc::clone(&callback_metrics);
            let mut scratch = vec![0.0f32; scratch_len_for_stream(config)];
            device.build_output_stream(
                *config,
                move |data: &mut [u16], _| {
                    callback_metrics.record_callback(data.len() / channels);
                    if let Ok(mut jitter) = jitter.try_lock() {
                        if data.len() > scratch.len() {
                            callback_metrics.record_scratch_overflow();
                            data.fill(u16::MAX / 2);
                            return;
                        }
                        let scratch = &mut scratch[..data.len()];
                        jitter.pull_f32_at_sample_rate(scratch, output_sample_rate);
                        for (dst, src) in data.iter_mut().zip(scratch.iter().copied()) {
                            *dst = f32_to_u16(src);
                        }
                    } else {
                        callback_metrics.record_lock_miss();
                        data.fill(u16::MAX / 2);
                    }
                },
                err_fn,
                None,
            )?
        }
        other => bail!("unsupported output sample format {other:?}"),
    };
    Ok(stream)
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
    let sample_rate = config.sample_rate;
    let channels = config.channels as usize;
    let err_fn = |err| eprintln!("audio output stream error: {err}");
    let stream = match sample_format {
        SampleFormat::F32 => {
            let phase = phase.clone();
            device.build_output_stream(
                *config,
                move |data: &mut [f32], _| fill_tone_f32(data, channels, sample_rate, freq, &phase),
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let phase = phase.clone();
            device.build_output_stream(
                *config,
                move |data: &mut [i16], _| {
                    let mut tmp = vec![0.0f32; data.len()];
                    fill_tone_f32(&mut tmp, channels, sample_rate, freq, &phase);
                    for (dst, src) in data.iter_mut().zip(tmp) {
                        *dst = f32_to_i16(src);
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let phase = phase.clone();
            device.build_output_stream(
                *config,
                move |data: &mut [u16], _| {
                    let mut tmp = vec![0.0f32; data.len()];
                    fill_tone_f32(&mut tmp, channels, sample_rate, freq, &phase);
                    for (dst, src) in data.iter_mut().zip(tmp) {
                        *dst = f32_to_u16(src);
                    }
                },
                err_fn,
                None,
            )?
        }
        other => bail!("unsupported output sample format {other:?}"),
    };
    Ok(stream)
}

fn fill_tone_f32(
    data: &mut [f32],
    channels: usize,
    sample_rate: u32,
    freq: f32,
    phase: &Arc<Mutex<f32>>,
) {
    let mut phase = match phase.try_lock() {
        Ok(phase) => phase,
        Err(_) => {
            data.fill(0.0);
            return;
        }
    };
    let step = freq * std::f32::consts::TAU / sample_rate as f32;
    for frame in data.chunks_exact_mut(channels) {
        let sample = phase.sin() * 0.2;
        *phase = (*phase + step) % std::f32::consts::TAU;
        frame[0] = sample;
        if channels > 1 {
            frame[1] = sample;
        }
        for extra in frame.iter_mut().skip(2) {
            *extra = 0.0;
        }
    }
}

#[derive(Default)]
struct OutputCallbackMetrics {
    callbacks: AtomicU64,
    frames: AtomicU64,
    lock_misses: AtomicU64,
    scratch_overflows: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default)]
struct OutputCallbackSnapshot {
    callbacks: u64,
    frames: u64,
    lock_misses: u64,
    scratch_overflows: u64,
}

impl OutputCallbackMetrics {
    fn record_callback(&self, frames: usize) {
        self.callbacks.fetch_add(1, Ordering::Relaxed);
        self.frames.fetch_add(frames as u64, Ordering::Relaxed);
    }

    fn record_lock_miss(&self) {
        self.lock_misses.fetch_add(1, Ordering::Relaxed);
    }

    fn record_scratch_overflow(&self) {
        self.scratch_overflows.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> OutputCallbackSnapshot {
        OutputCallbackSnapshot {
            callbacks: self.callbacks.load(Ordering::Relaxed),
            frames: self.frames.load(Ordering::Relaxed),
            lock_misses: self.lock_misses.load(Ordering::Relaxed),
            scratch_overflows: self.scratch_overflows.load(Ordering::Relaxed),
        }
    }
}

struct MetricsPrinter {
    interval: Duration,
    last: Instant,
    last_metrics: Option<JitterMetrics>,
    callback_metrics: Option<Arc<OutputCallbackMetrics>>,
    last_callback_metrics: OutputCallbackSnapshot,
}

impl MetricsPrinter {
    fn new(interval_sec: f64, callback_metrics: Option<Arc<OutputCallbackMetrics>>) -> Self {
        Self {
            interval: Duration::from_secs_f64(interval_sec.max(0.1)),
            last: Instant::now(),
            last_metrics: None,
            callback_metrics,
            last_callback_metrics: OutputCallbackSnapshot::default(),
        }
    }

    fn maybe_print(&mut self, jitter: &Arc<Mutex<JitterBuffer>>) {
        if self.last.elapsed() < self.interval {
            return;
        }
        let Ok(jitter) = jitter.try_lock() else {
            return;
        };
        let metrics = jitter.metrics();
        let target_ms = jitter.target_ms();
        drop(jitter);

        let elapsed = self.last.elapsed().as_secs_f64().max(0.001);
        let previous = self.last_metrics.clone().unwrap_or_default();
        let callback_metrics = self
            .callback_metrics
            .as_ref()
            .map(|metrics| metrics.snapshot())
            .unwrap_or_default();
        let trimmed_frames = metrics.trimmed_frames - previous.trimmed_frames;
        let trimmed_ms_per_sec = trimmed_frames as f64 * 1000.0 / SAMPLE_RATE as f64 / elapsed;
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
        println!(
            "receiver: state={:?} packets={:.1}/s loss={:.1}/s late={:.1}/s dup={:.1}/s ooo={:.1}/s latency={:.1}ms target={}ms device_ratio={:.6} corr={:.1}ppm ratio={:.6} drift={:.1}ppm startup_under={} steady_under={} missing_calls={:.1}/s missing_frames={:.0}/s cb={:.1}/s out_frames={:.0}/s lock_miss={:.1}/s scratch_overflow={:.1}/s trims={} trim={:.1}ms/s resyncs={} stream_resyncs={} underrun_resyncs={}",
            metrics.state,
            (metrics.received_packets - previous.received_packets) as f64 / elapsed,
            (metrics.lost_packets - previous.lost_packets) as f64 / elapsed,
            (metrics.late_packets - previous.late_packets) as f64 / elapsed,
            (metrics.duplicate_packets - previous.duplicate_packets) as f64 / elapsed,
            (metrics.out_of_order_packets - previous.out_of_order_packets) as f64 / elapsed,
            metrics.audio_latency_ms,
            target_ms,
            metrics.device_resample_ratio,
            metrics.correction_ppm,
            metrics.effective_resample_ratio,
            metrics.estimated_drift_ppm,
            metrics.startup_underruns,
            metrics.steady_underruns,
            (metrics.missing_frame_calls - previous.missing_frame_calls) as f64 / elapsed,
            (metrics.missing_frames - previous.missing_frames) as f64 / elapsed,
            callback_delta as f64 / elapsed,
            callback_frames_delta as f64 / elapsed,
            lock_miss_delta as f64 / elapsed,
            scratch_overflow_delta as f64 / elapsed,
            metrics.latency_trims,
            trimmed_ms_per_sec,
            metrics.resyncs,
            metrics.resyncs_by_stream_change,
            metrics.resyncs_by_underrun
        );

        self.last = Instant::now();
        self.last_metrics = Some(metrics);
        self.last_callback_metrics = callback_metrics;
    }
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

fn f32_to_u16(sample: f32) -> u16 {
    (((sample.clamp(-1.0, 1.0) + 1.0) * 0.5) * u16::MAX as f32).round() as u16
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
