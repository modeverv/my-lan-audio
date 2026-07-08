use crate::audio::{f32_to_i16, i16_to_f32, StereoFrame, CHANNELS, SAMPLE_RATE};
use crate::packet::AudioPacket;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const STREAM_SWITCH_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub struct JitterConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub capacity_ms: u32,
    pub target_ms: u32,
    pub fixed_delay_frames: u64,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            capacity_ms: 1000,
            target_ms: 300,
            fixed_delay_frames: 14_400,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JitterState {
    NotStarted,
    Priming,
    Running,
    Resync,
}

#[derive(Clone, Debug)]
pub struct JitterMetrics {
    pub state: JitterState,
    pub received_packets: u64,
    pub accepted_packets: u64,
    pub lost_packets: u64,
    pub duplicate_packets: u64,
    pub out_of_order_packets: u64,
    pub late_packets: u64,
    pub invalid_packets: u64,
    pub foreign_stream_packets: u64,
    pub output_underruns: u64,
    pub startup_underruns: u64,
    pub steady_underruns: u64,
    pub missing_frame_calls: u64,
    pub missing_frames: u64,
    pub resyncs: u64,
    pub resyncs_by_stream_change: u64,
    pub resyncs_by_underrun: u64,
    pub stream_id: Option<u64>,
    pub latest_sequence: Option<u32>,
    pub latest_sample_position: Option<u64>,
    pub fixed_delay_frames: u64,
    pub buffer_level_frames: u64,
    pub buffer_level_ms: f32,
    pub audio_latency_ms: f32,
    pub resample_ratio: f32,
    pub device_resample_ratio: f32,
    pub effective_resample_ratio: f32,
    pub estimated_drift_ppm: f32,
}

