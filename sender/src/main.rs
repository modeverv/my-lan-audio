use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig, I24, U24};
use hound::{SampleFormat as WavSampleFormat, WavReader, WavSpec, WavWriter};
use lan_audio_common::audio::{
    f32_to_i16, i16_to_f32, rms_db, stereo_to_i16_interleaved, StereoFrame, CHANNELS, SAMPLE_RATE,
};
use lan_audio_common::packet::{AudioPacket, AudioPacketHeader};
use lan_audio_common::resampler::{resample_linear, StreamingLinearResampler};
use lan_audio_common::status::ReceiverStatus;
use rand::Rng;
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufWriter;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Parser, Debug)]
#[command(about = "LAN audio UDP sender")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:50000")]
    target: SocketAddr,

    #[arg(long, default_value = "0.0.0.0:0")]
    bind: SocketAddr,

    #[arg(long)]
    feedback_listen: Option<SocketAddr>,

    #[arg(long, default_value = "sine", value_parser = ["dummy", "sine", "capture"])]
    input: String,

    #[arg(long)]
    input_file: Option<PathBuf>,

    #[arg(long)]
    device: Option<String>,

    #[arg(long)]
    list_devices: bool,

    #[arg(long)]
    meter_only: bool,

    #[arg(long)]
    output_file: Option<PathBuf>,

    #[arg(long, default_value_t = SAMPLE_RATE)]
    sample_rate: u32,

    #[arg(long, default_value_t = CHANNELS)]
    channels: u16,

    #[arg(long, default_value_t = 5.0)]
    packet_ms: f64,

    #[arg(long, default_value_t = 440.0)]
    freq: f32,

    #[arg(long)]
    duration_sec: Option<f64>,

    #[arg(long)]
    loop_input: bool,

    #[arg(long, default_value_t = 0.0)]
    drop_rate: f64,

    #[arg(long, default_value_t = 0.0)]
    jitter_ms: f64,

    #[arg(long, default_value_t = 0.0)]
    reorder_rate: f64,

    #[arg(long, default_value_t = 0.0)]
    drift_ppm: f64,

    #[arg(long)]
    sender_side_asrc: bool,

    #[arg(long, default_value_t = 40.0)]
    sender_asrc_kp: f64,

    #[arg(long, default_value_t = 1000.0)]
    sender_asrc_max_ppm: f64,

    #[arg(long, default_value_t = 1.0)]
    metrics_interval_sec: f64,
}

#[derive(Debug, Default)]
struct SendStats {
    sent_packets: u64,
    sent_bytes: u64,
    dropped_packets: u64,
    send_errors: u64,
}

type SharedReceiverStatus = Arc<Mutex<Option<ReceiverStatus>>>;

struct PacketSender {
    socket: UdpSocket,
    target: SocketAddr,
    stream_id: u64,
    sequence: u32,
    sample_position: u64,
    start: Instant,
    stats: SendStats,
    pending_reorder: VecDeque<Vec<u8>>,
    drop_rate: f64,
    jitter_ms: f64,
    reorder_rate: f64,
}

impl PacketSender {
    fn new(args: &Args) -> Result<Self> {
        let socket = UdpSocket::bind(args.bind)
            .with_context(|| format!("failed to bind UDP socket to {}", args.bind))?;

        Ok(Self {
            socket,
            target: args.target,
            stream_id: new_stream_id(),
            sequence: 0,
            sample_position: 0,
            start: Instant::now(),
            stats: SendStats::default(),
            pending_reorder: VecDeque::new(),
            drop_rate: args.drop_rate.clamp(0.0, 1.0),
            jitter_ms: args.jitter_ms.max(0.0),
            reorder_rate: args.reorder_rate.clamp(0.0, 1.0),
        })
    }

