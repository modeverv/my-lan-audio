use crate::audio::StereoFrame;

pub fn resample_linear(
    input: &[StereoFrame],
    source_rate: u32,
    target_rate: u32,
) -> Vec<StereoFrame> {
    if input.is_empty() || source_rate == 0 || target_rate == 0 {
        return Vec::new();
    }
    if source_rate == target_rate {
        return input.to_vec();
    }
    if input.len() == 1 {
        return vec![input[0]];
    }

    let ratio = source_rate as f64 / target_rate as f64;
    let output_len = ((input.len() as f64) / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(output_len);
    let mut pos = 0.0f64;

    while output.len() < output_len {
        let i0 = pos.floor() as usize;
        if i0 + 1 >= input.len() {
            break;
        }
        let frac = (pos - i0 as f64) as f32;
        output.push(input[i0].lerp(input[i0 + 1], frac));
        pos += ratio;
    }

    output
}

#[derive(Debug)]
pub struct StreamingLinearResampler {
    ratio: f64,
    read_pos: f64,
    buffer: Vec<StereoFrame>,
}

impl StreamingLinearResampler {
    pub fn new(source_rate: u32, target_rate: u32) -> Self {
        Self {
            ratio: source_rate as f64 / target_rate as f64,
            read_pos: 0.0,
            buffer: Vec::new(),
        }
    }

    pub fn set_ratio(&mut self, ratio: f64) {
        if ratio.is_finite() && ratio > 0.0 {
            self.ratio = ratio;
        }
    }

    pub fn set_effective_target_rate(&mut self, source_rate: u32, target_rate: f64) {
        if source_rate > 0 && target_rate.is_finite() && target_rate > 0.0 {
            self.set_ratio(source_rate as f64 / target_rate);
        }
    }

    pub fn push(&mut self, input: &[StereoFrame], output: &mut Vec<StereoFrame>) {
        if (self.ratio - 1.0).abs() < f64::EPSILON && self.buffer.is_empty() {
            output.extend_from_slice(input);
            return;
        }

        self.buffer.extend_from_slice(input);

        while self.read_pos + 1.0 < self.buffer.len() as f64 {
            let i0 = self.read_pos.floor() as usize;
            let frac = (self.read_pos - i0 as f64) as f32;
            output.push(self.buffer[i0].lerp(self.buffer[i0 + 1], frac));
            self.read_pos += self.ratio;
        }

        let drop = self.read_pos.floor() as usize;
        if drop > 0 {
            self.buffer.drain(..drop);
            self.read_pos -= drop as f64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_resampler_preserves_equal_rate() {
        let input = vec![
            StereoFrame {
                left: 0.0,
                right: 0.0,
            },
            StereoFrame {
                left: 1.0,
                right: -1.0,
            },
        ];
        assert_eq!(resample_linear(&input, 48_000, 48_000), input);
    }

    #[test]
    fn streaming_resampler_outputs_data() {
        let input = vec![StereoFrame::SILENCE; 100];
        let mut resampler = StreamingLinearResampler::new(44_100, 48_000);
        let mut output = Vec::new();
        resampler.push(&input, &mut output);
        assert!(!output.is_empty());
    }
}
