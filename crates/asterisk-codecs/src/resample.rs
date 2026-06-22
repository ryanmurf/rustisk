//! Sample rate converter - linear interpolation resampler.
//!
//! Port of codecs/codec_resample.c from Asterisk C.
//!
//! Converts between signed linear (SLIN) audio at different sample rates
//! (8kHz, 12kHz, 16kHz, 24kHz, 32kHz, 44.1kHz, 48kHz, 96kHz, 192kHz).
//!
//! The original Asterisk C code uses the Speex resampler library. This
//! Rust port uses basic linear interpolation, which is sufficient for
//! telephony audio and avoids the external dependency.

use crate::builtin_codecs::{
    ID_SLIN8, ID_SLIN12, ID_SLIN16, ID_SLIN24, ID_SLIN32,
    ID_SLIN44, ID_SLIN48, ID_SLIN96, ID_SLIN192,
};
use crate::codec::CodecId;
use crate::translate::{TransCost, TranslateError, Translator, TranslatorInstance};
use asterisk_types::Frame;
use bytes::Bytes;

/// All supported SLIN sample rates with their codec IDs.
pub const SLIN_RATES: &[(u32, CodecId)] = &[
    (8000, ID_SLIN8),
    (12000, ID_SLIN12),
    (16000, ID_SLIN16),
    (24000, ID_SLIN24),
    (32000, ID_SLIN32),
    (44100, ID_SLIN44),
    (48000, ID_SLIN48),
    (96000, ID_SLIN96),
    (192000, ID_SLIN192),
];

/// Output buffer size limit in samples.
const OUTBUF_SAMPLES: usize = 11520;

/// Resample signed-linear 16-bit audio using linear interpolation.
///
/// Input and output are slices of i16 samples. The function computes
/// the ratio `src_rate / dst_rate` and interpolates between adjacent
/// source samples to produce the output.
pub fn resample_linear(input: &[i16], src_rate: u32, dst_rate: u32) -> Vec<i16> {
    if src_rate == dst_rate || input.is_empty() {
        return input.to_vec();
    }

    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = ((input.len() as f64) / ratio).ceil() as usize;
    let out_len = out_len.min(OUTBUF_SAMPLES);
    let mut output = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = src_pos - idx as f64;

        if idx + 1 < input.len() {
            // Linear interpolation between two adjacent samples
            let s0 = input[idx] as f64;
            let s1 = input[idx + 1] as f64;
            let interpolated = s0 + frac * (s1 - s0);
            output.push(interpolated.round().clamp(-32768.0, 32767.0) as i16);
        } else if idx < input.len() {
            output.push(input[idx]);
        } else {
            break;
        }
    }

    output
}

// ---------------------------------------------------------------------------
// Polyphase windowed sinc resampler (higher quality)
// ---------------------------------------------------------------------------

/// Number of zero-crossings on each side of the sinc function.
const SINC_HALF_LEN: usize = 16;

/// Number of sub-sample phases for polyphase filter.
const NUM_PHASES: usize = 256;

/// Windowed sinc (Lanczos) interpolation function.
///
/// This is a higher-quality resampler compared to linear interpolation.
/// It pre-computes filter coefficients for efficient per-sample computation.
fn lanczos_sinc(x: f64, a: f64) -> f64 {
    if x.abs() < 1e-10 {
        return 1.0;
    }
    if x.abs() >= a {
        return 0.0;
    }
    let pi_x = std::f64::consts::PI * x;
    let pi_x_a = std::f64::consts::PI * x / a;
    (pi_x.sin() / pi_x) * (pi_x_a.sin() / pi_x_a)
}

/// Pre-computed polyphase filter bank for a specific rate conversion.
struct PolyphaseFilter {
    /// Filter coefficients: [phase][tap]
    coeffs: Vec<Vec<f64>>,
}

impl PolyphaseFilter {
    /// Create a polyphase filter bank for the given ratio.
    ///
    /// - `ratio`: src_rate / dst_rate
    fn new(ratio: f64) -> Self {
        let num_taps = SINC_HALF_LEN * 2;
        let cutoff = if ratio > 1.0 { 1.0 / ratio } else { 1.0 }; // Anti-aliasing for downsampling
        let mut coeffs = Vec::with_capacity(NUM_PHASES);

        for phase in 0..NUM_PHASES {
            let frac = phase as f64 / NUM_PHASES as f64;
            let mut taps = Vec::with_capacity(num_taps);

            for j in 0..num_taps {
                let n = j as f64 - SINC_HALF_LEN as f64 + frac;
                let w = lanczos_sinc(n * cutoff, SINC_HALF_LEN as f64);
                taps.push(w * cutoff);
            }

            // Normalize filter taps so they sum to 1.0
            let sum: f64 = taps.iter().sum();
            if sum.abs() > 1e-10 {
                for t in taps.iter_mut() {
                    *t /= sum;
                }
            }

            coeffs.push(taps);
        }

        Self { coeffs }
    }
}

