//! Opus codec integration layer.
//!
//! Port of codecs/codec_opus.c from Asterisk C.
//!
//! Opus is a versatile audio codec suitable for interactive speech and
//! music transmission. It operates at bitrates from 6 kbps to 510 kbps
//! and sample rates from 8 kHz to 48 kHz.
//!
//! This module provides the configuration/interface layer with SDP fmtp
//! attribute parsing. The actual encode/decode is stubbed since it
//! requires libopus FFI.
//!
//! References:
//! - RFC 6716: Definition of the Opus Audio Codec
//! - RFC 7587: RTP Payload Format for the Opus Speech and Audio Codec

use crate::builtin_codecs::{ID_OPUS, ID_SLIN48};
use crate::codec::CodecId;
use crate::translate::{TransCost, TranslateError, Translator, TranslatorInstance};
use asterisk_types::Frame;

/// Opus application type.
///
/// Corresponds to `OPUS_APPLICATION_*` constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusApplication {
    /// Best for most VoIP/videoconference applications where listening quality
    /// and intelligibility matter most.
    Voip,
    /// Best for broadcast/high-fidelity application where the decoded audio
    /// should be as close as possible to the input.
    Audio,
    /// Only use when lowest-achievable latency is what matters most.
    RestrictedLowDelay,
}

impl OpusApplication {
    pub fn as_str(&self) -> &'static str {
        match self {
            OpusApplication::Voip => "voip",
            OpusApplication::Audio => "audio",
            OpusApplication::RestrictedLowDelay => "restricted_lowdelay",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "voip" => Some(OpusApplication::Voip),
            "audio" => Some(OpusApplication::Audio),
            "restricted_lowdelay" | "lowdelay" => Some(OpusApplication::RestrictedLowDelay),
            _ => None,
        }
    }
}

/// Opus signal type hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusSignalType {
    /// Let the codec decide automatically.
    Auto,
    /// Signal is voice.
    Voice,
    /// Signal is music.
    Music,
}

impl OpusSignalType {
    pub fn as_str(&self) -> &'static str {
        match self {
            OpusSignalType::Auto => "auto",
            OpusSignalType::Voice => "voice",
            OpusSignalType::Music => "music",
        }
    }
}

/// Opus encoder configuration.
///
/// Maps to the options available in codecs.conf and SDP fmtp attributes.
#[derive(Debug, Clone)]
pub struct OpusEncoderConfig {
    /// Target bitrate in bits per second (6000-510000).
    pub bitrate: u32,
    /// Encoding complexity (0-10). Higher = better quality, more CPU.
    pub complexity: u32,
    /// Signal type hint for the encoder.
    pub signal_type: OpusSignalType,
    /// Application mode.
    pub application: OpusApplication,
    /// Enable Forward Error Correction (in-band FEC).
    pub fec: bool,
    /// Enable Discontinuous Transmission (DTX / silence suppression).
    pub dtx: bool,
    /// Expected packet loss percentage (0-100). Affects FEC level.
    pub packet_loss_pct: u32,
    /// Maximum playback rate to signal to decoder (Hz).
    pub max_playback_rate: u32,
    /// Maximum capture rate (Hz).
    pub max_capture_rate: u32,
    /// Enable stereo encoding.
    pub stereo: bool,
    /// Force constant bitrate.
    pub cbr: bool,
    /// Maximum average bitrate signaled to remote.
    pub max_average_bitrate: u32,
    /// Frame duration in ms (2.5, 5, 10, 20, 40, 60).
    pub frame_duration_ms: f32,
}

impl Default for OpusEncoderConfig {
    fn default() -> Self {
        Self {
            bitrate: 32000,
            complexity: 10,
            signal_type: OpusSignalType::Auto,
            application: OpusApplication::Voip,
            fec: true,
            dtx: false,
            packet_loss_pct: 0,
            max_playback_rate: 48000,
            max_capture_rate: 48000,
            stereo: false,
            cbr: false,
            max_average_bitrate: 0,
            frame_duration_ms: 20.0,
        }
    }
}

