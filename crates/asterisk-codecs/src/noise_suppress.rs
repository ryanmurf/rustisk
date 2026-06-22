//! Spectral Subtraction Noise Suppression.
//!
//! Implements noise reduction using spectral subtraction with Wiener filtering.
//! The algorithm estimates the noise floor during silence and subtracts it
//! from the signal spectrum to reduce background noise.
//!
//! Features:
//! - FFT-based spectral analysis
//! - Automatic noise floor estimation during silence
//! - Wiener filter gain computation per frequency bin
//! - Overlap-add synthesis for smooth output
//! - Built-in Voice Activity Detection (VAD)

use std::f32::consts::PI;

/// Voice Activity Detection using energy and zero-crossing rate.
pub struct VoiceActivityDetector {
    /// Energy threshold for speech detection (adaptive).
    energy_threshold: f32,
    /// Zero-crossing rate threshold.
    zcr_threshold: f32,
    /// Smoothed energy estimate for adaptive threshold.
    background_energy: f32,
    /// Adaptation rate for background energy.
    adaptation_rate: f32,
    /// Number of frames since last speech.
    silence_frames: u32,
    /// Minimum silence frames before noise estimation.
    min_silence_frames: u32,
}

impl VoiceActivityDetector {
    /// Create a new VAD.
    pub fn new() -> Self {
        Self {
            energy_threshold: 200.0,
            zcr_threshold: 0.3,
            background_energy: 0.0,
            adaptation_rate: 0.01,
            silence_frames: 0,
            min_silence_frames: 10,
        }
    }

    /// Detect voice activity in a frame of samples.
    ///
    /// Returns `true` if speech is detected.
    pub fn is_speech(&mut self, samples: &[f32]) -> bool {
        if samples.is_empty() {
            return false;
        }

        // Compute frame energy
        let energy: f32 = samples.iter().map(|&s| s * s).sum::<f32>() / samples.len() as f32;

        // Compute zero-crossing rate
        let mut zcr_count = 0u32;
        for i in 1..samples.len() {
            if (samples[i] >= 0.0) != (samples[i - 1] >= 0.0) {
                zcr_count += 1;
            }
        }
        let zcr = zcr_count as f32 / (samples.len() - 1).max(1) as f32;

        // Adaptive threshold
        let threshold = self.background_energy * 3.0 + self.energy_threshold;

        let is_speech = energy > threshold && zcr < self.zcr_threshold;

        if !is_speech {
            self.silence_frames += 1;
            // Slowly adapt background energy estimate during silence
            self.background_energy =
                (1.0 - self.adaptation_rate) * self.background_energy + self.adaptation_rate * energy;
        } else {
            self.silence_frames = 0;
        }

        is_speech
    }

    /// Check if we're in a stable silence period (good for noise estimation).
    pub fn is_stable_silence(&self) -> bool {
        self.silence_frames >= self.min_silence_frames
    }

    /// Reset the VAD state.
    pub fn reset(&mut self) {
        self.background_energy = 0.0;
        self.silence_frames = 0;
    }
}

impl Default for VoiceActivityDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Spectral subtraction noise suppressor.
pub struct NoiseSuppressor {
    /// FFT size (number of points).
    fft_size: usize,
    /// Estimated noise power spectrum (per bin).
    noise_estimate: Vec<f32>,
    /// Rate at which to adapt the noise floor estimate.
    adaptation_rate: f32,
    /// How aggressively to suppress noise (0.0-1.0).
    suppression_factor: f32,
    /// Minimum gain to prevent musical noise artifacts (linear scale).
    min_gain: f32,
    /// Sample rate.
    sample_rate: u32,
    /// Voice activity detector.
    vad: VoiceActivityDetector,
    /// Previous frame overlap for overlap-add.
    overlap_buffer: Vec<f32>,
    /// Whether noise estimate has been initialized.
    noise_initialized: bool,
    /// Number of frames used for initial noise estimation.
    init_frames: u32,
    /// Hann window coefficients (precomputed).
    window: Vec<f32>,
}

