use std::fmt;

pub const STATUS_MAGIC: [u8; 4] = *b"W2MS";
pub const STATUS_VERSION: u16 = 1;
pub const STATUS_SIZE: usize = 108;

#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverStatus {
    pub stream_id: u64,
    pub latest_sequence: u32,
    pub target_ms: u32,
    pub output_sample_rate: u32,
    pub received_packets: u64,
    pub steady_underruns: u64,
    pub startup_underruns: u64,
    pub callback_lock_misses: u64,
    pub latency_trims: u64,
    pub resyncs: u64,
    pub scratch_overflows: u64,
    pub audio_latency_ms: f32,
    pub correction_ppm: f32,
    pub effective_ratio: f32,
    pub receiver_time_ns: u64,
}

impl ReceiverStatus {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(STATUS_SIZE);
        out.extend_from_slice(&STATUS_MAGIC);
        write_u16(&mut out, STATUS_VERSION);
        write_u16(&mut out, STATUS_SIZE as u16);
        write_u64(&mut out, self.stream_id);
        write_u32(&mut out, self.latest_sequence);
        write_u32(&mut out, self.target_ms);
        write_u32(&mut out, self.output_sample_rate);
        write_u32(&mut out, 0);
        write_u64(&mut out, self.received_packets);
        write_u64(&mut out, self.steady_underruns);
        write_u64(&mut out, self.startup_underruns);
        write_u64(&mut out, self.callback_lock_misses);
        write_u64(&mut out, self.latency_trims);
        write_u64(&mut out, self.resyncs);
        write_u64(&mut out, self.scratch_overflows);
        write_f32(&mut out, self.audio_latency_ms);
        write_f32(&mut out, self.correction_ppm);
        write_f32(&mut out, self.effective_ratio);
        write_u64(&mut out, self.receiver_time_ns);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StatusError> {
        if bytes.len() < STATUS_SIZE {
            return Err(StatusError::TooShort {
                expected: STATUS_SIZE,
                actual: bytes.len(),
            });
        }
        if bytes[0..4] != STATUS_MAGIC {
            return Err(StatusError::InvalidMagic);
        }

        let mut cursor = 4;
        let version = read_u16(bytes, &mut cursor)?;
        if version != STATUS_VERSION {
            return Err(StatusError::InvalidVersion(version));
        }
        let size = read_u16(bytes, &mut cursor)?;
        if size as usize != STATUS_SIZE {
            return Err(StatusError::InvalidSize(size));
        }

        let stream_id = read_u64(bytes, &mut cursor)?;
        let latest_sequence = read_u32(bytes, &mut cursor)?;
        let target_ms = read_u32(bytes, &mut cursor)?;
        let output_sample_rate = read_u32(bytes, &mut cursor)?;
        let _reserved = read_u32(bytes, &mut cursor)?;
        let received_packets = read_u64(bytes, &mut cursor)?;
        let steady_underruns = read_u64(bytes, &mut cursor)?;
        let startup_underruns = read_u64(bytes, &mut cursor)?;
        let callback_lock_misses = read_u64(bytes, &mut cursor)?;
        let latency_trims = read_u64(bytes, &mut cursor)?;
        let resyncs = read_u64(bytes, &mut cursor)?;
        let scratch_overflows = read_u64(bytes, &mut cursor)?;
        let audio_latency_ms = read_f32(bytes, &mut cursor)?;
        let correction_ppm = read_f32(bytes, &mut cursor)?;
        let effective_ratio = read_f32(bytes, &mut cursor)?;
        let receiver_time_ns = read_u64(bytes, &mut cursor)?;

        Ok(Self {
            stream_id,
            latest_sequence,
            target_ms,
            output_sample_rate,
            received_packets,
            steady_underruns,
            startup_underruns,
            callback_lock_misses,
            latency_trims,
            resyncs,
            scratch_overflows,
            audio_latency_ms,
            correction_ppm,
            effective_ratio,
            receiver_time_ns,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusError {
    TooShort { expected: usize, actual: usize },
    InvalidMagic,
    InvalidVersion(u16),
    InvalidSize(u16),
}

impl fmt::Display for StatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { expected, actual } => {
                write!(
                    f,
                    "status packet too short: expected {expected} bytes, got {actual}"
                )
            }
            Self::InvalidMagic => write!(f, "invalid status packet magic"),
            Self::InvalidVersion(version) => {
                write!(f, "unsupported status packet version {version}")
            }
            Self::InvalidSize(size) => write!(f, "invalid status packet size {size}"),
        }
    }
}

impl std::error::Error for StatusError {}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn read_u16(bytes: &[u8], cursor: &mut usize) -> Result<u16, StatusError> {
    let end = *cursor + 2;
    let value = bytes
        .get(*cursor..end)
        .ok_or(StatusError::TooShort {
            expected: end,
            actual: bytes.len(),
        })?
        .try_into()
        .expect("u16 slice length");
    *cursor = end;
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, StatusError> {
    let end = *cursor + 4;
    let value = bytes
        .get(*cursor..end)
        .ok_or(StatusError::TooShort {
            expected: end,
            actual: bytes.len(),
        })?
        .try_into()
        .expect("u32 slice length");
    *cursor = end;
    Ok(u32::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64, StatusError> {
    let end = *cursor + 8;
    let value = bytes
        .get(*cursor..end)
        .ok_or(StatusError::TooShort {
            expected: end,
            actual: bytes.len(),
        })?
        .try_into()
        .expect("u64 slice length");
    *cursor = end;
    Ok(u64::from_le_bytes(value))
}

fn read_f32(bytes: &[u8], cursor: &mut usize) -> Result<f32, StatusError> {
    let end = *cursor + 4;
    let value = bytes
        .get(*cursor..end)
        .ok_or(StatusError::TooShort {
            expected: end,
            actual: bytes.len(),
        })?
        .try_into()
        .expect("f32 slice length");
    *cursor = end;
    Ok(f32::from_le_bytes(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_receiver_status() {
        let status = ReceiverStatus {
            stream_id: 7,
            latest_sequence: 42,
            target_ms: 80,
            output_sample_rate: 48_000,
            received_packets: 100,
            steady_underruns: 1,
            startup_underruns: 2,
            callback_lock_misses: 3,
            latency_trims: 4,
            resyncs: 5,
            scratch_overflows: 6,
            audio_latency_ms: 78.5,
            correction_ppm: 12.25,
            effective_ratio: 1.000012,
            receiver_time_ns: 123_456,
        };

        let bytes = status.to_bytes();
        assert_eq!(bytes.len(), STATUS_SIZE);
        assert_eq!(ReceiverStatus::from_bytes(&bytes).unwrap(), status);
    }
}