/// Opus decoder configuration.
#[derive(Debug, Clone)]
pub struct OpusDecoderConfig {
    /// Whether to request FEC from encoder.
    pub fec: bool,
    /// Sample rate for decoding output.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u8,
}

impl Default for OpusDecoderConfig {
    fn default() -> Self {
        Self {
            fec: false,
            sample_rate: 48000,
            channels: 2,
        }
    }
}

/// SDP fmtp attributes for Opus codec negotiation.
///
/// These attributes are exchanged in SDP offer/answer and configure
/// the encoder/decoder behavior for the RTP session.
#[derive(Debug, Clone, Default)]
pub struct OpusSdpAttributes {
    /// Maximum sample rate the receiver can play (Hz).
    pub maxplaybackrate: Option<u32>,
    /// Maximum sample rate the sender will capture (Hz).
    pub sprop_maxcapturerate: Option<u32>,
    /// Minimum packetization time in ms.
    pub minptime: Option<u32>,
    /// Maximum average bitrate the receiver can handle.
    pub maxaveragebitrate: Option<u32>,
    /// Whether stereo is allowed (0 or 1).
    pub stereo: Option<bool>,
    /// Whether constant bitrate is preferred (0 or 1).
    pub cbr: Option<bool>,
    /// Whether in-band FEC should be used (0 or 1).
    pub useinbandfec: Option<bool>,
    /// Whether DTX should be used (0 or 1).
    pub usedtx: Option<bool>,
}

impl OpusSdpAttributes {
    /// Parse fmtp attributes from an SDP fmtp line.
    ///
    /// Input format: "key1=value1; key2=value2; ..."
    /// or: "key1=value1;key2=value2"
    pub fn parse_fmtp(fmtp: &str) -> Self {
        let mut attrs = Self::default();

        for part in fmtp.split(';') {
            let part = part.trim();
            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value.trim();

                match key.as_str() {
                    "maxplaybackrate" => {
                        attrs.maxplaybackrate = value.parse().ok();
                    }
                    "sprop-maxcapturerate" => {
                        attrs.sprop_maxcapturerate = value.parse().ok();
                    }
                    "minptime" => {
                        attrs.minptime = value.parse().ok();
                    }
                    "maxaveragebitrate" => {
                        attrs.maxaveragebitrate = value.parse().ok();
                    }
                    "stereo" => {
                        attrs.stereo = Some(value == "1");
                    }
                    "cbr" => {
                        attrs.cbr = Some(value == "1");
                    }
                    "useinbandfec" => {
                        attrs.useinbandfec = Some(value == "1");
                    }
                    "usedtx" => {
                        attrs.usedtx = Some(value == "1");
                    }
                    _ => {} // Ignore unknown attributes
                }
            }
        }

        attrs
    }

    /// Generate an fmtp attribute string for SDP.
    pub fn to_fmtp(&self) -> String {
        let mut parts = Vec::new();

        if let Some(rate) = self.maxplaybackrate {
            parts.push(format!("maxplaybackrate={}", rate));
        }
        if let Some(rate) = self.sprop_maxcapturerate {
            parts.push(format!("sprop-maxcapturerate={}", rate));
        }
        if let Some(ptime) = self.minptime {
            parts.push(format!("minptime={}", ptime));
        }
        if let Some(bitrate) = self.maxaveragebitrate {
            parts.push(format!("maxaveragebitrate={}", bitrate));
        }
        if let Some(stereo) = self.stereo {
            parts.push(format!("stereo={}", if stereo { "1" } else { "0" }));
        }
        if let Some(cbr) = self.cbr {
            parts.push(format!("cbr={}", if cbr { "1" } else { "0" }));
        }
        if let Some(fec) = self.useinbandfec {
            parts.push(format!("useinbandfec={}", if fec { "1" } else { "0" }));
        }
        if let Some(dtx) = self.usedtx {
            parts.push(format!("usedtx={}", if dtx { "1" } else { "0" }));
        }

        parts.join(";")
    }

    /// Apply SDP attributes to an encoder configuration.
    pub fn apply_to_encoder(&self, config: &mut OpusEncoderConfig) {
        if let Some(rate) = self.maxplaybackrate {
            config.max_playback_rate = rate;
        }
        if let Some(rate) = self.sprop_maxcapturerate {
            config.max_capture_rate = rate;
        }
        if let Some(bitrate) = self.maxaveragebitrate {
            config.max_average_bitrate = bitrate;
            if config.bitrate > bitrate {
                config.bitrate = bitrate;
            }
        }
        if let Some(stereo) = self.stereo {
            config.stereo = stereo;
        }
        if let Some(cbr) = self.cbr {
            config.cbr = cbr;
        }
        if let Some(fec) = self.useinbandfec {
            config.fec = fec;
        }
        if let Some(dtx) = self.usedtx {
            config.dtx = dtx;
        }
    }
}