    fn send_frames(&mut self, frames: &[StereoFrame]) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let payload = stereo_to_i16_interleaved(frames);
        let header = AudioPacketHeader::new(
            self.stream_id,
            self.sequence,
            frames.len() as u16,
            self.sample_position,
            self.start.elapsed().as_nanos() as u64,
        );
        let packet = AudioPacket::new(header, payload)?;
        self.sequence = self.sequence.wrapping_add(1);
        self.sample_position += frames.len() as u64;
        self.send_bytes(packet.to_bytes())
    }

    fn flush(&mut self) -> Result<()> {
        while let Some(bytes) = self.pending_reorder.pop_front() {
            self.send_now(&bytes)?;
        }
        Ok(())
    }

    fn send_bytes(&mut self, bytes: Vec<u8>) -> Result<()> {
        let mut rng = rand::thread_rng();
        if rng.gen::<f64>() < self.drop_rate {
            self.stats.dropped_packets += 1;
            return Ok(());
        }

        if self.jitter_ms > 0.0 {
            let delay_ms = rng.gen_range(0.0..=self.jitter_ms);
            thread::sleep(Duration::from_secs_f64(delay_ms / 1000.0));
        }

        if rng.gen::<f64>() < self.reorder_rate {
            if let Some(previous) = self.pending_reorder.pop_front() {
                self.send_now(&bytes)?;
                self.send_now(&previous)?;
            } else {
                self.pending_reorder.push_back(bytes);
            }
            return Ok(());
        }

        if let Some(previous) = self.pending_reorder.pop_front() {
            self.send_now(&previous)?;
        }
        self.send_now(&bytes)
    }

    fn send_now(&mut self, bytes: &[u8]) -> Result<()> {
        match self.socket.send_to(bytes, self.target) {
            Ok(sent) => {
                self.stats.sent_packets += 1;
                self.stats.sent_bytes += sent as u64;
                Ok(())
            }
            Err(err) => {
                self.stats.send_errors += 1;
                Err(err).with_context(|| format!("failed to send UDP packet to {}", self.target))
            }
        }
    }
}

struct MetricsPrinter {
    interval: Duration,
    last: Instant,
    last_packets: u64,
    last_bytes: u64,
    remote_status: Option<SharedReceiverStatus>,
}

impl MetricsPrinter {
    fn new(interval_sec: f64, remote_status: Option<SharedReceiverStatus>) -> Self {
        Self {
            interval: Duration::from_secs_f64(interval_sec.max(0.1)),
            last: Instant::now(),
            last_packets: 0,
            last_bytes: 0,
            remote_status,
        }
    }

    fn maybe_print(
        &mut self,
        sender: &PacketSender,
        label: &str,
        rms: (f32, f32),
        buffer_ms: f32,
        send_rate_ppm: f64,
    ) {
        if self.last.elapsed() < self.interval {
            return;
        }

        let elapsed = self.last.elapsed().as_secs_f64().max(0.001);
        let packets = sender.stats.sent_packets - self.last_packets;
        let bytes = sender.stats.sent_bytes - self.last_bytes;
        let bitrate_mbps = bytes as f64 * 8.0 / elapsed / 1_000_000.0;
        let remote_suffix = self.remote_suffix();
        println!(
            "sender: input={label} packets={:.1}/s bitrate={:.3}Mbps sequence={} sample_position={} capture_buffer={:.1}ms rms={:.1}/{:.1}dB dropped={} errors={} send_corr={:.1}ppm{}",
            packets as f64 / elapsed,
            bitrate_mbps,
            sender.sequence,
            sender.sample_position,
            buffer_ms,
            rms.0,
            rms.1,
            sender.stats.dropped_packets,
            sender.stats.send_errors,
            send_rate_ppm,
            remote_suffix
        );

        self.last = Instant::now();
        self.last_packets = sender.stats.sent_packets;
        self.last_bytes = sender.stats.sent_bytes;
    }