impl Default for JitterMetrics {
    fn default() -> Self {
        Self {
            state: JitterState::NotStarted,
            received_packets: 0,
            accepted_packets: 0,
            lost_packets: 0,
            duplicate_packets: 0,
            out_of_order_packets: 0,
            late_packets: 0,
            invalid_packets: 0,
            foreign_stream_packets: 0,
            output_underruns: 0,
            startup_underruns: 0,
            steady_underruns: 0,
            missing_frame_calls: 0,
            missing_frames: 0,
            resyncs: 0,
            resyncs_by_stream_change: 0,
            resyncs_by_underrun: 0,
            stream_id: None,
            latest_sequence: None,
            latest_sample_position: None,
            fixed_delay_frames: 0,
            buffer_level_frames: 0,
            buffer_level_ms: 0.0,
            audio_latency_ms: 0.0,
            resample_ratio: 1.0,
            device_resample_ratio: 1.0,
            effective_resample_ratio: 1.0,
            estimated_drift_ppm: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertOutcome {
    Accepted,
    Duplicate,
    ForeignStream,
    Late,
    Resynced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResyncReason {
    StreamChange,
    Underrun,
}

#[derive(Clone, Debug)]
struct PacketFrames {
    payload: Vec<i16>,
    frame_count: usize,
}

#[derive(Debug)]
pub struct JitterBuffer {
    config: JitterConfig,
    state: JitterState,
    stream_id: Option<u64>,
    packets: BTreeMap<u64, PacketFrames>,
    read_pos: f64,
    latest_received_end: u64,
    expected_sequence: Option<u32>,
    first_drift_sample: Option<u64>,
    first_drift_arrival: Option<Instant>,
    fixed_anchor_sample: Option<u64>,
    fixed_anchor_playout_at: Option<Instant>,
    last_stream_arrival: Option<Instant>,
    consecutive_missing_frames: u64,
    metrics: JitterMetrics,
}

impl JitterBuffer {
    pub fn new(config: JitterConfig) -> Self {
        Self {
            config,
            state: JitterState::NotStarted,
            stream_id: None,
            packets: BTreeMap::new(),
            read_pos: 0.0,
            latest_received_end: 0,
            expected_sequence: None,
            first_drift_sample: None,
            first_drift_arrival: None,
            fixed_anchor_sample: None,
            fixed_anchor_playout_at: None,
            last_stream_arrival: None,
            consecutive_missing_frames: 0,
            metrics: JitterMetrics::default(),
        }
    }

    pub fn insert_packet(&mut self, packet: AudioPacket, arrival: Instant) -> InsertOutcome {
        self.metrics.received_packets += 1;

        if packet.header.sample_rate != self.config.sample_rate
            || packet.header.channels != self.config.channels
        {
            self.metrics.invalid_packets += 1;
            return InsertOutcome::Late;
        }

        let mut outcome = InsertOutcome::Accepted;
        if let Some(stream_id) = self.stream_id {
            if stream_id != packet.header.stream_id {
                if self.current_stream_is_active(arrival) {
                    self.metrics.foreign_stream_packets += 1;
                    self.refresh_metrics();
                    return InsertOutcome::ForeignStream;
                }
                self.clear_for_resync(ResyncReason::StreamChange);
                outcome = InsertOutcome::Resynced;
            }
        }

        if self.stream_id != Some(packet.header.stream_id) {
            self.stream_id = Some(packet.header.stream_id);
            self.read_pos = packet.header.sample_position as f64;
            self.set_fixed_schedule(packet.header.sample_position, arrival);
            self.state = JitterState::Priming;
        } else if self.fixed_anchor_playout_at.is_none() {
            self.read_pos = packet.header.sample_position as f64;
            self.state = JitterState::Priming;
            self.set_fixed_schedule(packet.header.sample_position, arrival);
        }

        self.update_sequence_metrics(packet.header.sequence);

        let start = packet.header.sample_position;
        let end = start + packet.header.frames as u64;
        if self.state == JitterState::Running && end <= self.read_pos.floor() as u64 {
            self.metrics.late_packets += 1;
            self.refresh_metrics();
            return InsertOutcome::Late;
        }

        if self.packets.contains_key(&start) {
            self.metrics.duplicate_packets += 1;
            self.refresh_metrics();
            return InsertOutcome::Duplicate;
        }

        self.packets.insert(
            start,
            PacketFrames {
                payload: packet.payload,
                frame_count: packet.header.frames as usize,
            },
        );
        self.latest_received_end = self.latest_received_end.max(end);
        self.metrics.accepted_packets += 1;
        self.metrics.latest_sequence = Some(packet.header.sequence);
        self.metrics.latest_sample_position = Some(start);
        self.last_stream_arrival = Some(arrival);
        self.update_drift_estimate(start, arrival);
        self.drop_over_capacity();
        self.refresh_metrics();
        outcome
    }

    pub fn record_invalid_packet(&mut self) {
        self.metrics.received_packets += 1;
        self.metrics.invalid_packets += 1;
    }

    pub fn pull_f32(&mut self, output: &mut [f32]) {
        self.pull_f32_at_sample_rate(output, self.config.sample_rate);
    }

    pub fn pull_f32_at_sample_rate(&mut self, output: &mut [f32], output_sample_rate: u32) {
        self.pull_f32_at_sample_rate_for_playout(output, output_sample_rate, Instant::now());
    }

    pub fn pull_f32_at_sample_rate_for_playout(
        &mut self,
        output: &mut [f32],
        output_sample_rate: u32,
        first_output_playout_at: Instant,
    ) {
        self.pull_fixed_f32_at_sample_rate(output, output_sample_rate, first_output_playout_at);
    }

    fn pull_fixed_f32_at_sample_rate(
        &mut self,
        output: &mut [f32],
        output_sample_rate: u32,
        first_output_playout_at: Instant,
    ) {
        output.fill(0.0);
        if output.is_empty() || self.config.channels == 0 {
            return;
        }

        let channels = self.config.channels as usize;
        let output_sample_rate = output_sample_rate.max(1);
        let device_resample_ratio = self.config.sample_rate as f64 / output_sample_rate as f64;
        self.set_resample_metrics(device_resample_ratio as f32);

        let output_frames = output.len() / channels;
        let chunk_duration = duration_from_frames(output_frames, output_sample_rate);
        let chunk_end_playout_at = first_output_playout_at + chunk_duration;

        let (Some(anchor_sample), Some(anchor_playout_at)) =
            (self.fixed_anchor_sample, self.fixed_anchor_playout_at)
        else {
            if self.stream_id.is_some() {
                self.metrics.startup_underruns += 1;
            }
            self.refresh_metrics();
            return;
        };

        if chunk_end_playout_at <= anchor_playout_at {
            if self.stream_id.is_some() {
                self.metrics.startup_underruns += 1;
            }
            self.state = JitterState::Priming;
            self.read_pos = anchor_sample as f64;
            self.refresh_metrics();
            return;
        }

        let mut start_frame = 0usize;
        if self.state != JitterState::Running {
            self.state = JitterState::Running;
            self.read_pos = anchor_sample as f64;

            if let Some(wait) = anchor_playout_at.checked_duration_since(first_output_playout_at) {
                start_frame = ((wait.as_secs_f64() * output_sample_rate as f64).ceil() as usize)
                    .min(output_frames);
                let first_audio_playout_at =
                    first_output_playout_at + duration_from_frames(start_frame, output_sample_rate);
                if let Some(elapsed) =
                    first_audio_playout_at.checked_duration_since(anchor_playout_at)
                {
                    self.read_pos = anchor_sample as f64
                        + elapsed.as_secs_f64() * self.config.sample_rate as f64;
                }
            } else {
                let elapsed = first_output_playout_at.duration_since(anchor_playout_at);
                self.read_pos =
                    anchor_sample as f64 + elapsed.as_secs_f64() * self.config.sample_rate as f64;
            }
        }

        let mut missing_in_call = 0u64;
        let mut consecutive_missing_frames = self.consecutive_missing_frames;
        let mut read_pos = self.read_pos;

        {
            let packets = &self.packets;
            let mut packet_cache = None;
            for frame_out in output.chunks_exact_mut(channels).skip(start_frame) {
                let pos0 = read_pos.floor().max(0.0) as u64;
                let frac = (read_pos - pos0 as f64) as f32;

                match (
                    sample_at_cached(packets, channels, pos0, &mut packet_cache),
                    sample_at_cached(packets, channels, pos0 + 1, &mut packet_cache),
                ) {
                    (Some(a), Some(b)) => {
                        let frame = a.lerp(b, frac);
                        frame_out[0] = frame.left;
                        if channels > 1 {
                            frame_out[1] = frame.right;
                        }
                        consecutive_missing_frames = 0;
                    }
                    (Some(a), None) => {
                        frame_out[0] = a.left;
                        if channels > 1 {
                            frame_out[1] = a.right;
                        }
                        consecutive_missing_frames = 0;
                    }
                    _ => {
                        missing_in_call += 1;
                        consecutive_missing_frames += 1;
                    }
                }
                read_pos += device_resample_ratio;
            }
        }

        self.read_pos = read_pos;
        self.consecutive_missing_frames = consecutive_missing_frames;

        if missing_in_call > 0 {
            self.metrics.missing_frame_calls += 1;
            self.metrics.missing_frames += missing_in_call;
            if self.buffer_level_frames() < 2 {
                self.metrics.output_underruns += 1;
                self.metrics.steady_underruns += 1;
                self.clear_for_resync(ResyncReason::Underrun);
            } else if self.consecutive_missing_frames > self.config.sample_rate as u64 / 2 {
                self.clear_for_resync(ResyncReason::Underrun);
            }
        }

        self.prune_played_packets();
        self.refresh_metrics();
    }

    pub fn pull_i16(&mut self, output: &mut [i16]) {
        self.pull_i16_at_sample_rate(output, self.config.sample_rate);
    }

    pub fn pull_i16_at_sample_rate(&mut self, output: &mut [i16], output_sample_rate: u32) {
        let mut tmp = vec![0.0f32; output.len()];
        self.pull_f32_at_sample_rate(&mut tmp, output_sample_rate);
        for (dst, src) in output.iter_mut().zip(tmp) {
            *dst = f32_to_i16(src);
        }
    }

    pub fn metrics(&self) -> JitterMetrics {
        let mut metrics = self.metrics.clone();
        metrics.state = self.state;
        metrics.stream_id = self.stream_id;
        let buffer_level_frames = self.buffer_level_frames();
        let audio_latency_ms = self.frames_to_ms(buffer_level_frames);
        metrics.fixed_delay_frames = self.config.fixed_delay_frames;
        metrics.buffer_level_frames = buffer_level_frames;
        metrics.buffer_level_ms = audio_latency_ms;
        metrics.audio_latency_ms = audio_latency_ms;
        metrics
    }

    pub fn target_ms(&self) -> u32 {
        self.config.target_ms
    }

    fn update_sequence_metrics(&mut self, sequence: u32) {
        if let Some(expected) = self.expected_sequence {
            let forward = sequence.wrapping_sub(expected);
            if forward == 0 {
                // Expected packet.
            } else if forward < u32::MAX / 2 {
                self.metrics.lost_packets += forward as u64;
            } else {
                self.metrics.out_of_order_packets += 1;
            }
        }
        self.expected_sequence = Some(sequence.wrapping_add(1));
    }

    fn update_drift_estimate(&mut self, sample_position: u64, arrival: Instant) {
        match (self.first_drift_sample, self.first_drift_arrival) {
            (Some(first_sample), Some(first_arrival)) => {
                let elapsed_frames = sample_position.saturating_sub(first_sample);
                let elapsed = arrival.duration_since(first_arrival).as_secs_f64();
                if elapsed >= 1.0 {
                    let rate = elapsed_frames as f64 / elapsed;
                    self.metrics.estimated_drift_ppm =
                        ((rate / self.config.sample_rate as f64 - 1.0) * 1_000_000.0) as f32;
                }
            }
            _ => {
                self.first_drift_sample = Some(sample_position);
                self.first_drift_arrival = Some(arrival);
            }
        }
    }

    fn set_fixed_schedule(&mut self, sample_position: u64, arrival: Instant) {
        self.fixed_anchor_sample = Some(sample_position);
        self.fixed_anchor_playout_at = Some(
            arrival
                + duration_from_source_frames(
                    self.config.fixed_delay_frames,
                    self.config.sample_rate,
                ),
        );
    }

    fn current_stream_is_active(&self, arrival: Instant) -> bool {
        self.last_stream_arrival
            .and_then(|last| arrival.checked_duration_since(last))
            .map(|elapsed| elapsed < STREAM_SWITCH_TIMEOUT)
            .unwrap_or(false)
    }

    fn set_resample_metrics(&mut self, device_resample_ratio: f32) {
        self.metrics.device_resample_ratio = device_resample_ratio;
        self.metrics.effective_resample_ratio = device_resample_ratio;
        self.metrics.resample_ratio = device_resample_ratio;
    }

    fn prune_played_packets(&mut self) {
        let read_pos = self.read_pos.floor() as u64;
        let old_keys: Vec<u64> = self
            .packets
            .iter()
            .take_while(|(start, packet)| **start + packet.frame_count as u64 + 1 < read_pos)
            .map(|(start, _)| *start)
            .collect();
        for key in old_keys {
            self.packets.remove(&key);
        }
    }

    fn drop_over_capacity(&mut self) {
        let capacity_frames = self.frames_from_ms(self.config.capacity_ms);
        while self
            .latest_received_end
            .saturating_sub(self.oldest_position())
            > capacity_frames
        {
            if let Some(key) = self.packets.keys().next().copied() {
                self.packets.remove(&key);
                self.metrics.late_packets += 1;
            } else {
                break;
            }
        }
    }

    fn oldest_position(&self) -> u64 {
        self.packets
            .keys()
            .next()
            .copied()
            .unwrap_or(self.latest_received_end)
    }

    fn clear_for_resync(&mut self, reason: ResyncReason) {
        self.packets.clear();
        self.latest_received_end = 0;
        self.expected_sequence = None;
        self.first_drift_sample = None;
        self.first_drift_arrival = None;
        self.fixed_anchor_sample = None;
        self.fixed_anchor_playout_at = None;
        self.last_stream_arrival = None;
        self.consecutive_missing_frames = 0;
        self.state = JitterState::Resync;
        self.metrics.resyncs += 1;
        match reason {
            ResyncReason::StreamChange => self.metrics.resyncs_by_stream_change += 1,
            ResyncReason::Underrun => self.metrics.resyncs_by_underrun += 1,
        }
    }

    fn refresh_metrics(&mut self) {
        self.metrics.state = self.state;
        self.metrics.stream_id = self.stream_id;
        let buffer_level_frames = self.buffer_level_frames();
        let audio_latency_ms = self.frames_to_ms(buffer_level_frames);
        self.metrics.fixed_delay_frames = self.config.fixed_delay_frames;
        self.metrics.buffer_level_frames = buffer_level_frames;
        self.metrics.buffer_level_ms = audio_latency_ms;
        self.metrics.audio_latency_ms = audio_latency_ms;
    }

    fn buffer_level_frames(&self) -> u64 {
        self.latest_received_end
            .saturating_sub(self.read_pos.floor().max(0.0) as u64)
    }

    fn frames_to_ms(&self, frames: u64) -> f32 {
        frames as f32 * 1000.0 / self.config.sample_rate as f32
    }

    fn frames_from_ms(&self, ms: u32) -> u64 {
        self.config.sample_rate as u64 * ms as u64 / 1000
    }
}

fn duration_from_frames(frames: usize, sample_rate: u32) -> Duration {
    Duration::from_secs_f64(frames as f64 / sample_rate.max(1) as f64)
}

fn duration_from_source_frames(frames: u64, sample_rate: u32) -> Duration {
    Duration::from_secs_f64(frames as f64 / sample_rate.max(1) as f64)
}

fn sample_at_cached<'a>(
    packets: &'a BTreeMap<u64, PacketFrames>,
    channels: usize,
    position: u64,
    cache: &mut Option<(u64, &'a PacketFrames)>,
) -> Option<StereoFrame> {
    if let Some((start, packet)) = cache {
        if position >= *start && position < *start + packet.frame_count as u64 {
            return sample_from_packet(*start, packet, channels, position);
        }
    }

    let (start, packet) = packets.range(..=position).next_back()?;
    if position >= *start + packet.frame_count as u64 {
        return None;
    }

    *cache = Some((*start, packet));
    sample_from_packet(*start, packet, channels, position)
}

fn sample_from_packet(
    start: u64,
    packet: &PacketFrames,
    channels: usize,
    position: u64,
) -> Option<StereoFrame> {
    let offset = position.checked_sub(start)? as usize;
    if offset >= packet.frame_count {
        return None;
    }

    let sample_offset = offset * channels;
    let left = *packet.payload.get(sample_offset)?;
    let right = if channels == 1 {
        left
    } else {
        *packet.payload.get(sample_offset + 1).unwrap_or(&left)
    };

    Some(StereoFrame {
        left: i16_to_f32(left),
        right: i16_to_f32(right),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::stereo_to_i16_interleaved;
    use crate::packet::{AudioPacket, AudioPacketHeader};

    fn packet(sequence: u32, sample_position: u64) -> AudioPacket {
        packet_for_stream(1, sequence, sample_position)
    }

    fn packet_for_stream(stream_id: u64, sequence: u32, sample_position: u64) -> AudioPacket {
        let frames = vec![
            StereoFrame {
                left: 0.1,
                right: 0.2
            };
            240
        ];
        let payload = stereo_to_i16_interleaved(&frames);
        AudioPacket::new(
            AudioPacketHeader::new(stream_id, sequence, 240, sample_position, 0),
            payload,
        )
        .unwrap()
    }

    #[test]
    fn fixed_delay_waits_until_scheduled_playout_time() {
        let t0 = Instant::now();
        let mut buffer = JitterBuffer::new(JitterConfig {
            target_ms: 100,
            fixed_delay_frames: 4_800,
            ..JitterConfig::default()
        });
        buffer.insert_packet(packet(0, 0), t0);

        let mut out = vec![0.0; 240 * 2];
        buffer.pull_f32_at_sample_rate_for_playout(
            &mut out,
            SAMPLE_RATE,
            t0 + Duration::from_millis(90),
        );

        assert_eq!(buffer.metrics().state, JitterState::Priming);
        assert!(out.iter().all(|sample| *sample == 0.0));

        buffer.pull_f32_at_sample_rate_for_playout(
            &mut out,
            SAMPLE_RATE,
            t0 + Duration::from_millis(100),
        );

        let metrics = buffer.metrics();
        assert_eq!(metrics.state, JitterState::Running);
        assert!(out.iter().any(|sample| *sample != 0.0));
        assert_eq!(metrics.fixed_delay_frames, 4_800);
        assert_eq!(metrics.effective_resample_ratio, 1.0);
    }

    #[test]
    fn detects_sequence_loss() {
        let mut buffer = JitterBuffer::new(JitterConfig::default());
        buffer.insert_packet(packet(0, 0), Instant::now());
        buffer.insert_packet(packet(3, 720), Instant::now());
        assert_eq!(buffer.metrics().lost_packets, 2);
    }

    #[test]
    fn duplicate_sample_position_is_rejected() {
        let mut buffer = JitterBuffer::new(JitterConfig::default());
        assert_eq!(
            buffer.insert_packet(packet(0, 0), Instant::now()),
            InsertOutcome::Accepted
        );
        assert_eq!(
            buffer.insert_packet(packet(1, 0), Instant::now()),
            InsertOutcome::Duplicate
        );
        assert_eq!(buffer.metrics().duplicate_packets, 1);
    }

    #[test]
    fn counts_priming_silence_separately_from_steady_underruns() {
        let t0 = Instant::now();
        let mut buffer = JitterBuffer::new(JitterConfig {
            target_ms: 100,
            fixed_delay_frames: 4_800,
            ..JitterConfig::default()
        });
        buffer.insert_packet(packet(0, 0), t0);

        let mut out = vec![0.0; 240 * 2];
        buffer.pull_f32_at_sample_rate_for_playout(
            &mut out,
            SAMPLE_RATE,
            t0 + Duration::from_millis(50),
        );

        let metrics = buffer.metrics();
        assert_eq!(metrics.state, JitterState::Priming);
        assert_eq!(metrics.startup_underruns, 1);
        assert_eq!(metrics.steady_underruns, 0);
        assert_eq!(metrics.output_underruns, 0);
        assert_eq!(metrics.effective_resample_ratio, 1.0);
    }

    #[test]
    fn reports_stream_change_resync_reason() {
        let mut buffer = JitterBuffer::new(JitterConfig::default());
        let t0 = Instant::now();
        assert_eq!(
            buffer.insert_packet(packet_for_stream(1, 0, 0), t0),
            InsertOutcome::Accepted
        );
        assert_eq!(
            buffer.insert_packet(
                packet_for_stream(2, 0, 0),
                t0 + STREAM_SWITCH_TIMEOUT + Duration::from_millis(1),
            ),
            InsertOutcome::Resynced
        );

        let metrics = buffer.metrics();
        assert_eq!(metrics.resyncs, 1);
        assert_eq!(metrics.resyncs_by_stream_change, 1);
        assert_eq!(metrics.resyncs_by_underrun, 0);
    }

    #[test]
    fn ignores_foreign_stream_while_current_stream_is_active() {
        let mut buffer = JitterBuffer::new(JitterConfig::default());
        let t0 = Instant::now();
        assert_eq!(
            buffer.insert_packet(packet_for_stream(1, 0, 0), t0),
            InsertOutcome::Accepted
        );
        assert_eq!(
            buffer.insert_packet(packet_for_stream(2, 0, 0), t0 + Duration::from_millis(10)),
            InsertOutcome::ForeignStream
        );

        let metrics = buffer.metrics();
        assert_eq!(metrics.stream_id, Some(1));
        assert_eq!(metrics.foreign_stream_packets, 1);
        assert_eq!(metrics.resyncs, 0);
        assert_eq!(metrics.resyncs_by_stream_change, 0);
    }

    #[test]
    fn consumes_fixed_delay_48k_stream_at_44k1_output_rate() {
        let t0 = Instant::now();
        let mut buffer = JitterBuffer::new(JitterConfig {
            target_ms: 100,
            fixed_delay_frames: 4_800,
            ..JitterConfig::default()
        });
        for sequence in 0..200 {
            buffer.insert_packet(packet(sequence, sequence as u64 * 240), t0);
        }

        let mut out = vec![0.0; 441 * 2];
        buffer.pull_f32_at_sample_rate_for_playout(
            &mut out,
            44_100,
            t0 + Duration::from_millis(100),
        );
        assert_eq!(buffer.metrics().state, JitterState::Running);

        buffer.insert_packet(packet(200, 200 * 240), t0);
        buffer.insert_packet(packet(201, 201 * 240), t0);
        let before = buffer.metrics().buffer_level_ms;
        buffer.pull_f32_at_sample_rate_for_playout(
            &mut out,
            44_100,
            t0 + Duration::from_millis(110),
        );
        let after = buffer.metrics().buffer_level_ms;

        assert_eq!(buffer.metrics().state, JitterState::Running);
        assert!((before - after - 10.0).abs() < 0.5);
        let metrics = buffer.metrics();
        assert!((metrics.audio_latency_ms - metrics.buffer_level_ms).abs() < 0.001);
        assert!((metrics.device_resample_ratio - (48_000.0 / 44_100.0)).abs() < 0.0001);
        assert!((metrics.effective_resample_ratio - (48_000.0 / 44_100.0)).abs() < 0.0001);
        assert!((metrics.resample_ratio - (48_000.0 / 44_100.0)).abs() < 0.0001);
    }

    #[test]
    fn fixed_delay_is_configurable() {
        let t0 = Instant::now();
        let mut buffer = JitterBuffer::new(JitterConfig {
            target_ms: 300,
            fixed_delay_frames: 14_400,
            ..JitterConfig::default()
        });
        buffer.insert_packet(packet(0, 0), t0);

        let mut out = vec![0.0; 240 * 2];
        buffer.pull_f32_at_sample_rate_for_playout(
            &mut out,
            SAMPLE_RATE,
            t0 + Duration::from_millis(290),
        );
        assert!(out.iter().all(|sample| *sample == 0.0));

        buffer.pull_f32_at_sample_rate_for_playout(
            &mut out,
            SAMPLE_RATE,
            t0 + Duration::from_millis(300),
        );
        assert!(out.iter().any(|sample| *sample != 0.0));
    }

    #[test]
    fn fixed_delay_resample_phase_survives_renderer_timing_jitter() {
        let t0 = Instant::now();
        let mut buffer = JitterBuffer::new(JitterConfig {
            target_ms: 100,
            fixed_delay_frames: 4_800,
            ..JitterConfig::default()
        });
        for sequence in 0..200 {
            buffer.insert_packet(packet(sequence, sequence as u64 * 240), t0);
        }

        let mut out = vec![0.0; 441 * 2];
        buffer.pull_f32_at_sample_rate_for_playout(
            &mut out,
            44_100,
            t0 + Duration::from_millis(100),
        );
        assert_eq!(buffer.metrics().state, JitterState::Running);
        let after_first_pull = buffer.read_pos;

        buffer.pull_f32_at_sample_rate_for_playout(
            &mut out,
            44_100,
            t0 + Duration::from_millis(120),
        );
        let consumed_by_second_pull = buffer.read_pos - after_first_pull;

        assert!((after_first_pull - 480.0).abs() < 0.5);
        assert!((consumed_by_second_pull - 480.0).abs() < 0.5);
        let metrics = buffer.metrics();
        assert!((metrics.device_resample_ratio - (48_000.0 / 44_100.0)).abs() < 0.0001);
        assert!((metrics.effective_resample_ratio - (48_000.0 / 44_100.0)).abs() < 0.0001);
    }
}