/// Opus encoder (stub).
///
/// In a real implementation, this would hold an `OpusEncoder*` from libopus.
pub struct OpusEncoder {
    pub config: OpusEncoderConfig,
}

impl OpusEncoder {
    pub fn new() -> Self {
        Self {
            config: OpusEncoderConfig::default(),
        }
    }

    pub fn with_config(config: OpusEncoderConfig) -> Self {
        Self { config }
    }

    /// Encode PCM samples to Opus data.
    ///
    /// STUB: Real implementation requires libopus.
    pub fn encode(&mut self, _samples: &[i16]) -> Result<Vec<u8>, TranslateError> {
        Err(TranslateError::Failed(
            "Opus encoding requires libopus (not linked)".into(),
        ))
    }
}

impl Default for OpusEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Opus decoder (stub).
pub struct OpusDecoder {
    pub config: OpusDecoderConfig,
}

impl OpusDecoder {
    pub fn new() -> Self {
        Self {
            config: OpusDecoderConfig::default(),
        }
    }

    /// Decode Opus data to PCM samples.
    ///
    /// STUB: Real implementation requires libopus.
    pub fn decode(&mut self, _data: &[u8]) -> Result<Vec<i16>, TranslateError> {
        Err(TranslateError::Failed(
            "Opus decoding requires libopus (not linked)".into(),
        ))
    }

    /// Decode with FEC (packet loss concealment).
    ///
    /// STUB: Real implementation requires libopus.
    pub fn decode_fec(&mut self, _data: &[u8]) -> Result<Vec<i16>, TranslateError> {
        Err(TranslateError::Failed(
            "Opus FEC decoding requires libopus (not linked)".into(),
        ))
    }
}

impl Default for OpusDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Translator implementations
// ---------------------------------------------------------------------------

/// Translator: Opus -> Signed Linear 48kHz.
pub struct OpusToSlinTranslator;

impl Translator for OpusToSlinTranslator {
    fn name(&self) -> &str { "opustolin48" }
    fn src_codec_id(&self) -> CodecId { ID_OPUS }
    fn dst_codec_id(&self) -> CodecId { ID_SLIN48 }
    fn table_cost(&self) -> u32 { TransCost::LY_LL_ORIGSAMP }
    fn new_instance(&self) -> Box<dyn TranslatorInstance> {
        Box::new(OpusToSlinInstance {
            decoder: OpusDecoder::new(),
        })
    }
}

struct OpusToSlinInstance {
    decoder: OpusDecoder,
}

impl TranslatorInstance for OpusToSlinInstance {
    fn frame_in(&mut self, frame: &Frame) -> Result<(), TranslateError> {
        let data = match frame {
            Frame::Voice { data, .. } => data,
            _ => return Err(TranslateError::Failed("expected voice frame".into())),
        };
        let _samples = self.decoder.decode(data)?;
        Ok(())
    }

    fn frame_out(&mut self) -> Option<Frame> {
        None
    }
}

/// Translator: Signed Linear 48kHz -> Opus.
pub struct SlinToOpusTranslator;