    fn remote_suffix(&self) -> String {
        let Some(remote_status) = &self.remote_status else {
            return String::new();
        };
        let Ok(status) = remote_status.try_lock() else {
            return " remote_status=busy".to_string();
        };
        let Some(status) = status.as_ref() else {
            return " remote_status=waiting".to_string();
        };
        format!(
            " remote_latency={:.1}ms remote_outq={:.1}ms remote_target={}ms remote_steady_under={} remote_ring_under={} remote_qdrop={} remote_lock_miss={} remote_trims={} remote_resyncs={} remote_ratio={:.6}",
            status.audio_latency_ms,
            status.output_queue_ms,
            status.target_ms,
            status.steady_underruns,
            status.ring_underruns,
            status.packet_queue_drops,
            status.callback_lock_misses,
            status.latency_trims,
            status.resyncs,
            status.effective_ratio
        )
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    validate_audio_args(&args)?;

    if args.list_devices {
        return list_input_devices();
    }

    if args.meter_only || args.output_file.is_some() {
        return run_capture_monitor(&args);
    }

    let remote_status = spawn_feedback_listener(args.feedback_listen)?;

    if let Some(path) = &args.input_file {
        return run_file_sender(&args, path, remote_status);
    }

    match args.input.as_str() {
        "dummy" => run_generated_sender(&args, GeneratedInput::Dummy, remote_status),
        "sine" => run_generated_sender(&args, GeneratedInput::Sine, remote_status),
        "capture" => run_capture_sender(&args, remote_status),
        other => bail!("unsupported input mode {other}"),
    }
}

fn validate_audio_args(args: &Args) -> Result<()> {
    if args.sample_rate != SAMPLE_RATE {
        bail!("only 48000Hz is supported by the packet format today");
    }
    if args.channels != CHANNELS {
        bail!("only stereo output is supported by the packet format today");
    }
    if args.packet_ms <= 0.0 {
        bail!("--packet-ms must be greater than zero");
    }
    if args.sender_asrc_kp < 0.0 || args.sender_asrc_max_ppm < 0.0 {
        bail!("--sender-asrc-kp and --sender-asrc-max-ppm must be zero or greater");
    }
    let packet_frames = packet_frames(args)?;
    if packet_frames > u16::MAX as usize {
        bail!("packet has too many frames for the protocol header");
    }
    Ok(())
}

fn spawn_feedback_listener(listen: Option<SocketAddr>) -> Result<Option<SharedReceiverStatus>> {
    let Some(listen) = listen else {
        return Ok(None);
    };
    let socket = UdpSocket::bind(listen)
        .with_context(|| format!("failed to bind feedback UDP socket to {listen}"))?;
    println!("sender: feedback_listening={listen}");

    let status = Arc::new(Mutex::new(None));
    let shared_status = Arc::clone(&status);
    thread::spawn(move || {
        let mut buf = [0u8; 256];
        loop {
            match socket.recv_from(&mut buf) {
                Ok((len, _addr)) => match ReceiverStatus::from_bytes(&buf[..len]) {
                    Ok(remote) => {
                        if let Ok(mut status) = shared_status.lock() {
                            *status = Some(remote);
                        }
                    }
                    Err(err) => eprintln!("sender: invalid feedback status: {err}"),
                },
                Err(err) => eprintln!("sender: feedback receive error: {err}"),
            }
        }
    });

    Ok(Some(status))
}

fn sender_side_asrc_ppm(args: &Args, remote_status: &Option<SharedReceiverStatus>) -> f64 {
    if !args.sender_side_asrc {
        return 0.0;
    }

    let Some(remote_status) = remote_status else {
        return 0.0;
    };
    let Ok(status) = remote_status.try_lock() else {
        return 0.0;
    };
    let Some(status) = status.as_ref() else {
        return 0.0;
    };

    let error_ms = status.audio_latency_ms as f64 - status.target_ms as f64;
    (-error_ms * args.sender_asrc_kp).clamp(-args.sender_asrc_max_ppm, args.sender_asrc_max_ppm)
}

fn packet_frames(args: &Args) -> Result<usize> {
    let frames = (args.sample_rate as f64 * args.packet_ms / 1000.0).round() as usize;
    if frames == 0 {
        bail!("--packet-ms is too small for {}Hz", args.sample_rate);
    }
    Ok(frames)
}

enum GeneratedInput {
    Dummy,
    Sine,
}

fn run_generated_sender(
    args: &Args,
    input: GeneratedInput,
    remote_status: Option<SharedReceiverStatus>,
) -> Result<()> {
    let packet_frames = packet_frames(args)?;
    let mut sender = PacketSender::new(args)?;
    let mut metrics = MetricsPrinter::new(args.metrics_interval_sec, remote_status.clone());
    let start = Instant::now();
    let mut next_tick = Instant::now();
    let mut phase = 0.0f32;
    let phase_step = args.freq * std::f32::consts::TAU / args.sample_rate as f32;
    let label = match input {
        GeneratedInput::Dummy => "dummy",
        GeneratedInput::Sine => "sine",
    };

    loop {
        if duration_elapsed(start, args.duration_sec) {
            sender.flush()?;
            return Ok(());
        }

        let mut frames = Vec::with_capacity(packet_frames);
        for _ in 0..packet_frames {
            let frame = match input {
                GeneratedInput::Dummy => StereoFrame::SILENCE,
                GeneratedInput::Sine => {
                    let sample = (phase.sin() * 0.2).clamp(-1.0, 1.0);
                    phase = (phase + phase_step) % std::f32::consts::TAU;
                    StereoFrame {
                        left: sample,
                        right: sample,
                    }
                }
            };
            frames.push(frame);
        }
        let rms = rms_db(&frames);
        sender.send_frames(&frames)?;
        let send_rate_ppm = sender_side_asrc_ppm(args, &remote_status);
        metrics.maybe_print(&sender, label, rms, 0.0, send_rate_ppm);
        sleep_until(next_tick);
        next_tick += packet_interval(args, send_rate_ppm);
    }
}

fn run_file_sender(
    args: &Args,
    path: &Path,
    remote_status: Option<SharedReceiverStatus>,
) -> Result<()> {
    let packet_frames = packet_frames(args)?;
    let frames = load_wav_as_stereo(path, args.sample_rate)
        .with_context(|| format!("failed to read WAV input {}", path.display()))?;
    if frames.is_empty() {
        bail!("input WAV contains no samples");
    }

    let mut sender = PacketSender::new(args)?;
    let mut metrics = MetricsPrinter::new(args.metrics_interval_sec, remote_status.clone());
    let start = Instant::now();
    let mut next_tick = Instant::now();
    let mut cursor = 0usize;

    loop {
        if duration_elapsed(start, args.duration_sec) {
            sender.flush()?;
            return Ok(());
        }

        if cursor >= frames.len() {
            if args.loop_input {
                cursor = 0;
            } else {
                sender.flush()?;
                return Ok(());
            }
        }

        let mut packet = Vec::with_capacity(packet_frames);
        while packet.len() < packet_frames {
            if cursor >= frames.len() {
                if args.loop_input {
                    cursor = 0;
                } else {
                    packet.resize(packet_frames, StereoFrame::SILENCE);
                    break;
                }
            }
            let remaining = packet_frames - packet.len();
            let take = remaining.min(frames.len() - cursor);
            packet.extend_from_slice(&frames[cursor..cursor + take]);
            cursor += take;
        }

        let rms = rms_db(&packet);
        sender.send_frames(&packet)?;
        let send_rate_ppm = sender_side_asrc_ppm(args, &remote_status);
        metrics.maybe_print(&sender, "wav", rms, 0.0, send_rate_ppm);
        sleep_until(next_tick);
        next_tick += packet_interval(args, send_rate_ppm);
    }
}

fn run_capture_sender(args: &Args, remote_status: Option<SharedReceiverStatus>) -> Result<()> {
    let packet_frames = packet_frames(args)?;
    let (stream, rx, source_rate) = open_capture_stream(args)?;
    stream.play().context("failed to start input stream")?;

    println!(
        "capture_started=true target={} source_rate={}",
        args.target, source_rate
    );
    let mut resampler = StreamingLinearResampler::new(source_rate, args.sample_rate);
    let mut sender = PacketSender::new(args)?;
    let mut metrics = MetricsPrinter::new(args.metrics_interval_sec, remote_status.clone());
    let mut pending = Vec::new();
    let start = Instant::now();
    let mut latest_rms = (-120.0, -120.0);

    loop {
        if duration_elapsed(start, args.duration_sec) {
            sender.flush()?;
            return Ok(());
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                latest_rms = rms_db(&chunk);
                let send_rate_ppm = sender_side_asrc_ppm(args, &remote_status);
                let effective_target_rate =
                    args.sample_rate as f64 * (1.0 + send_rate_ppm / 1_000_000.0).max(0.0001);
                resampler.set_effective_target_rate(source_rate, effective_target_rate);
                let mut resampled = Vec::new();
                resampler.push(&chunk, &mut resampled);
                pending.extend(resampled);
                while pending.len() >= packet_frames {
                    let packet: Vec<_> = pending.drain(..packet_frames).collect();
                    sender.send_frames(&packet)?;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(err) => return Err(err).context("capture stream stopped"),
        }
        let pending_ms = pending.len() as f32 * 1000.0 / args.sample_rate as f32;
        let send_rate_ppm = sender_side_asrc_ppm(args, &remote_status);
        metrics.maybe_print(&sender, "capture", latest_rms, pending_ms, send_rate_ppm);
    }
}

fn run_capture_monitor(args: &Args) -> Result<()> {
    let (stream, rx, source_rate) = open_capture_stream(args)?;
    stream.play().context("failed to start input stream")?;

    let mut writer = if let Some(path) = &args.output_file {
        Some(
            create_wav_writer(path, args.sample_rate)
                .with_context(|| format!("failed to create {}", path.display()))?,
        )
    } else {
        None
    };
    let mut resampler = StreamingLinearResampler::new(source_rate, args.sample_rate);
    let start = Instant::now();
    let mut last = Instant::now();
    let interval = Duration::from_secs_f64(args.metrics_interval_sec.max(0.1));
    let mut captured_frames = 0u64;
    let mut latest_rms = (-120.0, -120.0);

    println!(
        "capture_started=true source_rate={source_rate} output_file={:?}",
        args.output_file
    );
    loop {
        if duration_elapsed(start, args.duration_sec) {
            if let Some(writer) = writer {
                writer.finalize().context("failed to finalize WAV output")?;
            }
            return Ok(());
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                latest_rms = rms_db(&chunk);
                let mut resampled = Vec::new();
                resampler.push(&chunk, &mut resampled);
                captured_frames += resampled.len() as u64;
                if let Some(writer) = writer.as_mut() {
                    for frame in &resampled {
                        writer.write_sample(f32_to_i16(frame.left))?;
                        writer.write_sample(f32_to_i16(frame.right))?;
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(err) => return Err(err).context("capture stream stopped"),
        }

        if last.elapsed() >= interval {
            println!(
                "sender: meter frames={} rms={:.1}/{:.1}dB",
                captured_frames, latest_rms.0, latest_rms.1
            );
            last = Instant::now();
        }
    }
}

fn open_capture_stream(args: &Args) -> Result<(Stream, Receiver<Vec<StereoFrame>>, u32)> {
    let host = cpal::default_host();
    let device = select_input_device(&host, args.device.as_deref())?;
    let device_name = device.to_string();
    let supported = device
        .default_input_config()
        .context("failed to get default input config")?;
    let sample_format = supported.sample_format();
    let config = supported.config();
    let source_rate = config.sample_rate;
    let source_channels = config.channels as usize;
    let (tx, rx) = sync_channel::<Vec<StereoFrame>>(32);

    println!(
        "selected_device=\"{}\" input_format={}Hz/{}ch/{:?}",
        device_name, source_rate, source_channels, sample_format
    );

    let stream = build_input_stream(&device, sample_format, &config, source_channels, tx)?;
    Ok((stream, rx, source_rate))
}

fn build_input_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    channels: usize,
    tx: SyncSender<Vec<StereoFrame>>,
) -> Result<Stream> {
    let stream = match sample_format {
        SampleFormat::I8 => build_input_stream_for::<i8>(device, config, channels, tx)?,
        SampleFormat::I16 => build_input_stream_for::<i16>(device, config, channels, tx)?,
        SampleFormat::I24 => build_input_stream_for::<I24>(device, config, channels, tx)?,
        SampleFormat::I32 => build_input_stream_for::<i32>(device, config, channels, tx)?,
        SampleFormat::I64 => build_input_stream_for::<i64>(device, config, channels, tx)?,
        SampleFormat::U8 => build_input_stream_for::<u8>(device, config, channels, tx)?,
        SampleFormat::U16 => build_input_stream_for::<u16>(device, config, channels, tx)?,
        SampleFormat::U24 => build_input_stream_for::<U24>(device, config, channels, tx)?,
        SampleFormat::U32 => build_input_stream_for::<u32>(device, config, channels, tx)?,
        SampleFormat::U64 => build_input_stream_for::<u64>(device, config, channels, tx)?,
        SampleFormat::F32 => build_input_stream_for::<f32>(device, config, channels, tx)?,
        SampleFormat::F64 => build_input_stream_for::<f64>(device, config, channels, tx)?,
        SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 => {
            bail!("DSD input sample format {sample_format:?} is not supported")
        }
        other => bail!("unsupported input sample format {other:?}"),
    };
    Ok(stream)
}

fn build_input_stream_for<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    tx: SyncSender<Vec<StereoFrame>>,
) -> Result<Stream>
where
    T: Sample + SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let err_fn = |err| eprintln!("audio input stream error: {err}");
    Ok(device.build_input_stream(
        *config,
        move |data: &[T], _| {
            let _ = tx.try_send(input_to_stereo(data, channels));
        },
        err_fn,
        None,
    )?)
}

fn input_to_stereo<T>(data: &[T], channels: usize) -> Vec<StereoFrame>
where
    T: Sample,
    f32: FromSample<T>,
{
    let channels = channels.max(1);
    data.chunks_exact(channels)
        .map(|frame| {
            let left = f32::from_sample(frame[0]).clamp(-1.0, 1.0);
            let right = frame
                .get(1)
                .copied()
                .map(f32::from_sample)
                .unwrap_or(left)
                .clamp(-1.0, 1.0);
            StereoFrame { left, right }
        })
        .collect()
}

fn select_input_device(host: &cpal::Host, filter: Option<&str>) -> Result<cpal::Device> {
    if let Some(filter) = filter {
        let filter = filter.to_lowercase();
        for device in host.input_devices()? {
            let name = device.to_string();
            if name.to_lowercase().contains(&filter) {
                return Ok(device);
            }
        }
        bail!("input device containing {filter:?} was not found");
    }

    host.default_input_device()
        .ok_or_else(|| anyhow!("default input device was not found"))
}

fn list_input_devices() -> Result<()> {
    let host = cpal::default_host();
    for (index, device) in host.input_devices()?.enumerate() {
        let name = device.to_string();
        let default = device
            .default_input_config()
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

fn load_wav_as_stereo(path: &Path, target_rate: u32) -> Result<Vec<StereoFrame>> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();
    let source_channels = spec.channels as usize;
    if source_channels == 0 {
        bail!("WAV has zero channels");
    }

    let samples = match spec.sample_format {
        WavSampleFormat::Int if spec.bits_per_sample <= 16 => reader
            .samples::<i16>()
            .map(|sample| sample.map(i16_to_f32))
            .collect::<Result<Vec<_>, _>>()?,
        WavSampleFormat::Int => {
            let scale = ((1i64 << (spec.bits_per_sample - 1)) - 1) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| (value as f32 / scale).clamp(-1.0, 1.0)))
                .collect::<Result<Vec<_>, _>>()?
        }
        WavSampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| sample.map(|value| value.clamp(-1.0, 1.0)))
            .collect::<Result<Vec<_>, _>>()?,
    };

    let frames: Vec<_> = samples
        .chunks_exact(source_channels)
        .map(|frame| {
            let left = frame[0];
            let right = frame.get(1).copied().unwrap_or(left);
            StereoFrame { left, right }
        })
        .collect();

    Ok(resample_linear(&frames, spec.sample_rate, target_rate))
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

fn packet_interval(args: &Args, send_rate_ppm: f64) -> Duration {
    let nominal = args.packet_ms / 1000.0;
    let speed = 1.0 + (args.drift_ppm + send_rate_ppm) / 1_000_000.0;
    Duration::from_secs_f64(nominal / speed.max(0.0001))
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

fn new_stream_id() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    (now as u64) ^ ((now >> 64) as u64)
}
