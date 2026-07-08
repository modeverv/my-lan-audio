use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig, I24, U24};
use lan_audio_common::audio::{f32_to_i16, rms_db, StereoFrame, CHANNELS, SAMPLE_RATE};
use lan_audio_common::packet::{AudioPacketHeader, HEADER_SIZE, MAGIC};
use lan_audio_common::resampler::StreamingLinearResampler;
use lan_audio_common::status::ReceiverStatus;
use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
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

    #[arg(long)]
    device: Option<String>,

    #[arg(long)]
    list_devices: bool,

    #[arg(long, default_value_t = SAMPLE_RATE)]
    sample_rate: u32,

    #[arg(long, default_value_t = CHANNELS)]
    channels: u16,

    #[arg(long, default_value_t = 5.0)]
    packet_ms: f64,

    #[arg(long, default_value_t = DEFAULT_CAPTURE_SENDER_QUEUE_CAPACITY)]
    capture_queue_capacity: usize,

    #[arg(long)]
    duration_sec: Option<f64>,

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
    send_errors: u64,
}

type SharedReceiverStatus = Arc<Mutex<Option<ReceiverStatus>>>;

const DEFAULT_CAPTURE_SENDER_QUEUE_CAPACITY: usize = 32;
const CAPTURE_PACING_MAX_CATCH_UP_PACKETS: usize = 16;

#[derive(Default, Clone, Copy)]
struct CaptureQueueSnapshot {
    dropped_chunks: u64,
    dropped_frames: u64,
    lock_misses: u64,
}

struct CaptureQueue {
    capacity: usize,
    chunks: Mutex<VecDeque<Vec<StereoFrame>>>,
    ready: Condvar,
    dropped_chunks: AtomicU64,
    dropped_frames: AtomicU64,
    lock_misses: AtomicU64,
}

impl CaptureQueue {
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capture queue capacity must be non-zero");
        Self {
            capacity,
            chunks: Mutex::new(VecDeque::with_capacity(capacity)),
            ready: Condvar::new(),
            dropped_chunks: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            lock_misses: AtomicU64::new(0),
        }
    }

    fn push_realtime(&self, chunk: Vec<StereoFrame>) {
        let Ok(mut chunks) = self.chunks.try_lock() else {
            self.record_dropped_chunk_len(chunk.len());
            self.lock_misses.fetch_add(1, Ordering::Relaxed);
            return;
        };

        while chunks.len() >= self.capacity {
            if let Some(old) = chunks.pop_front() {
                self.record_dropped_chunk_len(old.len());
            }
        }
        chunks.push_back(chunk);
        self.ready.notify_one();
    }

    fn recv_timeout(&self, timeout: Duration) -> Option<Vec<StereoFrame>> {
        let mut chunks = self.chunks.lock().ok()?;
        if chunks.is_empty() {
            let Ok((guard, result)) = self.ready.wait_timeout(chunks, timeout) else {
                return None;
            };
            chunks = guard;
            if result.timed_out() && chunks.is_empty() {
                return None;
            }
        }
        chunks.pop_front()
    }

    fn snapshot(&self) -> CaptureQueueSnapshot {
        CaptureQueueSnapshot {
            dropped_chunks: self.dropped_chunks.load(Ordering::Relaxed),
            dropped_frames: self.dropped_frames.load(Ordering::Relaxed),
            lock_misses: self.lock_misses.load(Ordering::Relaxed),
        }
    }

    fn record_dropped_chunk_len(&self, frames: usize) {
        self.dropped_chunks.fetch_add(1, Ordering::Relaxed);
        self.dropped_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    }
}

