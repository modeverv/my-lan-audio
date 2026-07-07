use crate::audio::{CHANNELS, SAMPLE_FORMAT_S16LE, SAMPLE_RATE};
use std::fmt;

pub const MAGIC: [u8; 4] = *b"W2MA";
pub const VERSION: u16 = 1;
pub const HEADER_SIZE: usize = 52;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioPacketHeader {
    pub version: u16,
    pub header_size: u16,
    pub stream_id: u64,
    pub sequence: u32,
    pub flags: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: u16,
    pub frames: u16,
    pub reserved: u16,
    pub sample_position: u64,
    pub send_time_ns: u64,
}

impl AudioPacketHeader {
    pub fn new(
        stream_id: u64,
        sequence: u32,
        frames: u16,
        sample_position: u64,
        send_time_ns: u64,
    ) -> Self {
        Self {
            version: VERSION,
            header_size: HEADER_SIZE as u16,
            stream_id,
            sequence,
            flags: 0,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            sample_format: SAMPLE_FORMAT_S16LE,
            frames,
            reserved: 0,
            sample_position,
            send_time_ns,
        }
    }

    pub fn payload_bytes(&self) -> Result<usize, PacketError> {
        if self.channels == 0 {
            return Err(PacketError::UnsupportedFormat("channels must be non-zero"));
        }
        if self.sample_format != SAMPLE_FORMAT_S16LE {
            return Err(PacketError::UnsupportedFormat(
                "only s16le payloads are supported",
            ));
        }

        Ok(self.frames as usize * self.channels as usize * 2)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioPacket {
    pub header: AudioPacketHeader,
    pub payload: Vec<i16>,
}

impl AudioPacket {
    pub fn new(header: AudioPacketHeader, payload: Vec<i16>) -> Result<Self, PacketError> {
        let expected_samples = header.frames as usize * header.channels as usize;
        if payload.len() != expected_samples {
            return Err(PacketError::PayloadSize {
                expected: expected_samples * 2,
                actual: payload.len() * 2,
            });
        }

        Ok(Self { header, payload })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_SIZE + self.payload.len() * 2);
        out.extend_from_slice(&MAGIC);
        write_u16(&mut out, self.header.version);
        write_u16(&mut out, self.header.header_size);
        write_u64(&mut out, self.header.stream_id);
        write_u32(&mut out, self.header.sequence);
        write_u32(&mut out, self.header.flags);
        write_u32(&mut out, self.header.sample_rate);
        write_u16(&mut out, self.header.channels);
        write_u16(&mut out, self.header.sample_format);
        write_u16(&mut out, self.header.frames);
        write_u16(&mut out, self.header.reserved);
        write_u64(&mut out, self.header.sample_position);
        write_u64(&mut out, self.header.send_time_ns);

        for sample in &self.payload {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.len() < HEADER_SIZE {
            return Err(PacketError::TooShort {
                expected: HEADER_SIZE,
                actual: bytes.len(),
            });
        }
        if bytes[0..4] != MAGIC {
            return Err(PacketError::InvalidMagic);
        }

        let mut cursor = 4;
        let version = read_u16(bytes, &mut cursor)?;
        if version != VERSION {
            return Err(PacketError::InvalidVersion(version));
        }
        let header_size = read_u16(bytes, &mut cursor)?;
        if header_size as usize != HEADER_SIZE {
            return Err(PacketError::InvalidHeaderSize(header_size));
        }

        let header = AudioPacketHeader {
            version,
            header_size,
            stream_id: read_u64(bytes, &mut cursor)?,
            sequence: read_u32(bytes, &mut cursor)?,
            flags: read_u32(bytes, &mut cursor)?,
            sample_rate: read_u32(bytes, &mut cursor)?,
            channels: read_u16(bytes, &mut cursor)?,
            sample_format: read_u16(bytes, &mut cursor)?,
            frames: read_u16(bytes, &mut cursor)?,
            reserved: read_u16(bytes, &mut cursor)?,
            sample_position: read_u64(bytes, &mut cursor)?,
            send_time_ns: read_u64(bytes, &mut cursor)?,
        };

        if header.sample_rate != SAMPLE_RATE
            || header.channels != CHANNELS
            || header.sample_format != SAMPLE_FORMAT_S16LE
        {
            return Err(PacketError::UnsupportedPacketFormat {
                sample_rate: header.sample_rate,
                channels: header.channels,
                sample_format: header.sample_format,
            });
        }

        let expected_payload_bytes = header.payload_bytes()?;
        let actual_payload_bytes = bytes.len() - HEADER_SIZE;
        if actual_payload_bytes != expected_payload_bytes {
            return Err(PacketError::PayloadSize {
                expected: expected_payload_bytes,
                actual: actual_payload_bytes,
            });
        }

        let mut payload = Vec::with_capacity(expected_payload_bytes / 2);
        for chunk in bytes[HEADER_SIZE..].chunks_exact(2) {
            payload.push(i16::from_le_bytes([chunk[0], chunk[1]]));
        }

        Ok(Self { header, payload })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketError {
    TooShort {
        expected: usize,
        actual: usize,
    },
    InvalidMagic,
    InvalidVersion(u16),
    InvalidHeaderSize(u16),
    UnsupportedFormat(&'static str),
    UnsupportedPacketFormat {
        sample_rate: u32,
        channels: u16,
        sample_format: u16,
    },
    PayloadSize {
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { expected, actual } => {
                write!(f, "packet too short: expected at least {expected} bytes, got {actual}")
            }
            Self::InvalidMagic => write!(f, "invalid packet magic"),
            Self::InvalidVersion(version) => write!(f, "unsupported packet version {version}"),
            Self::InvalidHeaderSize(size) => write!(f, "invalid header size {size}"),
            Self::UnsupportedFormat(message) => write!(f, "{message}"),
            Self::UnsupportedPacketFormat {
                sample_rate,
                channels,
                sample_format,
            } => write!(
                f,
                "unsupported packet format: sample_rate={sample_rate}, channels={channels}, format={sample_format}"
            ),
            Self::PayloadSize { expected, actual } => {
                write!(f, "invalid payload size: expected {expected} bytes, got {actual}")
            }
        }
    }
}

impl std::error::Error for PacketError {}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn read_u16(bytes: &[u8], cursor: &mut usize) -> Result<u16, PacketError> {
    let end = *cursor + 2;
    let value = bytes
        .get(*cursor..end)
        .ok_or(PacketError::TooShort {
            expected: end,
            actual: bytes.len(),
        })?
        .try_into()
        .expect("u16 slice length");
    *cursor = end;
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, PacketError> {
    let end = *cursor + 4;
    let value = bytes
        .get(*cursor..end)
        .ok_or(PacketError::TooShort {
            expected: end,
            actual: bytes.len(),
        })?
        .try_into()
        .expect("u32 slice length");
    *cursor = end;
    Ok(u32::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64, PacketError> {
    let end = *cursor + 8;
    let value = bytes
        .get(*cursor..end)
        .ok_or(PacketError::TooShort {
            expected: end,
            actual: bytes.len(),
        })?
        .try_into()
        .expect("u64 slice length");
    *cursor = end;
    Ok(u64::from_le_bytes(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_packet() -> AudioPacket {
        let header = AudioPacketHeader::new(7, 42, 2, 480, 123);
        AudioPacket::new(header, vec![1, 2, 3, 4]).unwrap()
    }

    #[test]
    fn round_trips_packet_bytes() {
        let packet = test_packet();
        let bytes = packet.to_bytes();
        assert_eq!(bytes.len(), HEADER_SIZE + 8);
        assert_eq!(AudioPacket::from_bytes(&bytes).unwrap(), packet);
    }

    #[test]
    fn rejects_invalid_magic() {
        let mut bytes = test_packet().to_bytes();
        bytes[0] = b'X';
        assert_eq!(
            AudioPacket::from_bytes(&bytes),
            Err(PacketError::InvalidMagic)
        );
    }

    #[test]
    fn rejects_invalid_version() {
        let mut bytes = test_packet().to_bytes();
        bytes[4] = 2;
        assert_eq!(
            AudioPacket::from_bytes(&bytes),
            Err(PacketError::InvalidVersion(2))
        );
    }

    #[test]
    fn validates_payload_size() {
        let mut bytes = test_packet().to_bytes();
        bytes.pop();
        assert!(matches!(
            AudioPacket::from_bytes(&bytes),
            Err(PacketError::PayloadSize { .. })
        ));
    }
}
