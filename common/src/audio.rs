pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
pub const SAMPLE_FORMAT_S16LE: u16 = 1;
pub const FRAME_BYTES: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StereoFrame {
    pub left: f32,
    pub right: f32,
}

impl StereoFrame {
    pub const SILENCE: Self = Self {
        left: 0.0,
        right: 0.0,
    };

    pub fn lerp(self, other: Self, frac: f32) -> Self {
        Self {
            left: self.left + (other.left - self.left) * frac,
            right: self.right + (other.right - self.right) * frac,
        }
    }
}

pub fn f32_to_i16(sample: f32) -> i16 {
    if sample <= -1.0 {
        i16::MIN
    } else if sample >= 1.0 {
        i16::MAX
    } else {
        (sample * 32767.0).round() as i16
    }
}

pub fn i16_to_f32(sample: i16) -> f32 {
    if sample == i16::MIN {
        -1.0
    } else {
        sample as f32 / 32767.0
    }
}

pub fn stereo_to_i16_interleaved(frames: &[StereoFrame]) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames.len() * 2);
    for frame in frames {
        out.push(f32_to_i16(frame.left));
        out.push(f32_to_i16(frame.right));
    }
    out
}

pub fn i16_interleaved_to_stereo(samples: &[i16], channels: u16) -> Vec<StereoFrame> {
    let channels = channels as usize;
    if channels == 0 {
        return Vec::new();
    }

    samples
        .chunks_exact(channels)
        .map(|frame| {
            let left = frame.first().copied().unwrap_or_default();
            let right = if channels == 1 {
                left
            } else {
                frame.get(1).copied().unwrap_or(left)
            };
            StereoFrame {
                left: i16_to_f32(left),
                right: i16_to_f32(right),
            }
        })
        .collect()
}

pub fn rms_db(frames: &[StereoFrame]) -> (f32, f32) {
    if frames.is_empty() {
        return (-120.0, -120.0);
    }

    let (sum_l, sum_r) = frames.iter().fold((0.0f64, 0.0f64), |(l, r), frame| {
        (
            l + (frame.left as f64 * frame.left as f64),
            r + (frame.right as f64 * frame.right as f64),
        )
    });
    let len = frames.len() as f64;
    let l = (sum_l / len).sqrt().max(1.0e-6);
    let r = (sum_r / len).sqrt().max(1.0e-6);
    (20.0 * (l as f32).log10(), 20.0 * (r as f32).log10())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_i16_conversion_clamps() {
        assert_eq!(f32_to_i16(-2.0), i16::MIN);
        assert_eq!(f32_to_i16(2.0), i16::MAX);
        assert_eq!(f32_to_i16(0.0), 0);
    }

    #[test]
    fn mono_samples_are_duplicated_to_stereo() {
        let frames = i16_interleaved_to_stereo(&[1000, -1000], 1);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].left, frames[0].right);
        assert_eq!(frames[1].left, frames[1].right);
    }
}
