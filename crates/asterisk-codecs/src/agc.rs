//! Automatic Gain Control (AGC).
//!
//! Dynamically adjusts audio signal level to maintain a target loudness.
//! Uses RMS-based level measurement with smoothed gain changes to avoid
//! audible artifacts.
//!
//! Features:
//! - RMS level measurement in dBFS
//! - Configurable attack/release time constants
//! - Hard limiter to prevent clipping
//! - Configurable target level and gain range

/// Automatic gain control processor.
pub struct AutoGainControl {
    /// Target RMS level in dBFS (e.g., -18.0).
    target_level: f32,
    /// Maximum gain in dB (e.g., 30.0).
    max_gain: f32,
    /// Minimum gain in dB (e.g., -6.0).
    min_gain: f32,
    /// Attack time constant in milliseconds (fast gain reduction).
    attack_time: f32,
    /// Release time constant in milliseconds (slow gain increase).
    release_time: f32,
    /// Current applied gain in dB.
    current_gain: f32,
    /// Sample rate in Hz.
    sample_rate: u32,
    /// Attack coefficient (derived from attack_time and sample_rate).
    attack_coeff: f32,
    /// Release coefficient (derived from release_time and sample_rate).
    release_coeff: f32,
}

impl AutoGainControl {
    /// Create a new AGC processor.
    ///
    /// - `sample_rate`: audio sample rate in Hz
    /// - `target_level`: target RMS level in dBFS (e.g., -18.0)
    pub fn new(sample_rate: u32, target_level: f32) -> Self {
        let attack_time = 10.0; // 10ms attack
        let release_time = 100.0; // 100ms release

        let attack_coeff = Self::time_constant_to_coeff(attack_time, sample_rate);
        let release_coeff = Self::time_constant_to_coeff(release_time, sample_rate);

        Self {
            target_level,
            max_gain: 30.0,
            min_gain: -6.0,
            attack_time,
            release_time,
            current_gain: 0.0,
            sample_rate,
            attack_coeff,
            release_coeff,
        }
    }

    /// Convert a time constant in ms to an exponential smoothing coefficient.
    fn time_constant_to_coeff(time_ms: f32, sample_rate: u32) -> f32 {
        if time_ms <= 0.0 || sample_rate == 0 {
            return 1.0;
        }
        let time_samples = time_ms * sample_rate as f32 / 1000.0;
        (-1.0 / time_samples).exp()
    }

    /// Set the maximum gain in dB.
    pub fn set_max_gain(&mut self, max_gain_db: f32) {
        self.max_gain = max_gain_db;
    }

    /// Set the minimum gain in dB.
    pub fn set_min_gain(&mut self, min_gain_db: f32) {
        self.min_gain = min_gain_db;
    }

    /// Set the attack time in milliseconds.
    pub fn set_attack_time(&mut self, attack_ms: f32) {
        self.attack_time = attack_ms;
        self.attack_coeff = Self::time_constant_to_coeff(attack_ms, self.sample_rate);
    }

    /// Set the release time in milliseconds.
    pub fn set_release_time(&mut self, release_ms: f32) {
        self.release_time = release_ms;
        self.release_coeff = Self::time_constant_to_coeff(release_ms, self.sample_rate);
    }

    /// Get the current gain in dB.
    pub fn current_gain_db(&self) -> f32 {
        self.current_gain
    }

    /// Compute RMS level of a frame in dBFS.
    pub fn measure_rms_dbfs(samples: &[i16]) -> f32 {
        if samples.is_empty() {
            return -96.0; // Effectively silence
        }

        let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let rms = (sum_sq / samples.len() as f64).sqrt();

        if rms < 1.0 {
            return -96.0;
        }

        // dBFS relative to full scale (32768)
        20.0 * (rms as f32 / 32768.0).log10()
    }

    /// Process a frame of audio, applying automatic gain control.
    pub fn process(&mut self, frame: &[i16]) -> Vec<i16> {
        if frame.is_empty() {
            return Vec::new();
        }

        // Measure current level
        let current_level = Self::measure_rms_dbfs(frame);

        // Skip very quiet frames (likely silence)
        if current_level < -60.0 {
            return frame.to_vec();
        }

        // Compute desired gain
        let desired_gain = (self.target_level - current_level).clamp(self.min_gain, self.max_gain);

        // Smooth gain changes using attack/release
        let coeff = if desired_gain < self.current_gain {
            // Gain is decreasing (signal getting louder) -> use fast attack
            self.attack_coeff
        } else {
            // Gain is increasing (signal getting quieter) -> use slow release
            self.release_coeff
        };

        self.current_gain = coeff * self.current_gain + (1.0 - coeff) * desired_gain;

        // Convert gain from dB to linear
        let gain_linear = 10.0f32.powf(self.current_gain / 20.0);

        // Apply gain with limiter
        let mut output = Vec::with_capacity(frame.len());
        for &sample in frame {
            let amplified = sample as f32 * gain_linear;
            // Hard limiter: clamp to i16 range
            let limited = amplified.round().clamp(-32768.0, 32767.0) as i16;
            output.push(limited);
        }

        output
    }