struct PacketSender {
    socket: UdpSocket,
    target: SocketAddr,
    stream_id: u64,
    sequence: u32,
    sample_position: u64,
    start: Instant,
    stats: SendStats,
    packet_bytes: Vec<u8>,
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
            packet_bytes: Vec::with_capacity(HEADER_SIZE + 512),
        })
    }

    fn send_frames(&mut self, frames: &[StereoFrame]) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let header = AudioPacketHeader::new(
            self.stream_id,
            self.sequence,
            frames.len() as u16,
            self.sample_position,
            self.start.elapsed().as_nanos() as u64,
        );
        write_packet_bytes(&mut self.packet_bytes, &header, frames);
        self.sequence = self.sequence.wrapping_add(1);
        self.sample_position += frames.len() as u64;
        self.send_current_packet()
    }

    fn send_current_packet(&mut self) -> Result<()> {
        match self.socket.send_to(&self.packet_bytes, self.target) {
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

fn write_packet_bytes(out: &mut Vec<u8>, header: &AudioPacketHeader, frames: &[StereoFrame]) {
    out.clear();
    out.reserve(HEADER_SIZE + frames.len() * usize::from(CHANNELS) * 2);
    out.extend_from_slice(&MAGIC);
    write_u16(out, header.version);
    write_u16(out, header.header_size);
    write_u64(out, header.stream_id);
    write_u32(out, header.sequence);
    write_u32(out, header.flags);
    write_u32(out, header.sample_rate);
    write_u16(out, header.channels);
    write_u16(out, header.sample_format);
    write_u16(out, header.frames);
    write_u16(out, header.reserved);
    write_u64(out, header.sample_position);
    write_u64(out, header.send_time_ns);

    for frame in frames {
        write_i16(out, f32_to_i16(frame.left));
        write_i16(out, f32_to_i16(frame.right));
    }
}

fn write_i16(out: &mut Vec<u8>, value: i16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

struct MetricsPrinter {
    interval: Duration,
    last: Instant,
    last_packets: u64,
    last_bytes: u64,
    remote_status: Option<SharedReceiverStatus>,
    capture_queue: Option<Arc<CaptureQueue>>,
    last_capture_queue: CaptureQueueSnapshot,
}

impl MetricsPrinter {
    fn new(
        interval_sec: f64,
        remote_status: Option<SharedReceiverStatus>,
        capture_queue: Option<Arc<CaptureQueue>>,
    ) -> Self {
        Self {
            interval: Duration::from_secs_f64(interval_sec.max(0.1)),
            last: Instant::now(),
            last_packets: 0,
            last_bytes: 0,
            remote_status,
            capture_queue,
            last_capture_queue: CaptureQueueSnapshot::default(),
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
        let capture_suffix = self.capture_suffix(elapsed);
        let remote_suffix = self.remote_suffix();
        println!(
            "sender: input={label} packets={:.1}/s bitrate={:.3}Mbps sequence={} sample_position={} capture_buffer={:.1}ms rms={:.1}/{:.1}dB errors={} send_corr={:.1}ppm{}{}",
            packets as f64 / elapsed,
            bitrate_mbps,
            sender.sequence,
            sender.sample_position,
            buffer_ms,
            rms.0,
            rms.1,
            sender.stats.send_errors,
            send_rate_ppm,
            capture_suffix,
            remote_suffix
        );

        self.last = Instant::now();
        self.last_packets = sender.stats.sent_packets;
        self.last_bytes = sender.stats.sent_bytes;
    }

    fn capture_suffix(&mut self, elapsed: f64) -> String {
        let Some(capture_queue) = &self.capture_queue else {
            return String::new();
        };
        let snapshot = capture_queue.snapshot();
        let dropped_chunks = snapshot
            .dropped_chunks
            .saturating_sub(self.last_capture_queue.dropped_chunks);
        let dropped_frames = snapshot
            .dropped_frames
            .saturating_sub(self.last_capture_queue.dropped_frames);
        let lock_misses = snapshot
            .lock_misses
            .saturating_sub(self.last_capture_queue.lock_misses);
        self.last_capture_queue = snapshot;

        format!(
            " capture_qdrop={:.1}/s capture_qdrop_frames={:.0}/s capture_lock_miss={:.1}/s",
            dropped_chunks as f64 / elapsed,
            dropped_frames as f64 / elapsed,
            lock_misses as f64 / elapsed
        )
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
            " remote_buf={}fr/{:.1}ms remote_outq={}fr/{:.1}ms remote_total={}fr/{:.1}ms remote_fixed={}fr remote_steady_under={} remote_qdrop={} remote_resyncs={} remote_ratio={:.6}",
            status.audio_latency_frames,
            status.audio_latency_ms,
            status.output_queue_frames,
            status.output_queue_ms,
            status.total_buffered_frames,
            status.total_buffered_ms,
            status.fixed_delay_frames,
            status.steady_underruns,
            status.packet_queue_drops,
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

    let remote_status = spawn_feedback_listener(args.feedback_listen)?;
    run_capture_sender(&args, remote_status)
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
    if args.capture_queue_capacity == 0 {
        bail!("--capture-queue-capacity must be greater than zero");
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

fn run_capture_sender(args: &Args, remote_status: Option<SharedReceiverStatus>) -> Result<()> {
    let packet_frames = packet_frames(args)?;
    let (stream, capture_queue, source_rate) =
        open_capture_stream(args, args.capture_queue_capacity)?;
    stream.play().context("failed to start input stream")?;

    println!(
        "capture_started=true target={} source_rate={} capture_queue_capacity={} capture_queue_mode=fifo capture_packet_pacing=on",
        args.target,
        source_rate,
        args.capture_queue_capacity
    );
    let mut resampler = StreamingLinearResampler::new(source_rate, args.sample_rate);
    let mut sender = PacketSender::new(args)?;
    let mut metrics = MetricsPrinter::new(
        args.metrics_interval_sec,
        remote_status.clone(),
        Some(Arc::clone(&capture_queue)),
    );
    let mut pending = Vec::new();
    let mut resampled = Vec::new();
    let start = Instant::now();
    let mut next_packet_deadline = Instant::now();
    let mut latest_rms = (-120.0, -120.0);

    loop {
        if duration_elapsed(start, args.duration_sec) {
            return Ok(());
        }

        let send_rate_ppm = sender_side_asrc_ppm(args, &remote_status);
        let interval = packet_interval(args, send_rate_ppm);
        let now = Instant::now();
        if pending.len() >= packet_frames {
            let max_lag = interval.mul_f64(CAPTURE_PACING_MAX_CATCH_UP_PACKETS as f64);
            if now.saturating_duration_since(next_packet_deadline) > max_lag {
                let keep_frames = packet_frames * CAPTURE_PACING_MAX_CATCH_UP_PACKETS;
                if pending.len() > keep_frames {
                    pending.drain(..pending.len() - keep_frames);
                }
                next_packet_deadline = now;
            }

            let mut sent_packets = 0usize;
            while pending.len() >= packet_frames && Instant::now() >= next_packet_deadline {
                sender.send_frames(&pending[..packet_frames])?;
                pending.drain(..packet_frames);
                next_packet_deadline += interval;
                sent_packets += 1;
                if sent_packets >= CAPTURE_PACING_MAX_CATCH_UP_PACKETS {
                    break;
                }
            }

            if sent_packets > 0 {
                let pending_ms = pending.len() as f32 * 1000.0 / args.sample_rate as f32;
                metrics.maybe_print(&sender, "capture", latest_rms, pending_ms, send_rate_ppm);
                continue;
            }
        }

        let capture_timeout = if pending.len() >= packet_frames {
            next_packet_deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(100))
        } else {
            Duration::from_millis(100)
        };
        let captured = capture_queue.recv_timeout(capture_timeout);

        if let Some(chunk) = captured {
            latest_rms = rms_db(&chunk);
            let effective_target_rate =
                args.sample_rate as f64 * (1.0 + send_rate_ppm / 1_000_000.0).max(0.0001);
            resampler.set_effective_target_rate(source_rate, effective_target_rate);
            resampled.clear();
            resampler.push(&chunk, &mut resampled);
            pending.extend_from_slice(&resampled);
        }
        let pending_ms = pending.len() as f32 * 1000.0 / args.sample_rate as f32;
        metrics.maybe_print(&sender, "capture", latest_rms, pending_ms, send_rate_ppm);
    }
}

fn open_capture_stream(
    args: &Args,
    queue_capacity: usize,
) -> Result<(Stream, Arc<CaptureQueue>, u32)> {
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
    let capture_queue = Arc::new(CaptureQueue::new(queue_capacity));

    println!(
        "selected_device=\"{}\" input_format={}Hz/{}ch/{:?}",
        device_name, source_rate, source_channels, sample_format
    );

    let stream = build_input_stream(
        &device,
        sample_format,
        &config,
        source_channels,
        Arc::clone(&capture_queue),
    )?;
    Ok((stream, capture_queue, source_rate))
}

fn build_input_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    channels: usize,
    capture_queue: Arc<CaptureQueue>,
) -> Result<Stream> {
    let stream = match sample_format {
        SampleFormat::I8 => build_input_stream_for::<i8>(device, config, channels, capture_queue)?,
        SampleFormat::I16 => {
            build_input_stream_for::<i16>(device, config, channels, capture_queue)?
        }
        SampleFormat::I24 => {
            build_input_stream_for::<I24>(device, config, channels, capture_queue)?
        }
        SampleFormat::I32 => {
            build_input_stream_for::<i32>(device, config, channels, capture_queue)?
        }
        SampleFormat::I64 => {
            build_input_stream_for::<i64>(device, config, channels, capture_queue)?
        }
        SampleFormat::U8 => build_input_stream_for::<u8>(device, config, channels, capture_queue)?,
        SampleFormat::U16 => {
            build_input_stream_for::<u16>(device, config, channels, capture_queue)?
        }
        SampleFormat::U24 => {
            build_input_stream_for::<U24>(device, config, channels, capture_queue)?
        }
        SampleFormat::U32 => {
            build_input_stream_for::<u32>(device, config, channels, capture_queue)?
        }
        SampleFormat::U64 => {
            build_input_stream_for::<u64>(device, config, channels, capture_queue)?
        }
        SampleFormat::F32 => {
            build_input_stream_for::<f32>(device, config, channels, capture_queue)?
        }
        SampleFormat::F64 => {
            build_input_stream_for::<f64>(device, config, channels, capture_queue)?
        }
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
    capture_queue: Arc<CaptureQueue>,
) -> Result<Stream>
where
    T: Sample + SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let err_fn = |err| eprintln!("audio input stream error: {err}");
    Ok(device.build_input_stream(
        *config,
        move |data: &[T], _| {
            capture_queue.push_realtime(input_to_stereo(data, channels));
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

fn new_stream_id() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    (now as u64) ^ ((now >> 64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(value: f32, frames: usize) -> Vec<StereoFrame> {
        vec![
            StereoFrame {
                left: value,
                right: value,
            };
            frames
        ]
    }

    #[test]
    fn capture_queue_push_drops_oldest_when_full() {
        let queue = CaptureQueue::new(2);

        queue.push_realtime(chunk(1.0, 10));
        queue.push_realtime(chunk(2.0, 20));
        queue.push_realtime(chunk(3.0, 30));

        let first = queue.recv_timeout(Duration::from_millis(0)).unwrap();
        assert_eq!(first[0].left, 2.0);

        let snapshot = queue.snapshot();
        assert_eq!(snapshot.dropped_chunks, 1);
        assert_eq!(snapshot.dropped_frames, 10);
    }

    #[test]
    fn capture_queue_fifo_recv_preserves_backlog() {
        let queue = CaptureQueue::new(4);

        queue.push_realtime(chunk(1.0, 10));
        queue.push_realtime(chunk(2.0, 20));
        queue.push_realtime(chunk(3.0, 30));

        let first = queue.recv_timeout(Duration::from_millis(0)).unwrap();
        let second = queue.recv_timeout(Duration::from_millis(0)).unwrap();
        let third = queue.recv_timeout(Duration::from_millis(0)).unwrap();

        assert_eq!(first[0].left, 1.0);
        assert_eq!(second[0].left, 2.0);
        assert_eq!(third[0].left, 3.0);

        let snapshot = queue.snapshot();
        assert_eq!(snapshot.dropped_chunks, 0);
        assert_eq!(snapshot.dropped_frames, 0);
    }
}