/// Resample signed-linear 16-bit audio using windowed sinc interpolation.
///
/// This provides significantly better audio quality than linear interpolation,
/// especially for downsampling (proper anti-aliasing) and larger ratio changes.
pub fn resample_sinc(input: &[i16], src_rate: u32, dst_rate: u32) -> Vec<i16> {
    if src_rate == dst_rate || input.is_empty() {
        return input.to_vec();
    }

    let ratio = src_rate as f64 / dst_rate as f64;
    let filter = PolyphaseFilter::new(ratio);

    let out_len = ((input.len() as f64) / ratio).ceil() as usize;
    let out_len = out_len.min(OUTBUF_SAMPLES);
    let mut output = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let src_idx = src_pos.floor() as i64;
        let frac = src_pos - src_idx as f64;

        // Select the appropriate polyphase filter
        let phase = (frac * NUM_PHASES as f64).min((NUM_PHASES - 1) as f64) as usize;
        let taps = &filter.coeffs[phase];

        let mut sum = 0.0f64;
        for (j, &coeff) in taps.iter().enumerate() {
            let idx = src_idx - SINC_HALF_LEN as i64 + j as i64;
            let sample = if idx >= 0 && (idx as usize) < input.len() {
                input[idx as usize] as f64
            } else {
                0.0 // Zero-pad outside bounds
            };
            sum += sample * coeff;
        }

        output.push(sum.round().clamp(-32768.0, 32767.0) as i16);
    }

    output
}

/// A resampling translator between two SLIN sample rates.
pub struct ResampleTranslator {
    name: String,
    src_rate: u32,
    dst_rate: u32,
    src_codec_id: CodecId,
    dst_codec_id: CodecId,
}

impl ResampleTranslator {
    pub fn new(src_rate: u32, src_id: CodecId, dst_rate: u32, dst_id: CodecId) -> Self {
        Self {
            name: format!("slin {}khz -> {}khz", src_rate / 1000, dst_rate / 1000),
            src_rate,
            dst_rate,
            src_codec_id: src_id,
            dst_codec_id: dst_id,
        }
    }
}

impl Translator for ResampleTranslator {
    fn name(&self) -> &str { &self.name }
    fn src_codec_id(&self) -> CodecId { self.src_codec_id }
    fn dst_codec_id(&self) -> CodecId { self.dst_codec_id }
    fn table_cost(&self) -> u32 {
        if self.src_rate < self.dst_rate {
            TransCost::LL_LL_UPSAMP
        } else {
            TransCost::LL_LL_DOWNSAMP
        }
    }
    fn new_instance(&self) -> Box<dyn TranslatorInstance> {
        Box::new(ResampleInstance {
            src_rate: self.src_rate,
            dst_rate: self.dst_rate,
            dst_codec_id: self.dst_codec_id,
            output_buf: Vec::with_capacity(OUTBUF_SAMPLES * 2),
            samples: 0,
        })
    }
}

struct ResampleInstance {
    src_rate: u32,
    dst_rate: u32,
    dst_codec_id: CodecId,
    output_buf: Vec<u8>,
    samples: u32,
}

impl TranslatorInstance for ResampleInstance {
    fn frame_in(&mut self, frame: &Frame) -> Result<(), TranslateError> {
        let data = match frame {
            Frame::Voice { data, .. } => data,
            _ => return Err(TranslateError::Failed("expected voice frame".into())),
        };

        if data.is_empty() {
            return Err(TranslateError::Failed("empty frame data".into()));
        }

        if data.len() % 2 != 0 {
            return Err(TranslateError::Failed("slin data must have even length".into()));
        }

        // Convert bytes to i16 samples
        let mut input_samples: Vec<i16> = Vec::with_capacity(data.len() / 2);
        let mut i = 0;
        while i + 1 < data.len() {
            input_samples.push(i16::from_le_bytes([data[i], data[i + 1]]));
            i += 2;
        }

        // Resample
        let resampled = resample_linear(&input_samples, self.src_rate, self.dst_rate);

        // Append to output buffer
        for &sample in &resampled {
            self.output_buf.extend_from_slice(&sample.to_le_bytes());
            self.samples += 1;
        }

        Ok(())
    }

    fn frame_out(&mut self) -> Option<Frame> {
        if self.output_buf.is_empty() {
            return None;
        }
        let data = Bytes::from(std::mem::take(&mut self.output_buf));
        let samples = self.samples;
        self.samples = 0;
        Some(Frame::voice(self.dst_codec_id, samples, data))
    }
}

