//! Real low-frequency energy of a synthesized utterance.
//!
//! The TUI's equalizer component can either play its synthetic bass rhythm or
//! follow the voice it is speaking. This module produces the latter: for every
//! animation frame it measures the RMS energy of the samples below a set of low
//! cutoffs (the "bass bands" the bars stand for).
//!
//! There is deliberately no FFT dependency: the bars only need a coarse,
//! stable per-band level, and a cascade of one-pole low-pass filters gives a
//! real measurement of the low end for a fraction of the cost. The envelope is
//! normalized to its own peak, so a quiet recording still animates fully.

/// Per-frame bass levels, frame-major (`values[frame * bars + bar]`).
#[derive(Debug, Clone, PartialEq)]
pub struct BassEnvelope {
    /// Frames per second the levels were sampled at.
    pub fps: u32,
    /// Number of bands per frame.
    pub bars: usize,
    /// Normalized levels in `[0.0, 1.0]`.
    pub values: Vec<f32>,
}

impl BassEnvelope {
    /// How long the envelope lasts.
    #[must_use]
    pub fn duration_secs(&self) -> f32 {
        if self.fps == 0 {
            return 0.0;
        }
        self.values.len() as f32 / (self.fps as f32 * self.bars as f32)
    }

    /// The levels for `frame`, clamped to the last frame.
    #[must_use]
    pub fn frame(&self, frame: usize) -> Vec<f32> {
        if self.bars == 0 || self.values.is_empty() {
            return Vec::new();
        }
        let frames = self.values.len() / self.bars;
        let frame = frame.min(frames.saturating_sub(1));
        let start = frame * self.bars;
        self.values[start..start + self.bars].to_vec()
    }
}

/// Lowest band cutoff in Hz.
const MIN_CUTOFF_HZ: f32 = 60.0;

/// Highest band cutoff in Hz - still well inside the bass/bass-drum range, so
/// consonants and sibilance do not drive the bars.
const MAX_CUTOFF_HZ: f32 = 400.0;

/// Measure the bass envelope of `samples` (mono, `sample_rate` Hz).
///
/// `bars` bands are spread logarithmically between [`MIN_CUTOFF_HZ`] and
/// [`MAX_CUTOFF_HZ`]; each frame's level is the RMS of that band's filtered
/// signal over the frame window, normalized to the envelope's peak.
#[must_use]
pub fn bass_envelope(samples: &[f32], sample_rate: u32, fps: u32, bars: usize) -> BassEnvelope {
    if samples.is_empty() || sample_rate == 0 || fps == 0 || bars == 0 {
        return BassEnvelope {
            fps,
            bars,
            values: Vec::new(),
        };
    }
    let sample_rate = sample_rate as f32;
    let samples_per_frame = (sample_rate / fps as f32).max(1.0) as usize;
    let frames = samples.len().div_ceil(samples_per_frame);
    let mut values = vec![0.0f32; frames * bars];

    // Log-spaced low-pass cutoffs: bar 0 is the deepest bass, the last bar
    // the upper edge of the low range. Each bar's signal is the difference
    // between neighbouring low-pass outputs - a band-pass - not the low-pass
    // itself: the latter would make every wider band contain all the energy
    // of the narrower ones, so the bars could only ever grow to the right.
    let mut alphas = Vec::with_capacity(bars);
    for bar in 0..bars {
        let t = if bars == 1 {
            0.0
        } else {
            bar as f32 / (bars - 1) as f32
        };
        let cutoff = MIN_CUTOFF_HZ * (MAX_CUTOFF_HZ / MIN_CUTOFF_HZ).powf(t);
        let dt = 1.0 / sample_rate;
        let rc = 1.0 / (std::f32::consts::TAU * cutoff);
        alphas.push(dt / (rc + dt));
    }

    let mut states = vec![0.0f32; bars];
    let mut frame = 0usize;
    let mut sum_sq = vec![0.0f32; bars];
    let mut count = 0usize;
    for &sample in samples {
        for (state, alpha) in states.iter_mut().zip(&alphas) {
            *state += alpha * (sample - *state);
        }
        for bar in 0..bars {
            let band = if bar == 0 {
                states[0]
            } else {
                states[bar] - states[bar - 1]
            };
            sum_sq[bar] += band * band;
        }
        count += 1;
        if count == samples_per_frame {
            for bar in 0..bars {
                values[frame * bars + bar] = (sum_sq[bar] / count as f32).sqrt();
                sum_sq[bar] = 0.0;
            }
            frame += 1;
            count = 0;
        }
    }
    if count > 0 && frame < frames {
        for bar in 0..bars {
            values[frame * bars + bar] = (sum_sq[bar] / count as f32).sqrt();
        }
    }

    // Normalize to the loudest measurement so quiet speech still fills the
    // bars. A silent utterance stays at zero.
    let peak = values.iter().copied().fold(0.0f32, f32::max);
    if peak > 0.0 {
        for value in &mut values {
            *value = (*value / peak).clamp(0.0, 1.0);
        }
    }

    BassEnvelope { fps, bars, values }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A loud 100 Hz tone must light up the low bars more than the high ones,
    /// while a 1 kHz tone must barely move the bass at all.
    #[test]
    fn only_low_frequencies_drive_the_bars() {
        let sample_rate = 24_000;
        let samples: Vec<f32> = (0..sample_rate as usize)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                (std::f32::consts::TAU * 100.0 * t).sin()
            })
            .collect();
        let envelope = bass_envelope(&samples, sample_rate, 30, 8);
        assert!(!envelope.values.is_empty());
        let frame = envelope.frame(10);
        assert_eq!(frame.len(), 8);
        // 100 Hz lands in the low bands, not the top of the range.
        let peak_band = frame
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        assert!(peak_band <= 3, "100 Hz peaks in a low band: {frame:?}");
        assert!(
            frame[0] > 0.2,
            "the deepest band must see the tone: {frame:?}"
        );
        assert!(
            frame[7] < frame[peak_band],
            "top band must trail the tone's band: {frame:?}"
        );

        // A 60 Hz tone sits in the deepest band only.
        let deep: Vec<f32> = (0..sample_rate as usize)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                (std::f32::consts::TAU * 60.0 * t).sin()
            })
            .collect();
        let deep_env = bass_envelope(&deep, sample_rate, 30, 8);
        let deep_frame = deep_env.frame(10);
        let deep_peak = deep_frame
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(
            deep_peak, 0,
            "60 Hz belongs to the deepest band: {deep_frame:?}"
        );
        assert!(
            deep_frame[7] < deep_frame[0] * 0.5,
            "60 Hz must not fill the upper bands: {deep_frame:?}"
        );
    }

    #[test]
    fn silence_and_empty_input_stay_flat() {
        assert!(bass_envelope(&[], 24_000, 30, 8).values.is_empty());
        let silent = vec![0.0f32; 24_000];
        let envelope = bass_envelope(&silent, 24_000, 30, 8);
        assert!(envelope.values.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn frame_layout_is_frame_major_and_clamped() {
        let samples: Vec<f32> = (0..4_800).map(|i| (i as f32 / 20.0).sin() * 0.5).collect();
        let envelope = bass_envelope(&samples, 24_000, 30, 8);
        // 0.2 s at 30 fps → 6 frames; 800 samples/frame.
        assert_eq!(envelope.values.len(), 6 * 8);
        assert_eq!(envelope.frame(0).len(), 8);
        // Past the end clamps to the last frame instead of panicking.
        assert_eq!(envelope.frame(usize::MAX), envelope.frame(5));
        assert!((envelope.duration_secs() - 0.2).abs() < 0.01);
    }
}
