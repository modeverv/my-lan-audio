use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct Metrics {
    created_at: Instant,
    events: AtomicU64,
    event_frames: AtomicU64,
    packets: AtomicU64,
    sent_bytes: AtomicU64,
    send_errors: AtomicU64,
    send_would_block: AtomicU64,
    discontinuities: AtomicU64,
    timestamp_errors: AtomicU64,
    silent_frames: AtomicU64,
    last_event_ns: AtomicU64,
    event_gap_count: AtomicU64,
    event_gap_max_ns: AtomicU64,
    event_to_send_max_ns: AtomicU64,
    latest_rms_l_bits: AtomicU32,
    latest_rms_r_bits: AtomicU32,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            created_at: Instant::now(),
            events: AtomicU64::new(0),
            event_frames: AtomicU64::new(0),
            packets: AtomicU64::new(0),
            sent_bytes: AtomicU64::new(0),
            send_errors: AtomicU64::new(0),
            send_would_block: AtomicU64::new(0),
            discontinuities: AtomicU64::new(0),
            timestamp_errors: AtomicU64::new(0),
            silent_frames: AtomicU64::new(0),
            last_event_ns: AtomicU64::new(0),
            event_gap_count: AtomicU64::new(0),
            event_gap_max_ns: AtomicU64::new(0),
            event_to_send_max_ns: AtomicU64::new(0),
            latest_rms_l_bits: AtomicU32::new((-120.0f32).to_bits()),
            latest_rms_r_bits: AtomicU32::new((-120.0f32).to_bits()),
        }
    }

    pub fn record_event(
        &self,
        now: Instant,
        frames: u32,
        silent: bool,
        discontinuity: bool,
        timestamp_error: bool,
    ) {
        self.events.fetch_add(1, Ordering::Relaxed);
        self.event_frames
            .fetch_add(u64::from(frames), Ordering::Relaxed);
        if silent {
            self.silent_frames
                .fetch_add(u64::from(frames), Ordering::Relaxed);
        }
        if discontinuity {
            self.discontinuities.fetch_add(1, Ordering::Relaxed);
        }
        if timestamp_error {
            self.timestamp_errors.fetch_add(1, Ordering::Relaxed);
        }

        let now_ns = instant_offset_ns(self.created_at, now);
        let previous_ns = self.last_event_ns.swap(now_ns, Ordering::Relaxed);
        if previous_ns != 0 && now_ns >= previous_ns {
            let gap_ns = now_ns - previous_ns;
            self.event_gap_count.fetch_add(1, Ordering::Relaxed);
            self.event_gap_max_ns.fetch_max(gap_ns, Ordering::Relaxed);
        }
    }

    pub fn record_event_done(&self, event_start: Instant) {
        let elapsed_ns = event_start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.event_to_send_max_ns
            .fetch_max(elapsed_ns, Ordering::Relaxed);
    }

    pub fn record_packet_sent(&self, bytes: usize) {
        self.packets.fetch_add(1, Ordering::Relaxed);
        self.sent_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn record_send_error(&self) {
        self.send_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_send_would_block(&self) {
        self.send_would_block.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_rms(&self, left_db: f32, right_db: f32) {
        self.latest_rms_l_bits
            .store(left_db.to_bits(), Ordering::Relaxed);
        self.latest_rms_r_bits
            .store(right_db.to_bits(), Ordering::Relaxed);
    }

    fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            events: self.events.load(Ordering::Relaxed),
            event_frames: self.event_frames.load(Ordering::Relaxed),
            packets: self.packets.load(Ordering::Relaxed),
            sent_bytes: self.sent_bytes.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            send_would_block: self.send_would_block.load(Ordering::Relaxed),
            discontinuities: self.discontinuities.load(Ordering::Relaxed),
            timestamp_errors: self.timestamp_errors.load(Ordering::Relaxed),
            silent_frames: self.silent_frames.load(Ordering::Relaxed),
            event_gap_count: self.event_gap_count.swap(0, Ordering::Relaxed),
            event_gap_max: Duration::from_nanos(self.event_gap_max_ns.swap(0, Ordering::Relaxed)),
            event_to_send_max: Duration::from_nanos(
                self.event_to_send_max_ns.swap(0, Ordering::Relaxed),
            ),
            latest_rms_l: f32::from_bits(self.latest_rms_l_bits.load(Ordering::Relaxed)),
            latest_rms_r: f32::from_bits(self.latest_rms_r_bits.load(Ordering::Relaxed)),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct MetricsSnapshot {
    events: u64,
    event_frames: u64,
    packets: u64,
    sent_bytes: u64,
    send_errors: u64,
    send_would_block: u64,
    discontinuities: u64,
    timestamp_errors: u64,
    silent_frames: u64,
    event_gap_count: u64,
    event_gap_max: Duration,
    event_to_send_max: Duration,
    latest_rms_l: f32,
    latest_rms_r: f32,
}

pub struct MetricsPrinter {
    interval: Duration,
    last: Instant,
    metrics: Arc<Metrics>,
    previous: MetricsSnapshot,
}

impl MetricsPrinter {
    pub fn new(interval_sec: f64, metrics: Arc<Metrics>) -> Self {
        Self {
            interval: Duration::from_secs_f64(interval_sec.max(0.1)),
            last: Instant::now(),
            metrics,
            previous: MetricsSnapshot::default(),
        }
    }

    pub fn maybe_print(&mut self) {
        if self.last.elapsed() < self.interval {
            return;
        }

        let elapsed = self.last.elapsed().as_secs_f64().max(0.001);
        let snapshot = self.metrics.snapshot();
        let event_delta = snapshot.events.saturating_sub(self.previous.events);
        let event_frame_delta = snapshot
            .event_frames
            .saturating_sub(self.previous.event_frames);
        let packet_delta = snapshot.packets.saturating_sub(self.previous.packets);
        let byte_delta = snapshot.sent_bytes.saturating_sub(self.previous.sent_bytes);
        let send_error_delta = snapshot
            .send_errors
            .saturating_sub(self.previous.send_errors);
        let would_block_delta = snapshot
            .send_would_block
            .saturating_sub(self.previous.send_would_block);
        let discontinuity_delta = snapshot
            .discontinuities
            .saturating_sub(self.previous.discontinuities);
        let timestamp_error_delta = snapshot
            .timestamp_errors
            .saturating_sub(self.previous.timestamp_errors);
        let silent_frame_delta = snapshot
            .silent_frames
            .saturating_sub(self.previous.silent_frames);
        let avg_event_frames = if event_delta == 0 {
            0.0
        } else {
            event_frame_delta as f64 / event_delta as f64
        };
        println!(
            "w-sender: events={:.1}/s event_frames_avg={:.1} packets={:.1}/s bitrate={:.3}Mbps rms={:.1}/{:.1}dB send_error={:.1}/s send_would_block={:.1}/s discontinuity={} timestamp_error={} silent_frames={:.0}/s event_gap_max={:.2}ms event_gap_n={} event_to_send_max={:.3}ms",
            event_delta as f64 / elapsed,
            avg_event_frames,
            packet_delta as f64 / elapsed,
            byte_delta as f64 * 8.0 / elapsed / 1_000_000.0,
            snapshot.latest_rms_l,
            snapshot.latest_rms_r,
            send_error_delta as f64 / elapsed,
            would_block_delta as f64 / elapsed,
            discontinuity_delta,
            timestamp_error_delta,
            silent_frame_delta as f64 / elapsed,
            snapshot.event_gap_max.as_secs_f64() * 1000.0,
            snapshot.event_gap_count,
            snapshot.event_to_send_max.as_secs_f64() * 1000.0,
        );

        self.last = Instant::now();
        self.previous = snapshot;
    }
}

fn instant_offset_ns(start: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(start)
        .as_nanos()
        .min(u64::MAX as u128) as u64
}
