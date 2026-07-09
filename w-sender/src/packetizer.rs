use anyhow::{bail, Result};
use lan_audio_common::audio::{f32_to_i16, i16_to_f32, SAMPLE_RATE};
use lan_audio_common::packet::{write_i16_packet_bytes, AudioPacketHeader, HEADER_SIZE};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleEncoding {
    F32,
    I16,
    I24,
    I32,
}

#[derive(Clone, Debug)]
pub struct InputFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub block_align: u16,
    pub encoding: SampleEncoding,
}

impl InputFormat {
    pub fn bytes_per_sample(&self) -> usize {
        usize::from(self.bits_per_sample / 8)
    }

    pub fn describe(&self) -> String {
        format!(
            "{}Hz/{}ch/{:?}/{}bit block_align={}",
            self.sample_rate, self.channels, self.encoding, self.bits_per_sample, self.block_align
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ChunkStats {
    pub packets: usize,
    pub rms_left_db: f32,
    pub rms_right_db: f32,
}

pub struct Packetizer {
    stream_id: u64,
    sequence: u32,
    sample_position: u64,
    start: Instant,
    max_packet_frames: usize,
    packet_samples: Vec<i16>,
    packet_bytes: Vec<u8>,
}

impl Packetizer {
    pub fn new(max_packet_frames: usize) -> Self {
        Self {
            stream_id: new_stream_id(),
            sequence: 0,
            sample_position: 0,
            start: Instant::now(),
            max_packet_frames,
            packet_samples: Vec::with_capacity(max_packet_frames * 2),
            packet_bytes: Vec::with_capacity(HEADER_SIZE + max_packet_frames * 4),
        }
    }

    pub fn packetize_capture_chunk<F>(
        &mut self,
        data: Option<&[u8]>,
        frames: u32,
        format: &InputFormat,
        mut on_packet: F,
    ) -> Result<ChunkStats>
    where
        F: FnMut(&[u8]),
    {
        if frames == 0 {
            return Ok(ChunkStats::default());
        }
        if format.sample_rate != SAMPLE_RATE {
            bail!(
                "w-sender only supports {}Hz capture today; got {}Hz",
                SAMPLE_RATE,
                format.sample_rate
            );
        }
        if format.channels == 0 {
            bail!("capture format has zero channels");
        }
        if data.is_some() && format.bytes_per_sample() == 0 {
            bail!("capture format has zero bytes per sample");
        }

        let mut stats = ChunkStats {
            rms_left_db: -120.0,
            rms_right_db: -120.0,
            ..ChunkStats::default()
        };
        let mut sum_l = 0.0f64;
        let mut sum_r = 0.0f64;
        let mut base_frame = 0usize;
        let total_frames = frames as usize;

        while base_frame < total_frames {
            let packet_frames = (total_frames - base_frame).min(self.max_packet_frames);
            self.packet_samples.clear();
            for frame_index in base_frame..base_frame + packet_frames {
                let (left_i16, right_i16, left_f32, right_f32) =
                    read_stereo_frame(data, frame_index, format)?;
                self.packet_samples.push(left_i16);
                self.packet_samples.push(right_i16);
                sum_l += f64::from(left_f32) * f64::from(left_f32);
                sum_r += f64::from(right_f32) * f64::from(right_f32);
            }

            let header = AudioPacketHeader::new(
                self.stream_id,
                self.sequence,
                packet_frames as u16,
                self.sample_position,
                self.start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            );
            write_i16_packet_bytes(&mut self.packet_bytes, &header, &self.packet_samples)?;
            on_packet(&self.packet_bytes);
            self.sequence = self.sequence.wrapping_add(1);
            self.sample_position += packet_frames as u64;
            stats.packets += 1;
            base_frame += packet_frames;
        }

        stats.rms_left_db = rms_db(sum_l, total_frames);
        stats.rms_right_db = rms_db(sum_r, total_frames);
        Ok(stats)
    }
}

fn read_stereo_frame(
    data: Option<&[u8]>,
    frame_index: usize,
    format: &InputFormat,
) -> Result<(i16, i16, f32, f32)> {
    let Some(data) = data else {
        return Ok((0, 0, 0.0, 0.0));
    };

    let left = read_channel_sample(data, frame_index, 0, format)?;
    let right = if format.channels == 1 {
        left
    } else {
        read_channel_sample(data, frame_index, 1, format)?
    };
    Ok((f32_to_i16(left), f32_to_i16(right), left, right))
}

fn read_channel_sample(
    data: &[u8],
    frame_index: usize,
    channel_index: usize,
    format: &InputFormat,
) -> Result<f32> {
    let bytes_per_sample = format.bytes_per_sample();
    let frame_offset = frame_index
        .checked_mul(usize::from(format.block_align))
        .ok_or_else(|| anyhow::anyhow!("capture frame offset overflow"))?;
    let sample_offset = frame_offset
        .checked_add(channel_index.saturating_mul(bytes_per_sample))
        .ok_or_else(|| anyhow::anyhow!("capture sample offset overflow"))?;
    let sample = data
        .get(sample_offset..sample_offset + bytes_per_sample)
        .ok_or_else(|| anyhow::anyhow!("capture buffer shorter than expected"))?;

    Ok(match format.encoding {
        SampleEncoding::F32 => f32::from_le_bytes(sample.try_into().expect("f32 sample size")),
        SampleEncoding::I16 => i16_to_f32(i16::from_le_bytes(
            sample.try_into().expect("i16 sample size"),
        )),
        SampleEncoding::I24 => i24_to_f32(sample),
        SampleEncoding::I32 => i32_to_f32(i32::from_le_bytes(
            sample.try_into().expect("i32 sample size"),
        )),
    }
    .clamp(-1.0, 1.0))
}

fn i24_to_f32(bytes: &[u8]) -> f32 {
    let raw = i32::from_le_bytes([
        bytes[0],
        bytes[1],
        bytes[2],
        if bytes[2] & 0x80 == 0 { 0x00 } else { 0xff },
    ]);
    (raw as f32 / 8_388_607.0).clamp(-1.0, 1.0)
}

fn i32_to_f32(sample: i32) -> f32 {
    if sample == i32::MIN {
        -1.0
    } else {
        sample as f32 / i32::MAX as f32
    }
}

fn rms_db(sum: f64, frames: usize) -> f32 {
    if frames == 0 {
        return -120.0;
    }
    let rms = (sum / frames as f64).sqrt().max(1.0e-6);
    20.0 * (rms as f32).log10()
}

fn new_stream_id() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    (now as u64) ^ ((now >> 64) as u64)
}
