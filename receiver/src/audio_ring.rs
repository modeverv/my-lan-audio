use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

#[derive(Debug)]
pub struct SpscF32Ring {
    buffer: Vec<AtomicU32>,
    capacity_samples: usize,
    read_index: AtomicUsize,
    write_index: AtomicUsize,
}

impl SpscF32Ring {
    pub fn new(capacity_samples: usize) -> Self {
        assert!(capacity_samples > 0, "ring capacity must be non-zero");
        let buffer = (0..capacity_samples)
            .map(|_| AtomicU32::new(0.0f32.to_bits()))
            .collect();
        Self {
            buffer,
            capacity_samples,
            read_index: AtomicUsize::new(0),
            write_index: AtomicUsize::new(0),
        }
    }

    pub fn len_samples(&self) -> usize {
        let write = self.write_index.load(Ordering::Acquire);
        let read = self.read_index.load(Ordering::Acquire);
        write.saturating_sub(read).min(self.capacity_samples)
    }

    pub fn push_interleaved(&self, samples: &[f32], channels: usize) -> usize {
        let channels = channels.max(1);
        let frame_aligned_len = samples.len() / channels * channels;
        if frame_aligned_len == 0 {
            return 0;
        }

        let read = self.read_index.load(Ordering::Acquire);
        let write = self.write_index.load(Ordering::Relaxed);
        let used = write.saturating_sub(read).min(self.capacity_samples);
        let free = self.capacity_samples - used;
        let write_len = frame_aligned_len.min(free / channels * channels);
        for (offset, sample) in samples.iter().take(write_len).enumerate() {
            let index = (write + offset) % self.capacity_samples;
            self.buffer[index].store(sample.to_bits(), Ordering::Relaxed);
        }
        self.write_index.store(write + write_len, Ordering::Release);
        write_len
    }

    pub fn pop_interleaved(&self, output: &mut [f32], channels: usize) -> usize {
        let channels = channels.max(1);
        let frame_aligned_len = output.len() / channels * channels;
        if frame_aligned_len == 0 {
            return 0;
        }

        let write = self.write_index.load(Ordering::Acquire);
        let read = self.read_index.load(Ordering::Relaxed);
        let available = write.saturating_sub(read).min(self.capacity_samples);
        let read_len = frame_aligned_len.min(available / channels * channels);
        for (offset, sample) in output.iter_mut().take(read_len).enumerate() {
            let index = (read + offset) % self.capacity_samples;
            *sample = f32::from_bits(self.buffer[index].load(Ordering::Relaxed));
        }
        self.read_index.store(read + read_len, Ordering::Release);
        read_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_and_pops_samples_in_order() {
        let ring = SpscF32Ring::new(8);
        assert_eq!(ring.push_interleaved(&[1.0, 2.0, 3.0, 4.0], 2), 4);
        assert_eq!(ring.len_samples(), 4);

        let mut out = [0.0; 4];
        assert_eq!(ring.pop_interleaved(&mut out, 2), 4);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ring.len_samples(), 0);
    }

    #[test]
    fn wraps_without_reordering() {
        let ring = SpscF32Ring::new(6);
        assert_eq!(ring.push_interleaved(&[1.0, 2.0, 3.0, 4.0], 2), 4);
        let mut first = [0.0; 2];
        assert_eq!(ring.pop_interleaved(&mut first, 2), 2);
        assert_eq!(first, [1.0, 2.0]);

        assert_eq!(ring.push_interleaved(&[5.0, 6.0, 7.0, 8.0], 2), 4);
        let mut rest = [0.0; 6];
        assert_eq!(ring.pop_interleaved(&mut rest, 2), 6);
        assert_eq!(rest, [3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn preserves_frame_alignment() {
        let ring = SpscF32Ring::new(5);
        assert_eq!(ring.push_interleaved(&[1.0, 2.0, 3.0], 2), 2);
        let mut out = [0.0; 3];
        assert_eq!(ring.pop_interleaved(&mut out, 2), 2);
        assert_eq!(out, [1.0, 2.0, 0.0]);
    }
}