/// Generate all resample translator pairs (N*(N-1) combinations).
///
/// Returns a Vec of Arc<dyn Translator> for registration into a TranslationMatrix.
pub fn all_resample_translators() -> Vec<std::sync::Arc<dyn Translator>> {
    let mut translators: Vec<std::sync::Arc<dyn Translator>> = Vec::new();
    for &(src_rate, src_id) in SLIN_RATES {
        for &(dst_rate, dst_id) in SLIN_RATES {
            if src_rate != dst_rate {
                translators.push(std::sync::Arc::new(
                    ResampleTranslator::new(src_rate, src_id, dst_rate, dst_id),
                ));
            }
        }
    }
    translators
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resample_same_rate() {
        let input: Vec<i16> = vec![100, 200, 300, 400];
        let output = resample_linear(&input, 8000, 8000);
        assert_eq!(output, input);
    }

    #[test]
    fn test_resample_upsample_2x() {
        let input: Vec<i16> = vec![0, 1000, 2000, 3000];
        let output = resample_linear(&input, 8000, 16000);
        // Should produce approximately twice as many samples
        assert!(output.len() >= input.len());
        assert!(output.len() <= input.len() * 2 + 1);
    }

    #[test]
    fn test_resample_downsample_2x() {
        let input: Vec<i16> = vec![0, 500, 1000, 1500, 2000, 2500, 3000, 3500];
        let output = resample_linear(&input, 16000, 8000);
        // Should produce approximately half as many samples
        assert!(output.len() <= input.len());
        assert!(output.len() >= input.len() / 2 - 1);
    }

    #[test]
    fn test_resample_empty() {
        let input: Vec<i16> = vec![];
        let output = resample_linear(&input, 8000, 16000);
        assert!(output.is_empty());
    }

    #[test]
    fn test_resample_preserves_dc() {
        // A constant signal should remain constant after resampling
        let input: Vec<i16> = vec![1000; 100];
        let output = resample_linear(&input, 8000, 16000);
        for &s in &output {
            assert_eq!(s, 1000);
        }
    }

    #[test]
    fn test_all_translator_count() {
        let translators = all_resample_translators();
        // 9 rates, so 9 * 8 = 72 translators
        assert_eq!(translators.len(), 9 * 8);
    }

    // --- Windowed sinc resampler tests ---

    #[test]
    fn test_sinc_resample_same_rate() {
        let input: Vec<i16> = vec![100, 200, 300, 400];
        let output = resample_sinc(&input, 8000, 8000);
        assert_eq!(output, input);
    }

    #[test]
    fn test_sinc_resample_upsample_2x() {
        let input: Vec<i16> = vec![0, 1000, 2000, 3000, 4000, 3000, 2000, 1000,
                                   0, -1000, -2000, -3000, -4000, -3000, -2000, -1000];
        let output = resample_sinc(&input, 8000, 16000);
        // Should produce approximately twice as many samples
        assert!(output.len() >= input.len(), "Upsampled length {} should be >= input {}", output.len(), input.len());
        assert!(output.len() <= input.len() * 2 + 1);
    }

    #[test]
    fn test_sinc_resample_downsample_2x() {
        let input: Vec<i16> = vec![0, 500, 1000, 1500, 2000, 2500, 3000, 3500,
                                   4000, 3500, 3000, 2500, 2000, 1500, 1000, 500];
        let output = resample_sinc(&input, 16000, 8000);
        assert!(output.len() <= input.len());
        assert!(output.len() >= input.len() / 2 - 1);
    }

    #[test]
    fn test_sinc_resample_empty() {
        let input: Vec<i16> = vec![];
        let output = resample_sinc(&input, 8000, 16000);
        assert!(output.is_empty());
    }

    #[test]
    fn test_sinc_resample_preserves_dc() {
        // A constant signal should remain approximately constant after sinc resampling
        let input: Vec<i16> = vec![1000; 200];
        let output = resample_sinc(&input, 8000, 16000);
        // Skip edges (edge effects from windowing)
        let mid_start = output.len() / 4;
        let mid_end = output.len() * 3 / 4;
        for (i, sample) in output.iter().enumerate().take(mid_end).skip(mid_start) {
            assert!(
                (*sample - 1000).abs() < 50,
                "DC preservation failed at index {}: got {}",
                i,
                sample
            );
        }
    }

    #[test]
    fn test_sinc_quality_vs_linear() {
        // Generate a test signal: 1kHz sine at 48kHz
        let n = 4800; // 100ms at 48kHz
        let input: Vec<i16> = (0..n)
            .map(|i| (10000.0 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48000.0).sin()) as i16)
            .collect();

        // Downsample to 8kHz with both methods
        let linear_result = resample_linear(&input, 48000, 8000);
        let sinc_result = resample_sinc(&input, 48000, 8000);

        // Both should produce roughly 800 samples (100ms at 8kHz)
        assert!(linear_result.len() >= 790 && linear_result.len() <= 810);
        assert!(sinc_result.len() >= 790 && sinc_result.len() <= 810);

        // Both should have signal content (not silence)
        let linear_energy: f64 = linear_result.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let sinc_energy: f64 = sinc_result.iter().map(|&s| (s as f64) * (s as f64)).sum();
        assert!(linear_energy > 0.0, "Linear resampler should produce non-zero output");
        assert!(sinc_energy > 0.0, "Sinc resampler should produce non-zero output");
    }
}
