use crate::audio::{f32_to_i16, i16_to_f32, StereoFrame, CHANNELS, SAMPLE_RATE};
use crate::packet::AudioPacket;
use std::collections::BTreeMap;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct JitterConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub capacity_ms: u32,
    pub target_ms: u32,
    pub start_threshold_ms: u32,
    pub adaptive_resampling: bool,
    pub kp: f32,
    pub max_ppm: f32,
    pub emergency_max_ppm: f32,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            capacity_ms: 1000,
            target_ms: 100,
            start_threshold_ms: 100,
            adaptive_resampling: true,
            kp: 5.0,
            max_ppm: 100.0,
            emergency_max_ppm: 500.0,
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
    pub output_underruns: u64,
    pub missing_frames: u64,
    pub resyncs: u64,
    pub latest_sequence: Option<u32>,
    pub latest_sample_position: Option<u64>,
    pub buffer_level_ms: f32,
    pub resample_ratio: f32,
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
            output_underruns: 0,
            missing_frames: 0,
            resyncs: 0,
            latest_sequence: None,
            latest_sample_position: None,
            buffer_level_ms: 0.0,
            resample_ratio: 1.0,
            estimated_drift_ppm: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertOutcome {
    Accepted,
    Duplicate,
    Late,
    Resynced,
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
        if self.stream_id != Some(packet.header.stream_id) {
            if self.stream_id.is_some() {
                self.clear_for_resync();
                outcome = InsertOutcome::Resynced;
            }
            self.stream_id = Some(packet.header.stream_id);
            self.read_pos = packet.header.sample_position as f64;
            self.state = JitterState::Priming;
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
        output.fill(0.0);
        if output.is_empty() || self.config.channels == 0 {
            return;
        }

        self.maybe_start();
        if self.state != JitterState::Running {
            self.refresh_metrics();
            return;
        }

        let channels = self.config.channels as usize;
        let output_sample_rate = output_sample_rate.max(1);
        let device_resample_ratio = self.config.sample_rate as f32 / output_sample_rate as f32;
        let ratio = self.compute_resample_ratio() * device_resample_ratio;
        self.metrics.resample_ratio = ratio;

        let mut missing_in_call = 0u64;
        for frame_out in output.chunks_exact_mut(channels) {
            let pos0 = self.read_pos.floor() as u64;
            let frac = (self.read_pos - pos0 as f64) as f32;

            match (self.sample_at(pos0), self.sample_at(pos0 + 1)) {
                (Some(a), Some(b)) => {
                    let frame = a.lerp(b, frac);
                    frame_out[0] = frame.left;
                    if channels > 1 {
                        frame_out[1] = frame.right;
                    }
                    self.consecutive_missing_frames = 0;
                }
                (Some(a), None) => {
                    frame_out[0] = a.left;
                    if channels > 1 {
                        frame_out[1] = a.right;
                    }
                    self.consecutive_missing_frames = 0;
                }
                _ => {
                    missing_in_call += 1;
                    self.consecutive_missing_frames += 1;
                }
            }

            self.read_pos += ratio as f64;
        }

        if missing_in_call > 0 {
            self.metrics.missing_frames += missing_in_call;
            if self.buffer_level_frames() < 2 {
                self.metrics.output_underruns += 1;
                self.state = JitterState::Priming;
                self.read_pos = self.latest_received_end as f64;
                self.consecutive_missing_frames = 0;
            } else if self.consecutive_missing_frames > self.config.sample_rate as u64 / 2 {
                self.clear_for_resync();
                self.state = JitterState::Resync;
                self.metrics.resyncs += 1;
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
        metrics.buffer_level_ms = self.buffer_level_ms();
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

    fn maybe_start(&mut self) {
        if matches!(
            self.state,
            JitterState::NotStarted | JitterState::Priming | JitterState::Resync
        ) && self.buffer_level_frames() >= self.frames_from_ms(self.config.start_threshold_ms)
        {
            if let Some(first) = self.packets.keys().next().copied() {
                let target_frames = self.frames_from_ms(self.config.target_ms);
                let target_read_pos = self.latest_received_end.saturating_sub(target_frames);
                self.read_pos = target_read_pos.max(first) as f64;
            }
            self.state = JitterState::Running;
        }
    }

    fn compute_resample_ratio(&self) -> f32 {
        if !self.config.adaptive_resampling || self.state != JitterState::Running {
            return 1.0;
        }

        let error_ms = self.buffer_level_ms() - self.config.target_ms as f32;
        let max_ppm = if error_ms.abs() > self.config.target_ms as f32 {
            self.config.emergency_max_ppm
        } else {
            self.config.max_ppm
        };
        let correction_ppm = (error_ms * self.config.kp).clamp(-max_ppm, max_ppm);
        1.0 + correction_ppm / 1_000_000.0
    }

    fn sample_at(&self, position: u64) -> Option<StereoFrame> {
        let (start, packet) = self.packets.range(..=position).next_back()?;
        let offset = position.checked_sub(*start)? as usize;
        if offset >= packet.frame_count {
            return None;
        }
        let sample_offset = offset * self.config.channels as usize;
        let left = *packet.payload.get(sample_offset)?;
        let right = if self.config.channels == 1 {
            left
        } else {
            *packet.payload.get(sample_offset + 1).unwrap_or(&left)
        };

        Some(StereoFrame {
            left: i16_to_f32(left),
            right: i16_to_f32(right),
        })
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

    fn clear_for_resync(&mut self) {
        self.packets.clear();
        self.latest_received_end = 0;
        self.expected_sequence = None;
        self.first_drift_sample = None;
        self.first_drift_arrival = None;
        self.consecutive_missing_frames = 0;
        self.state = JitterState::Resync;
        self.metrics.resyncs += 1;
    }

    fn refresh_metrics(&mut self) {
        self.metrics.state = self.state;
        self.metrics.buffer_level_ms = self.buffer_level_ms();
    }

    fn buffer_level_frames(&self) -> u64 {
        self.latest_received_end
            .saturating_sub(self.read_pos.floor().max(0.0) as u64)
    }

    fn buffer_level_ms(&self) -> f32 {
        self.buffer_level_frames() as f32 * 1000.0 / self.config.sample_rate as f32
    }

    fn frames_from_ms(&self, ms: u32) -> u64 {
        self.config.sample_rate as u64 * ms as u64 / 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::stereo_to_i16_interleaved;
    use crate::packet::{AudioPacket, AudioPacketHeader};

    fn packet(sequence: u32, sample_position: u64) -> AudioPacket {
        let frames = vec![
            StereoFrame {
                left: 0.1,
                right: 0.2
            };
            240
        ];
        let payload = stereo_to_i16_interleaved(&frames);
        AudioPacket::new(
            AudioPacketHeader::new(1, sequence, 240, sample_position, 0),
            payload,
        )
        .unwrap()
    }

    #[test]
    fn starts_after_threshold_and_outputs_audio() {
        let mut buffer = JitterBuffer::new(JitterConfig {
            start_threshold_ms: 5,
            target_ms: 5,
            ..JitterConfig::default()
        });
        buffer.insert_packet(packet(0, 0), Instant::now());
        let mut out = vec![0.0; 240 * 2];
        buffer.pull_f32(&mut out);
        assert_eq!(buffer.metrics().state, JitterState::Running);
        assert!(out.iter().any(|sample| *sample != 0.0));
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
    fn starts_near_target_latency_when_over_primed() {
        let mut buffer = JitterBuffer::new(JitterConfig {
            start_threshold_ms: 50,
            target_ms: 100,
            ..JitterConfig::default()
        });
        for sequence in 0..200 {
            buffer.insert_packet(packet(sequence, sequence as u64 * 240), Instant::now());
        }

        let mut out = vec![0.0; 240 * 2];
        buffer.pull_f32(&mut out);

        assert_eq!(buffer.metrics().state, JitterState::Running);
        assert!(buffer.metrics().buffer_level_ms <= 110.0);
    }

    #[test]
    fn consumes_48k_stream_at_44k1_output_rate() {
        let mut buffer = JitterBuffer::new(JitterConfig {
            start_threshold_ms: 50,
            target_ms: 100,
            adaptive_resampling: false,
            ..JitterConfig::default()
        });
        for sequence in 0..200 {
            buffer.insert_packet(packet(sequence, sequence as u64 * 240), Instant::now());
        }

        let mut out = vec![0.0; 441 * 2];
        buffer.pull_f32_at_sample_rate(&mut out, 44_100);
        assert_eq!(buffer.metrics().state, JitterState::Running);
        assert!(buffer.metrics().buffer_level_ms <= 100.0);

        buffer.insert_packet(packet(200, 200 * 240), Instant::now());
        buffer.insert_packet(packet(201, 201 * 240), Instant::now());
        let before = buffer.metrics().buffer_level_ms;
        buffer.pull_f32_at_sample_rate(&mut out, 44_100);
        let after = buffer.metrics().buffer_level_ms;

        assert_eq!(buffer.metrics().state, JitterState::Running);
        assert!((before - after - 10.0).abs() < 0.5);
        assert!((buffer.metrics().resample_ratio - (48_000.0 / 44_100.0)).abs() < 0.0001);
    }
}