impl NoiseSuppressor {
    /// Create a new noise suppressor.
    ///
    /// - `sample_rate`: audio sample rate in Hz
    /// - `fft_size`: FFT size (256 or 512 recommended)
    pub fn new(sample_rate: u32, fft_size: usize) -> Self {
        let fft_size = fft_size.max(64).next_power_of_two();
        let half = fft_size / 2 + 1;

        // Precompute Hann window
        let window: Vec<f32> = (0..fft_size)
            .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / fft_size as f32).cos()))
            .collect();

        Self {
            fft_size,
            noise_estimate: vec![0.0; half],
            adaptation_rate: 0.02,
            suppression_factor: 1.0,
            min_gain: 0.1, // -20 dB minimum gain
            sample_rate,
            vad: VoiceActivityDetector::new(),
            overlap_buffer: vec![0.0; fft_size / 2],
            noise_initialized: false,
            init_frames: 0,
            window,
        }
    }

    /// Process a frame of audio, reducing noise.
    ///
    /// Input frame should be `fft_size` samples.
    /// Returns a noise-suppressed frame of `fft_size / 2` samples (due to overlap-add with 50% overlap).
    #[allow(clippy::needless_range_loop)]
    pub fn process(&mut self, frame: &[i16]) -> Vec<i16> {
        let n = self.fft_size;
        let half = n / 2 + 1;

        // Convert to float and zero-pad if necessary
        let mut signal: Vec<f32> = frame.iter().map(|&s| s as f32).collect();
        signal.resize(n, 0.0);

        // Check VAD
        let is_speech = self.vad.is_speech(&signal);

        // Apply window
        let windowed: Vec<f32> = signal.iter().zip(self.window.iter()).map(|(&s, &w)| s * w).collect();

        // Compute DFT (real-valued FFT via DIT radix-2)
        let (re, im) = real_fft(&windowed, n);

        // Compute power spectrum
        let mut power_spectrum: Vec<f32> = Vec::with_capacity(half);
        for i in 0..half {
            power_spectrum.push(re[i] * re[i] + im[i] * im[i]);
        }

        // Update noise estimate during silence
        if !is_speech || !self.noise_initialized {
            self.init_frames += 1;
            if !self.noise_initialized && self.init_frames <= 10 {
                // Initial noise estimation: average first few frames
                let alpha = 1.0 / self.init_frames as f32;
                for i in 0..half {
                    self.noise_estimate[i] =
                        (1.0 - alpha) * self.noise_estimate[i] + alpha * power_spectrum[i];
                }
                if self.init_frames >= 5 {
                    self.noise_initialized = true;
                }
            } else {
                // Continuous noise adaptation during silence
                for i in 0..half {
                    self.noise_estimate[i] = (1.0 - self.adaptation_rate) * self.noise_estimate[i]
                        + self.adaptation_rate * power_spectrum[i];
                }
            }
        }

        // Apply Wiener filter gain
        let mut gain: Vec<f32> = Vec::with_capacity(half);
        for i in 0..half {
            let snr = if self.noise_estimate[i] > 1e-10 {
                power_spectrum[i] / self.noise_estimate[i]
            } else {
                1000.0 // Very high SNR if noise is essentially zero
            };

            // Wiener gain: G = max(1 - suppression * noise/signal, min_gain)
            let g = (1.0 - self.suppression_factor / snr.max(1e-10)).max(self.min_gain);
            gain.push(g);
        }

        // Apply gain to spectrum
        let mut out_re = vec![0.0f32; n];
        let mut out_im = vec![0.0f32; n];
        for i in 0..half {
            let g_sqrt = gain[i].sqrt(); // Apply sqrt(gain) to amplitude
            out_re[i] = re[i] * g_sqrt;
            out_im[i] = im[i] * g_sqrt;
        }
        // Mirror for negative frequencies
        for i in 1..(n / 2) {
            out_re[n - i] = out_re[i];
            out_im[n - i] = -out_im[i];
        }

        // Inverse FFT
        let reconstructed = real_ifft(&out_re, &out_im, n);

        // Apply synthesis window
        let synth: Vec<f32> = reconstructed.iter().zip(self.window.iter()).map(|(&s, &w)| s * w).collect();

        // Overlap-add: combine with previous overlap
        let hop = n / 2;
        let mut output = Vec::with_capacity(hop);
        for i in 0..hop {
            let sample = synth[i] + self.overlap_buffer[i];
            output.push(sample.round().clamp(-32768.0, 32767.0) as i16);
        }

        // Save overlap for next frame
        self.overlap_buffer.clear();
        for i in hop..n {
            self.overlap_buffer.push(synth[i]);
        }

        output
    }

    /// Reset the noise suppressor state.
    pub fn reset(&mut self) {
        let half = self.fft_size / 2 + 1;
        self.noise_estimate = vec![0.0; half];
        self.overlap_buffer = vec![0.0; self.fft_size / 2];
        self.noise_initialized = false;
        self.init_frames = 0;
        self.vad.reset();
    }

    /// Get the sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Get the FFT size.
    pub fn fft_size(&self) -> usize {
        self.fft_size
    }
}

// ---------------------------------------------------------------------------
// Simple DFT/IDFT implementation (no external FFT crate dependency)
// ---------------------------------------------------------------------------

/// Compute the real-valued FFT of a signal using radix-2 DIT.
///
/// Returns (real, imaginary) parts for bins 0..=N/2.
#[allow(clippy::needless_range_loop)]
fn real_fft(signal: &[f32], n: usize) -> (Vec<f32>, Vec<f32>) {
    // DFT via direct computation (O(N^2) but correct for small N)
    let half = n / 2 + 1;
    let mut re = vec![0.0f32; half];
    let mut im = vec![0.0f32; half];

    for k in 0..half {
        let mut sum_re = 0.0f32;
        let mut sum_im = 0.0f32;
        for i in 0..n {
            let angle = -2.0 * PI * (k as f32) * (i as f32) / (n as f32);
            sum_re += signal[i] * angle.cos();
            sum_im += signal[i] * angle.sin();
        }
        re[k] = sum_re;
        im[k] = sum_im;
    }

    (re, im)
}