impl Translator for SlinToOpusTranslator {
    fn name(&self) -> &str { "lin48toopus" }
    fn src_codec_id(&self) -> CodecId { ID_SLIN48 }
    fn dst_codec_id(&self) -> CodecId { ID_OPUS }
    fn table_cost(&self) -> u32 { TransCost::LL_LY_ORIGSAMP }
    fn new_instance(&self) -> Box<dyn TranslatorInstance> {
        Box::new(SlinToOpusInstance {
            encoder: OpusEncoder::new(),
        })
    }
}

struct SlinToOpusInstance {
    encoder: OpusEncoder,
}

impl TranslatorInstance for SlinToOpusInstance {
    fn frame_in(&mut self, frame: &Frame) -> Result<(), TranslateError> {
        let data = match frame {
            Frame::Voice { data, .. } => data,
            _ => return Err(TranslateError::Failed("expected voice frame".into())),
        };

        if data.len() % 2 != 0 {
            return Err(TranslateError::Failed("slin data must have even length".into()));
        }

        let mut samples: Vec<i16> = Vec::with_capacity(data.len() / 2);
        let mut i = 0;
        while i + 1 < data.len() {
            samples.push(i16::from_le_bytes([data[i], data[i + 1]]));
            i += 2;
        }

        let _encoded = self.encoder.encode(&samples)?;
        Ok(())
    }

    fn frame_out(&mut self) -> Option<Frame> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opus_encoder_config_defaults() {
        let config = OpusEncoderConfig::default();
        assert_eq!(config.bitrate, 32000);
        assert_eq!(config.complexity, 10);
        assert!(config.fec);
        assert!(!config.dtx);
        assert!(!config.stereo);
        assert!(!config.cbr);
    }

    #[test]
    fn test_opus_sdp_parse_fmtp() {
        let fmtp = "maxplaybackrate=16000;stereo=0;useinbandfec=1;maxaveragebitrate=20000";
        let attrs = OpusSdpAttributes::parse_fmtp(fmtp);
        assert_eq!(attrs.maxplaybackrate, Some(16000));
        assert_eq!(attrs.stereo, Some(false));
        assert_eq!(attrs.useinbandfec, Some(true));
        assert_eq!(attrs.maxaveragebitrate, Some(20000));
    }

    #[test]
    fn test_opus_sdp_to_fmtp() {
        let attrs = OpusSdpAttributes {
            maxplaybackrate: Some(16000),
            stereo: Some(false),
            useinbandfec: Some(true),
            ..Default::default()
        };
        let fmtp = attrs.to_fmtp();
        assert!(fmtp.contains("maxplaybackrate=16000"));
        assert!(fmtp.contains("stereo=0"));
        assert!(fmtp.contains("useinbandfec=1"));
    }

    #[test]
    fn test_opus_sdp_apply_to_encoder() {
        let fmtp = "maxaveragebitrate=24000;useinbandfec=0;usedtx=1;cbr=1";
        let attrs = OpusSdpAttributes::parse_fmtp(fmtp);
        let mut config = OpusEncoderConfig::default();
        attrs.apply_to_encoder(&mut config);
        assert_eq!(config.max_average_bitrate, 24000);
        assert!(!config.fec);
        assert!(config.dtx);
        assert!(config.cbr);
        assert!(config.bitrate <= 24000);
    }

    #[test]
    fn test_opus_application_types() {
        assert_eq!(OpusApplication::from_str("voip"), Some(OpusApplication::Voip));
        assert_eq!(OpusApplication::from_str("audio"), Some(OpusApplication::Audio));
        assert_eq!(
            OpusApplication::from_str("restricted_lowdelay"),
            Some(OpusApplication::RestrictedLowDelay)
        );
        assert_eq!(OpusApplication::from_str("unknown"), None);
    }

    #[test]
    fn test_encode_stub() {
        let mut enc = OpusEncoder::new();
        let samples = vec![0i16; 960]; // 20ms at 48kHz
        assert!(enc.encode(&samples).is_err());
    }

    #[test]
    fn test_decode_stub() {
        let mut dec = OpusDecoder::new();
        let data = vec![0u8; 100];
        assert!(dec.decode(&data).is_err());
    }
}