    /// Reset the AGC state.
    pub fn reset(&mut self) {
        self.current_gain = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agc_creation() {
        let agc = AutoGainControl::new(8000, -18.0);
        assert_eq!(agc.target_level, -18.0);
        assert_eq!(agc.sample_rate, 8000);
        assert_eq!(agc.current_gain, 0.0);
    }

    #[test]
    fn test_agc_silent_input_passthrough() {
        let mut agc = AutoGainControl::new(8000, -18.0);
        let silence = vec![0i16; 160];
        let output = agc.process(&silence);
        assert_eq!(output, silence);
    }

    #[test]
    fn test_agc_quiet_signal_amplified() {
        let mut agc = AutoGainControl::new(8000, -18.0);
        agc.set_max_gain(40.0);
        // Use a faster release time so gain converges in fewer frames
        agc.set_release_time(20.0);

        // Generate a quiet 440Hz tone (about -40 dBFS)
        let quiet: Vec<i16> = (0..160)
            .map(|i| (300.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 8000.0).sin()) as i16)
            .collect();

        let quiet_rms = AutoGainControl::measure_rms_dbfs(&quiet);
        assert!(quiet_rms < -30.0, "Input should be quiet: {:.1} dBFS", quiet_rms);

        // Process many frames to let gain converge
        let mut output = quiet.clone();
        for _ in 0..500 {
            output = agc.process(&quiet);
        }

        let output_rms = AutoGainControl::measure_rms_dbfs(&output);
        // Output should be louder than input
        assert!(
            output_rms > quiet_rms + 5.0,
            "AGC should amplify quiet signal: input={:.1}dBFS, output={:.1}dBFS",
            quiet_rms,
            output_rms
        );
    }

    #[test]
    fn test_agc_loud_signal_attenuated() {
        let mut agc = AutoGainControl::new(8000, -18.0);
        agc.set_min_gain(-30.0);

        // Generate a loud 440Hz tone (about -3 dBFS)
        let loud: Vec<i16> = (0..160)
            .map(|i| (24000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 8000.0).sin()) as i16)
            .collect();

        let loud_rms = AutoGainControl::measure_rms_dbfs(&loud);
        assert!(loud_rms > -10.0, "Input should be loud: {:.1} dBFS", loud_rms);

        // Process many frames to let gain converge
        let mut output = loud.clone();
        for _ in 0..200 {
            output = agc.process(&loud);
        }

        let output_rms = AutoGainControl::measure_rms_dbfs(&output);
        // Output should be quieter than input
        assert!(
            output_rms < loud_rms - 3.0,
            "AGC should attenuate loud signal: input={:.1}dBFS, output={:.1}dBFS",
            loud_rms,
            output_rms
        );
    }

    #[test]
    fn test_agc_no_clipping() {
        let mut agc = AutoGainControl::new(8000, -6.0);
        agc.set_max_gain(40.0);

        let signal: Vec<i16> = (0..160)
            .map(|i| (5000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 8000.0).sin()) as i16)
            .collect();

        for _ in 0..200 {
            let output = agc.process(&signal);
            for &s in &output {
                assert_ne!(s, i16::MIN);
                assert_ne!(s, i16::MAX);
            }
        }
    }

    #[test]
    fn test_rms_measurement() {
        // Full-scale sine: RMS should be about -3 dBFS
        let sine: Vec<i16> = (0..1000)
            .map(|i| {
                (32767.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 8000.0).sin()) as i16
            })
            .collect();

        let rms = AutoGainControl::measure_rms_dbfs(&sine);
        // Sine wave RMS = peak / sqrt(2) = -3.01 dBFS
        assert!(
            (rms - (-3.0)).abs() < 1.0,
            "Full-scale sine RMS should be ~-3 dBFS, got {:.1}",
            rms
        );
    }

    #[test]
    fn test_agc_reset() {
        let mut agc = AutoGainControl::new(8000, -18.0);
        let signal: Vec<i16> = (0..160)
            .map(|i| (5000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 8000.0).sin()) as i16)
            .collect();

        for _ in 0..50 {
            agc.process(&signal);
        }
        assert!(agc.current_gain_db() != 0.0);

        agc.reset();
        assert_eq!(agc.current_gain_db(), 0.0);
    }

    #[test]
    fn test_agc_empty_input() {
        let mut agc = AutoGainControl::new(8000, -18.0);
        let output = agc.process(&[]);
        assert!(output.is_empty());
    }
}