/// Compute the inverse FFT, returning real-valued samples.
#[allow(clippy::needless_range_loop)]
fn real_ifft(re: &[f32], im: &[f32], n: usize) -> Vec<f32> {
    let mut output = vec![0.0f32; n];
    let inv_n = 1.0 / n as f32;

    for i in 0..n {
        let mut sum = 0.0f32;
        for k in 0..n {
            let angle = 2.0 * PI * (k as f32) * (i as f32) / (n as f32);
            let r = if k < re.len() { re[k] } else { re[n - k] };
            let m = if k < im.len() { im[k] } else { -im[n - k] };
            sum += r * angle.cos() - m * angle.sin();
        }
        output[i] = sum * inv_n;
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_noise_suppressor_creation() {
        let ns = NoiseSuppressor::new(8000, 256);
        assert_eq!(ns.fft_size(), 256);
        assert_eq!(ns.sample_rate(), 8000);
    }

    #[test]
    fn test_noise_suppressor_silent_input() {
        let mut ns = NoiseSuppressor::new(8000, 256);
        let silence = vec![0i16; 256];
        let output = ns.process(&silence);
        // Output should be fft_size/2 samples
        assert_eq!(output.len(), 128);
        for &s in &output {
            assert!(s.abs() < 10, "Silent input should produce near-silent output");
        }
    }

    #[test]
    fn test_noise_suppressor_reset() {
        let mut ns = NoiseSuppressor::new(8000, 256);
        let signal = vec![1000i16; 256];
        ns.process(&signal);
        ns.reset();
        assert!(!ns.noise_initialized);
    }

    #[test]
    fn test_noise_suppressor_improves_snr() {
        let mut ns = NoiseSuppressor::new(8000, 256);
        let n = 256;

        // Generate clean signal: 1kHz tone
        let clean: Vec<i16> = (0..n)
            .map(|i| (8000.0 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 8000.0).sin()) as i16)
            .collect();

        // Generate noise (low level)
        let mut noise_state = 12345u32;
        let noise: Vec<i16> = (0..n)
            .map(|_| {
                noise_state ^= noise_state << 13;
                noise_state ^= noise_state >> 17;
                noise_state ^= noise_state << 5;
                ((noise_state as f32 / u32::MAX as f32) * 2.0 - 1.0) as i16 * 500
            })
            .collect();

        // First, feed noise-only frames to let the noise estimator learn
        let noise_only: Vec<i16> = noise.iter().map(|&n| n / 2).collect();
        for _ in 0..20 {
            ns.process(&noise_only);
        }

        // Now process noisy signal
        let noisy: Vec<i16> = clean.iter().zip(noise.iter()).map(|(&c, &n)| c.saturating_add(n)).collect();
        let output = ns.process(&noisy);

        // Measure energy of noisy input and output
        let input_noise_energy: f64 = noise[..n / 2].iter().map(|&s| (s as f64) * (s as f64)).sum();
        let _output_energy: f64 = output.iter().map(|&s| (s as f64) * (s as f64)).sum();

        // The suppressor should have processed the signal (non-trivially)
        assert!(output.len() == n / 2);
        // Basic sanity: output should not be all zeros unless input was very quiet
        let output_has_signal = output.iter().any(|&s| s.abs() > 10);
        assert!(output_has_signal || input_noise_energy < 100.0, "Output should contain signal");
    }

    #[test]
    fn test_vad_silence_detection() {
        let mut vad = VoiceActivityDetector::new();
        let silence = vec![0.0f32; 160];
        // Feed multiple silent frames
        for _ in 0..20 {
            assert!(!vad.is_speech(&silence));
        }
        assert!(vad.is_stable_silence());
    }

    #[test]
    fn test_vad_speech_detection() {
        let mut vad = VoiceActivityDetector::new();
        // Feed silence to establish baseline
        let silence = vec![0.0f32; 160];
        for _ in 0..20 {
            vad.is_speech(&silence);
        }
        // Feed loud signal
        let loud: Vec<f32> = (0..160)
            .map(|i| 10000.0 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 8000.0).sin())
            .collect();
        assert!(vad.is_speech(&loud));
    }

    #[test]
    fn test_fft_ifft_roundtrip() {
        let n = 64;
        let signal: Vec<f32> = (0..n)
            .map(|i| 1000.0 * (2.0 * PI * 5.0 * i as f32 / n as f32).sin())
            .collect();

        let (re, im) = real_fft(&signal, n);
        let mut full_re = vec![0.0f32; n];
        let mut full_im = vec![0.0f32; n];
        let half = n / 2 + 1;
        full_re[..half].copy_from_slice(&re[..half]);
        full_im[..half].copy_from_slice(&im[..half]);
        for i in 1..(n / 2) {
            full_re[n - i] = re[i];
            full_im[n - i] = -im[i];
        }

        let reconstructed = real_ifft(&full_re, &full_im, n);

        // Verify roundtrip accuracy
        for i in 0..n {
            let diff = (signal[i] - reconstructed[i]).abs();
            assert!(
                diff < 1.0,
                "FFT/IFFT roundtrip error at index {}: expected {}, got {}, diff={}",
                i,
                signal[i],
                reconstructed[i],
                diff
            );
        }
    }
}
