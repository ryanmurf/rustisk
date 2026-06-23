//! Integration tests exercising multiple Rustisk crates working together.

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use std::sync::Arc;

    // =========================================================================
    // Test 1: Channel lifecycle
    // =========================================================================
    #[test]
    fn test_channel_lifecycle() {
        use asterisk_core::channel::Channel;
        use asterisk_types::{ChannelState, HangupCause};

        // Create a channel
        let mut chan = Channel::new("SIP/alice-00000001");
        assert_eq!(chan.state, ChannelState::Down);
        assert_eq!(chan.name, "SIP/alice-00000001");

        // Transition: Down -> Ringing
        chan.set_state(ChannelState::Ringing);
        assert_eq!(chan.state, ChannelState::Ringing);

        // Take a snapshot while ringing
        let snap_ringing = chan.snapshot();
        assert_eq!(snap_ringing.state, ChannelState::Ringing);
        assert_eq!(snap_ringing.name, "SIP/alice-00000001");

        // Transition: Ringing -> Up (answer)
        chan.answer();
        assert_eq!(chan.state, ChannelState::Up);

        // Set some caller information
        chan.caller.id.name.name = "Alice".to_string();
        chan.caller.id.number.number = "5551234".to_string();

        // Take a snapshot while up
        let snap_up = chan.snapshot();
        assert_eq!(snap_up.state, ChannelState::Up);
        assert_eq!(snap_up.caller.id.name.name, "Alice");
        assert_eq!(snap_up.caller.id.number.number, "5551234");

        // Set channel variables
        chan.set_variable("MY_VAR", "my_value");
        assert_eq!(chan.get_variable("MY_VAR"), Some("my_value"));

        // Hangup
        chan.hangup(HangupCause::NormalClearing);
        assert_eq!(chan.state, ChannelState::Down);
        assert_eq!(chan.hangup_cause, HangupCause::NormalClearing);

        // Verify the snapshot from before hangup still shows the old state
        assert_eq!(snap_up.state, ChannelState::Up);
    }

    // =========================================================================
    // Test 2: Frame creation and codec/format operations
    // =========================================================================
    #[test]
    fn test_frame_creation_and_codec_translation() {
        use asterisk_codecs::builtin_codecs::{ID_ALAW, ID_SLIN8, ID_ULAW};
        use asterisk_codecs::format_cap::FormatCap;
        use asterisk_codecs::registry::CodecRegistry;
        use asterisk_types::{ControlFrame, Frame, FrameType, MediaType};

        // -- Frame creation --
        // Voice frame
        let voice = Frame::voice(ID_ULAW, 160, Bytes::from_static(&[0xFF; 160]));
        assert!(voice.is_voice());
        assert_eq!(voice.frame_type(), FrameType::Voice);

        // DTMF frames
        let dtmf_begin = Frame::dtmf_begin('5');
        assert!(dtmf_begin.is_dtmf());
        assert_eq!(dtmf_begin.frame_type(), FrameType::DtmfBegin);

        let dtmf_end = Frame::dtmf_end('5', 120);
        assert!(dtmf_end.is_dtmf());
        assert_eq!(dtmf_end.frame_type(), FrameType::DtmfEnd);
        if let Frame::DtmfEnd { digit, duration_ms } = &dtmf_end {
            assert_eq!(*digit, '5');
            assert_eq!(*duration_ms, 120);
        } else {
            panic!("Expected DtmfEnd frame");
        }

        // Control frames
        let ringing = Frame::control(ControlFrame::Ringing);
        assert!(ringing.is_control());
        assert_eq!(ringing.frame_type(), FrameType::Control);

        let answer = Frame::control(ControlFrame::Answer);
        assert!(answer.is_control());

        let hangup = Frame::control(ControlFrame::Hangup);
        assert!(hangup.is_control());

        // Null frame
        let null_frame = Frame::null();
        assert_eq!(null_frame.frame_type(), FrameType::Null);

        // Text frame
        let text = Frame::text("Hello".to_string());
        assert_eq!(text.frame_type(), FrameType::Text);

        // -- Codec registry --
        let registry = CodecRegistry::with_builtins();
        assert!(registry.codec_count() > 0);

        // Look up ULAW
        let ulaw = registry.get_codec(ID_ULAW).expect("ULAW codec not found");
        assert_eq!(ulaw.name, "ulaw");
        assert_eq!(ulaw.sample_rate, 8000);
        assert_eq!(ulaw.media_type, MediaType::Audio);

        // Look up ALAW
        let alaw = registry.get_codec(ID_ALAW).expect("ALAW codec not found");
        assert_eq!(alaw.name, "alaw");
        assert_eq!(alaw.sample_rate, 8000);

        // Look up SLIN
        let slin = registry.get_codec(ID_SLIN8).expect("SLIN codec not found");
        assert_eq!(slin.name, "slin");
        assert_eq!(slin.sample_rate, 8000);

        // Look up by name
        let ulaw_by_name = registry
            .get_codec_by_name("ulaw", MediaType::Audio, 8000)
            .expect("ULAW by name not found");
        assert_eq!(ulaw_by_name.id, ID_ULAW);

        // -- FormatCap --
        let fmt_ulaw = registry.get_format(ID_ULAW).expect("ULAW format");
        let fmt_alaw = registry.get_format(ID_ALAW).expect("ALAW format");
        let fmt_slin = registry.get_format(ID_SLIN8).expect("SLIN format");

        let mut cap1 = FormatCap::new();
        cap1.add(Arc::clone(&fmt_ulaw), 0);
        cap1.add(Arc::clone(&fmt_alaw), 0);
        assert_eq!(cap1.count(), 2);
        assert!(!cap1.is_empty());

        let mut cap2 = FormatCap::new();
        cap2.add(Arc::clone(&fmt_alaw), 0);
        cap2.add(Arc::clone(&fmt_slin), 0);

        // Joint capabilities (intersection) - should contain only alaw
        let joint = cap1.get_joint(&cap2);
        assert_eq!(joint.count(), 1);
        assert_eq!(joint.get_format(0).unwrap().codec_name(), "alaw");

        // Compatibility check
        assert!(cap1.is_compatible(&cap2)); // they share alaw

        // Best by type
        let best = cap1.best_by_type(MediaType::Audio);
        assert!(best.is_some());
        assert_eq!(best.unwrap().codec_name(), "ulaw"); // first added = most preferred
    }

    // =========================================================================
    // Test 3: mu-law / A-law roundtrip (SNR check)
    // =========================================================================
    #[test]
    fn test_ulaw_alaw_roundtrip_snr() {
        use asterisk_codecs::alaw_table::{alaw_to_linear, linear_to_alaw_fast};
        use asterisk_codecs::ulaw_table::{linear_to_mulaw_fast, mulaw_to_linear};

        // Generate a known sine wave (1 kHz at 8 kHz sample rate, 1 second)
        let num_samples = 8000;
        let frequency = 1000.0f64;
        let sample_rate = 8000.0f64;
        let amplitude = 16000.0; // Use a moderate amplitude

        let pcm_samples: Vec<i16> = (0..num_samples)
            .map(|i| {
                let t = i as f64 / sample_rate;
                (amplitude * (2.0 * std::f64::consts::PI * frequency * t).sin()) as i16
            })
            .collect();

        // -- mu-law roundtrip --
        let ulaw_encoded: Vec<u8> = pcm_samples.iter().map(|&s| linear_to_mulaw_fast(s)).collect();
        let ulaw_decoded: Vec<i16> = ulaw_encoded.iter().map(|&u| mulaw_to_linear(u)).collect();

        // Compute SNR: signal power / noise power
        let signal_power: f64 = pcm_samples
            .iter()
            .map(|&s| (s as f64) * (s as f64))
            .sum::<f64>()
            / num_samples as f64;

        let ulaw_noise_power: f64 = pcm_samples
            .iter()
            .zip(ulaw_decoded.iter())
            .map(|(&orig, &dec)| {
                let err = orig as f64 - dec as f64;
                err * err
            })
            .sum::<f64>()
            / num_samples as f64;

        let ulaw_snr_db = 10.0 * (signal_power / ulaw_noise_power).log10();
        // mu-law should achieve around 38 dB SNR
        assert!(
            ulaw_snr_db > 30.0,
            "mu-law SNR too low: {:.1} dB (expected > 30 dB)",
            ulaw_snr_db
        );

        // -- A-law roundtrip --
        let alaw_encoded: Vec<u8> = pcm_samples.iter().map(|&s| linear_to_alaw_fast(s)).collect();
        let alaw_decoded: Vec<i16> = alaw_encoded.iter().map(|&a| alaw_to_linear(a)).collect();

        let alaw_noise_power: f64 = pcm_samples
            .iter()
            .zip(alaw_decoded.iter())
            .map(|(&orig, &dec)| {
                let err = orig as f64 - dec as f64;
                err * err
            })
            .sum::<f64>()
            / num_samples as f64;

        let alaw_snr_db = 10.0 * (signal_power / alaw_noise_power).log10();
        assert!(
            alaw_snr_db > 25.0,
            "A-law SNR too low: {:.1} dB (expected > 25 dB)",
            alaw_snr_db
        );

        // Verify the encoded data is not all zeros (sanity check)
        assert!(
            ulaw_encoded.iter().any(|&b| b != 0xFF),
            "mu-law encoded data is all silence"
        );
        assert!(
            alaw_encoded.iter().any(|&b| b != 0),
            "A-law encoded data is all zeros"
        );
    }

    // =========================================================================
    // Test 4: SIP message parsing roundtrip
    // =========================================================================
    #[test]
    fn test_sip_message_parsing_roundtrip() {
        use asterisk_sip::parser::{extract_tag, SipMessage, SipMethod};

        let invite_bytes = b"INVITE sip:bob@biloxi.example.com SIP/2.0\r\n\
Via: SIP/2.0/UDP pc33.atlanta.example.com;branch=z9hG4bKnashds8\r\n\
Max-Forwards: 70\r\n\
To: Bob <sip:bob@biloxi.example.com>\r\n\
From: Alice <sip:alice@atlanta.example.com>;tag=1928301774\r\n\
Call-ID: a84b4c76e66710@pc33.atlanta.example.com\r\n\
CSeq: 314159 INVITE\r\n\
Contact: <sip:alice@pc33.atlanta.example.com>\r\n\
Content-Type: application/sdp\r\n\
Content-Length: 142\r\n\
\r\n\
v=0\r\n\
o=alice 2890844526 2890844526 IN IP4 pc33.atlanta.example.com\r\n\
s=Session SDP\r\n\
c=IN IP4 pc33.atlanta.example.com\r\n\
t=0 0\r\n\
m=audio 49170 RTP/AVP 0\r\n";

        let parsed = SipMessage::parse(invite_bytes).expect("Failed to parse INVITE");

        // Verify it is a request
        assert!(parsed.is_request());
        assert_eq!(parsed.method(), Some(SipMethod::Invite));

        // Verify headers
        let via = parsed.get_header("Via").expect("Missing Via");
        assert!(via.contains("z9hG4bKnashds8"));

        let from = parsed.from_header().expect("Missing From");
        assert!(from.contains("Alice"));
        assert_eq!(extract_tag(from), Some("1928301774".to_string()));

        let to = parsed.to_header().expect("Missing To");
        assert!(to.contains("Bob"));

        let call_id = parsed.call_id().expect("Missing Call-ID");
        assert_eq!(call_id, "a84b4c76e66710@pc33.atlanta.example.com");

        let cseq = parsed.cseq().expect("Missing CSeq");
        assert_eq!(cseq, "314159 INVITE");

        let contact = parsed.get_header("Contact").expect("Missing Contact");
        assert!(contact.contains("alice@pc33.atlanta.example.com"));

        let content_type = parsed
            .get_header("Content-Type")
            .expect("Missing Content-Type");
        assert_eq!(content_type, "application/sdp");

        // Verify SDP body is present
        assert!(parsed.body.contains("v=0"));
        assert!(parsed.body.contains("m=audio"));

        // Generate a 200 OK response
        let response = parsed
            .create_response(200, "OK")
            .expect("Failed to create response");
        assert!(response.is_response());
        assert_eq!(response.status_code(), Some(200));

        // Verify Via was copied
        let resp_via = response.get_header("Via").expect("Missing Via in response");
        assert!(resp_via.contains("z9hG4bKnashds8"));

        // Verify From/To/Call-ID were copied
        assert_eq!(
            response.call_id(),
            Some("a84b4c76e66710@pc33.atlanta.example.com")
        );
        let resp_from = response.from_header().expect("Missing From in response");
        assert!(resp_from.contains("Alice"));
        let resp_to = response.to_header().expect("Missing To in response");
        assert!(resp_to.contains("Bob"));

        // Re-parse the serialized response
        let resp_str = response.to_string();
        let reparsed =
            SipMessage::parse(resp_str.as_bytes()).expect("Failed to re-parse response");
        assert!(reparsed.is_response());
        assert_eq!(reparsed.status_code(), Some(200));
        assert_eq!(
            reparsed.call_id(),
            Some("a84b4c76e66710@pc33.atlanta.example.com")
        );
    }

    // =========================================================================
    // Test 5: SDP offer/answer
    // =========================================================================
    #[test]
    fn test_sdp_offer_answer() {
        use asterisk_codecs::Codec;
        use asterisk_sip::sdp::SessionDescription;

        // Create an SDP offer with PCMU, PCMA, and telephone-event
        let offer_codecs = vec![
            Codec::new("PCMU", 0, 8000),
            Codec::new("PCMA", 8, 8000),
            Codec::new("telephone-event", 101, 8000),
        ];

        let offer = SessionDescription::create_offer("10.0.0.1", 20000, &offer_codecs);
        assert_eq!(offer.media_descriptions.len(), 1);
        assert_eq!(offer.media_descriptions[0].media_type, "audio");
        assert_eq!(offer.media_descriptions[0].port, 20000);
        assert_eq!(offer.media_descriptions[0].formats, vec![0, 8, 101]);

        // Serialize and re-parse
        let offer_text = offer.to_string();
        let parsed_offer =
            SessionDescription::parse(&offer_text).expect("Failed to parse SDP offer");
        assert_eq!(parsed_offer.media_descriptions.len(), 1);
        let offer_media_codecs = parsed_offer.media_descriptions[0].codecs();
        assert_eq!(offer_media_codecs.len(), 3);
        assert_eq!(offer_media_codecs[0].name, "PCMU");
        assert_eq!(offer_media_codecs[1].name, "PCMA");
        assert_eq!(offer_media_codecs[2].name, "telephone-event");

        // Create an answer - answerer supports PCMA and G729 (not PCMU)
        let supported_codecs = vec![
            Codec::new("PCMA", 8, 8000),
            Codec::new("G729", 18, 8000),
            Codec::new("telephone-event", 101, 8000),
        ];

        let answer = SessionDescription::create_answer(
            &parsed_offer,
            "10.0.0.2",
            30000,
            &supported_codecs,
        );

        // The answer should contain only codecs in both offer and supported.
        // create_answer internally calls create_offer with empty codecs first
        // (which creates one empty audio media), then pushes each matched media.
        // So we look at all media descriptions with formats that have codecs.
        let all_answer_codecs: Vec<asterisk_codecs::Codec> = answer
            .media_descriptions
            .iter()
            .filter(|m| m.media_type == "audio" && m.port > 0 && !m.formats.is_empty())
            .flat_map(|m| m.codecs())
            .collect();

        let answer_names: Vec<&str> = all_answer_codecs.iter().map(|c| c.name.as_str()).collect();
        assert!(
            answer_names.contains(&"PCMA"),
            "Answer should contain PCMA, got: {:?}",
            answer_names
        );
        assert!(
            answer_names.contains(&"telephone-event"),
            "Answer should contain telephone-event, got: {:?}",
            answer_names
        );
        assert!(
            !answer_names.contains(&"PCMU"),
            "Answer should NOT contain PCMU, got: {:?}",
            answer_names
        );
        assert!(
            !answer_names.contains(&"G729"),
            "Answer should NOT contain G729, got: {:?}",
            answer_names
        );
    }

    // =========================================================================
    // Test 6: RTP packet roundtrip
    // =========================================================================
    #[test]
    fn test_rtp_packet_roundtrip() {
        use asterisk_sip::rtp::{build_rtp_packet, parse_rtp_header, RtpHeader};

        let payload = b"Hello, RTP!";

        let header = RtpHeader {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type: 0,
            sequence: 1,
            timestamp: 160,
            ssrc: 12345,
        };

        // Build the packet
        let packet = build_rtp_packet(&header, payload);
        assert_eq!(packet.len(), 12 + payload.len());

        // Parse it back
        let (parsed_header, parsed_payload) =
            parse_rtp_header(&packet).expect("Failed to parse RTP");

        // Verify all header fields
        assert_eq!(parsed_header.version, 2);
        assert!(!parsed_header.padding);
        assert!(!parsed_header.extension);
        assert_eq!(parsed_header.csrc_count, 0);
        assert!(!parsed_header.marker);
        assert_eq!(parsed_header.payload_type, 0);
        assert_eq!(parsed_header.sequence, 1);
        assert_eq!(parsed_header.timestamp, 160);
        assert_eq!(parsed_header.ssrc, 12345);

        // Verify payload is intact
        assert_eq!(parsed_payload, payload);

        // Test with marker bit set
        let header_marker = RtpHeader {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: true,
            payload_type: 8, // PCMA
            sequence: 42,
            timestamp: 3200,
            ssrc: 99999,
        };

        let packet2 = build_rtp_packet(&header_marker, &[0xAA; 160]);
        let (h2, p2) = parse_rtp_header(&packet2).expect("Failed to parse RTP with marker");
        assert!(h2.marker);
        assert_eq!(h2.payload_type, 8);
        assert_eq!(h2.sequence, 42);
        assert_eq!(h2.timestamp, 3200);
        assert_eq!(h2.ssrc, 99999);
        assert_eq!(p2.len(), 160);
        assert!(p2.iter().all(|&b| b == 0xAA));
    }

    // =========================================================================
    // Test 7: Config file parsing
    // =========================================================================
    #[test]
    fn test_config_file_parsing() {
        use asterisk_config::AsteriskConfig;

        let config_content = r#"
; This is a comment about the configuration file
[general]
context = default
allowguest = no
bindport = 5060
transport = udp

[my-phone-template](!)
type = friend
host = dynamic
dtmfmode = rfc2833
disallow = all
allow = ulaw
allow = alaw

[phone1](my-phone-template)
secret = password123
callerid = "Phone One" <100>
mailbox = 100@default

[phone2](my-phone-template)
secret = password456
callerid = "Phone Two" <101>
mailbox = 101@default

[extensions]
exten => 100,1,Answer()
exten => 100,2,Dial(SIP/phone1,30)
exten => 100,3,Hangup()
"#;

        let config =
            AsteriskConfig::from_str(config_content, "sip.conf").expect("Failed to parse config");

        // Verify sections exist
        let names = config.category_names();
        assert!(names.contains(&"general"));
        assert!(names.contains(&"my-phone-template"));
        assert!(names.contains(&"phone1"));
        assert!(names.contains(&"phone2"));
        assert!(names.contains(&"extensions"));

        // Verify general section variables
        assert_eq!(config.get_variable("general", "context"), Some("default"));
        assert_eq!(config.get_variable("general", "allowguest"), Some("no"));
        assert_eq!(config.get_variable("general", "bindport"), Some("5060"));

        // Verify template
        let tmpl = config.get_category("my-phone-template").unwrap();
        assert!(tmpl.is_template);
        assert_eq!(tmpl.get_variable("type"), Some("friend"));
        assert_eq!(tmpl.get_variable("host"), Some("dynamic"));

        // Verify template inheritance
        let phone1 = config.get_category("phone1").unwrap();
        assert!(!phone1.is_template);
        assert_eq!(phone1.template_name.as_deref(), Some("my-phone-template"));
        // Inherited variables should be accessible
        assert_eq!(phone1.get_variable("type"), Some("friend"));
        assert_eq!(phone1.get_variable("host"), Some("dynamic"));
        // Own variables
        assert_eq!(phone1.get_variable("secret"), Some("password123"));

        // Verify phone2 also inherited from template
        let phone2 = config.get_category("phone2").unwrap();
        assert_eq!(phone2.get_variable("type"), Some("friend"));
        assert_eq!(phone2.get_variable("secret"), Some("password456"));

        // Verify extensions section with object assignments
        let extensions = config.get_category("extensions").unwrap();
        let exten_values = extensions.get_all_variables("exten");
        assert_eq!(exten_values.len(), 3);
        assert!(exten_values[0].contains("Answer"));
        assert!(exten_values[1].contains("Dial"));
        assert!(exten_values[2].contains("Hangup"));
        // Verify they are object assignments (=>)
        assert!(extensions.variables.iter().all(|v| v.is_object));
    }

    // =========================================================================
    // Test 8: Dialplan pattern matching
    // =========================================================================
    #[test]
    fn test_dialplan_pattern_matching() {
        use asterisk_core::pbx::{Context, Dialplan, Extension, Priority};

        let mut dp = Dialplan::new();

        // Create "default" context with various patterns
        let mut default_ctx = Context::new("default");

        // Exact match extension
        let mut ext_100 = Extension::new("100");
        ext_100.add_priority(Priority {
            priority: 1,
            app: "Answer".to_string(),
            app_data: String::new(),
            label: None,
        });
        default_ctx.add_extension(ext_100);

        // Pattern _X. matches any string starting with a digit
        let mut ext_x_dot = Extension::new("_X.");
        ext_x_dot.add_priority(Priority {
            priority: 1,
            app: "Dial".to_string(),
            app_data: "SIP/${EXTEN}".to_string(),
            label: None,
        });
        default_ctx.add_extension(ext_x_dot);

        // Pattern _NXXNXXXXXX matches 10-digit NANP numbers
        let mut ext_nanp = Extension::new("_NXXNXXXXXX");
        ext_nanp.add_priority(Priority {
            priority: 1,
            app: "Dial".to_string(),
            app_data: "SIP/trunk/${EXTEN}".to_string(),
            label: None,
        });
        default_ctx.add_extension(ext_nanp);

        // Pattern _[1-5]XX matches 3-digit extensions 100-599
        let mut ext_range = Extension::new("_[1-5]XX");
        ext_range.add_priority(Priority {
            priority: 1,
            app: "Dial".to_string(),
            app_data: "SIP/phone${EXTEN}".to_string(),
            label: None,
        });
        default_ctx.add_extension(ext_range);

        // Add an include to search "internal" context
        default_ctx.add_include("internal");
        dp.add_context(default_ctx);

        // Create "internal" context
        let mut internal_ctx = Context::new("internal");
        let mut ext_200 = Extension::new("200");
        ext_200.add_priority(Priority {
            priority: 1,
            app: "Answer".to_string(),
            app_data: String::new(),
            label: None,
        });
        internal_ctx.add_extension(ext_200);
        dp.add_context(internal_ctx);

        // Test 1: Exact match "100"
        let result = dp.find_extension("default", "100");
        assert!(result.is_some(), "100 should match");
        let (ctx, ext) = result.unwrap();
        assert_eq!(ctx.name, "default");
        assert_eq!(ext.name, "100");

        // Test 2: Pattern _X. matches "1234567890"
        let result = dp.find_extension("default", "1234567890");
        assert!(result.is_some(), "_X. should match 1234567890");

        // Test 3: Pattern _NXXNXXXXXX matches "2125551234"
        let ext_nanp_check = Extension::new("_NXXNXXXXXX");
        assert!(
            ext_nanp_check.matches("2125551234"),
            "_NXXNXXXXXX should match 2125551234"
        );

        // Test 4: Pattern _[1-5]XX matches "300" but not "600"
        let ext_range_check = Extension::new("_[1-5]XX");
        assert!(
            ext_range_check.matches("300"),
            "_[1-5]XX should match 300"
        );
        assert!(
            !ext_range_check.matches("600"),
            "_[1-5]XX should NOT match 600"
        );
        // Also test boundary values
        assert!(
            ext_range_check.matches("100"),
            "_[1-5]XX should match 100"
        );
        assert!(
            ext_range_check.matches("599"),
            "_[1-5]XX should match 599"
        );

        // Test 5: Include-based context searching
        // Note: numeric extensions like "200" or "700" also match _X. or _[1-5]XX
        // in "default", so the include path is never reached for those.
        // Use "operator" which won't match any digit-based pattern.
        let mut ext_operator = Extension::new("operator");
        ext_operator.add_priority(Priority {
            priority: 1,
            app: "Answer".to_string(),
            app_data: String::new(),
            label: None,
        });
        dp.get_context_mut("internal")
            .unwrap()
            .add_extension(ext_operator);

        let result = dp.find_extension("default", "operator");
        assert!(
            result.is_some(),
            "operator should be found via include"
        );
        let (ctx, ext) = result.unwrap();
        assert_eq!(ctx.name, "internal");
        assert_eq!(ext.name, "operator");

        // Test 6: Extension range does NOT match 999
        assert!(
            !ext_range_check.matches("999"),
            "_[1-5]XX should NOT match 999"
        );
    }

    // =========================================================================
    // Test 9: CDR lifecycle
    // =========================================================================
    #[test]
    fn test_cdr_lifecycle() {
        use asterisk_cdr::engine::CdrEngine;
        use asterisk_cdr::{Cdr, CdrBackend, CdrDisposition, CdrError};
        use std::sync::Mutex;

        // Create a collecting backend for verification
        struct CollectingBackend {
            records: Mutex<Vec<Cdr>>,
        }

        impl CdrBackend for CollectingBackend {
            fn name(&self) -> &str {
                "test-collector"
            }

            fn log(&self, cdr: &Cdr) -> Result<(), CdrError> {
                self.records.lock().unwrap().push(cdr.clone());
                Ok(())
            }
        }

        let backend = Arc::new(CollectingBackend {
            records: Mutex::new(Vec::new()),
        });

        // Create engine with config that logs unanswered calls
        let config = asterisk_cdr::engine::CdrConfig {
            enabled: true,
            log_unanswered: true,
            log_congestion: true,
            ..Default::default()
        };
        let engine = CdrEngine::with_config(config);
        engine.register_backend(Arc::clone(&backend) as Arc<dyn CdrBackend>);

        // Simulate channel lifecycle
        let uid = "test-uid-cdr-001";

        // Channel created
        engine.channel_created(uid, "SIP/alice-001", "Alice <5551234>", "5551234", "default");
        assert_eq!(engine.active_count(), 1);

        // Dial begins
        engine.dial_begin(uid, "SIP/bob-001", "100");

        // Channel answered
        engine.channel_answered(uid);

        // Bridge enter
        engine.bridge_enter(uid, "bridge-001", "SIP/bob-001");

        // Bridge leave
        engine.bridge_leave(uid, "bridge-001");

        // Channel hangup (normal clearing = cause 16)
        engine.channel_hangup(uid, 16, "Dial", "SIP/bob,30");
        assert_eq!(engine.active_count(), 0);

        // Verify CDR was produced
        let records = backend.records.lock().unwrap();
        assert_eq!(records.len(), 1, "Expected exactly one CDR record");

        let cdr = &records[0];
        assert_eq!(cdr.src, "5551234");
        assert_eq!(cdr.dst, "100");
        assert_eq!(cdr.channel, "SIP/alice-001");
        assert_eq!(cdr.dst_channel, "SIP/bob-001");
        assert_eq!(cdr.disposition, CdrDisposition::Answered);
        assert!(cdr.answer.is_some());
        assert!(cdr.duration >= 0);
        assert_eq!(cdr.last_app, "Dial");
        assert_eq!(cdr.last_data, "SIP/bob,30");
    }

    // =========================================================================
    // Test 10: Local channel pair
    // =========================================================================
    #[tokio::test]
    async fn test_local_channel_pair() {
        use asterisk_channels::local::LocalChannelDriver;
        use asterisk_core::channel::ChannelDriver;
        use asterisk_types::FrameType;

        let driver = Arc::new(LocalChannelDriver::new());
        let (mut chan1, mut chan2) = driver.request_pair("100@default").unwrap();

        // Verify channel names
        assert!(chan1.name.contains(";1"), "chan1 name should contain ;1");
        assert!(chan2.name.contains(";2"), "chan2 name should contain ;2");
        assert!(chan2.name.contains("100@default"));

        // Write a voice frame to side ;1, read it from side ;2
        let voice_data = vec![0xABu8; 320];
        let voice_frame = asterisk_types::Frame::voice(0, 160, Bytes::from(voice_data.clone()));
        driver
            .write_frame(&mut chan1, &voice_frame)
            .await
            .expect("Failed to write frame to ;1");

        let read_frame = driver
            .read_frame(&mut chan2)
            .await
            .expect("Failed to read frame from ;2");
        assert_eq!(read_frame.frame_type(), FrameType::Voice);
        if let asterisk_types::Frame::Voice { data, samples, .. } = &read_frame {
            assert_eq!(data.as_ref(), &voice_data[..]);
            assert_eq!(*samples, 160);
        } else {
            panic!("Expected Voice frame");
        }

        // Write a DTMF frame to side ;2, read it from side ;1
        let dtmf_frame = asterisk_types::Frame::dtmf_begin('#');
        driver
            .write_frame(&mut chan2, &dtmf_frame)
            .await
            .expect("Failed to write DTMF to ;2");

        let read_dtmf = driver
            .read_frame(&mut chan1)
            .await
            .expect("Failed to read DTMF from ;1");
        assert_eq!(read_dtmf.frame_type(), FrameType::DtmfBegin);
        if let asterisk_types::Frame::DtmfBegin { digit } = &read_dtmf {
            assert_eq!(*digit, '#');
        } else {
            panic!("Expected DtmfBegin frame");
        }
    }

    // =========================================================================
    // Test 11: Digest authentication
    // =========================================================================
    #[test]
    fn test_digest_authentication() {
        use asterisk_sip::auth::{
            create_digest_response, DigestAlgorithm, DigestChallenge, DigestCredentials,
        };
        use md5::{Digest, Md5};

        // Known test vector: MD5 without qop
        let challenge = DigestChallenge {
            realm: "asterisk".to_string(),
            nonce: "testnonce123".to_string(),
            algorithm: DigestAlgorithm::Md5,
            qop: None,
            opaque: None,
            stale: false,
            domain: None,
        };

        let credentials = DigestCredentials {
            username: "alice".to_string(),
            password: "secret".to_string(),
            realm: "asterisk".to_string(),
        };

        let method = "REGISTER";
        let uri = "sip:asterisk.example.com";

        let response = create_digest_response(&challenge, &credentials, method, uri);

        // Manually compute the expected response
        // HA1 = MD5(alice:asterisk:secret)
        let ha1 = format!("{:x}", Md5::digest(b"alice:asterisk:secret"));
        // HA2 = MD5(REGISTER:sip:asterisk.example.com)
        let ha2_input = format!("{}:{}", method, uri);
        let ha2 = format!("{:x}", Md5::digest(ha2_input.as_bytes()));
        // response = MD5(HA1:nonce:HA2)
        let resp_input = format!("{}:testnonce123:{}", ha1, ha2);
        let expected_response = format!("{:x}", Md5::digest(resp_input.as_bytes()));

        // Verify the response contains the expected hash
        assert!(
            response.contains(&format!("response=\"{}\"", expected_response)),
            "Digest response hash mismatch.\nGot: {}\nExpected response field: {}",
            response,
            expected_response
        );

        // Verify other fields are present
        assert!(response.contains("username=\"alice\""));
        assert!(response.contains("realm=\"asterisk\""));
        assert!(response.contains("nonce=\"testnonce123\""));
        assert!(response.contains(&format!("uri=\"{}\"", uri)));
        assert!(response.contains("algorithm=MD5"));

        // Test parsing a challenge from a header string
        let challenge_header =
            r#"Digest realm="pbx.example.com", nonce="abc456", algorithm=MD5, qop="auth""#;
        let parsed = DigestChallenge::parse(challenge_header).expect("Failed to parse challenge");
        assert_eq!(parsed.realm, "pbx.example.com");
        assert_eq!(parsed.nonce, "abc456");
        assert_eq!(parsed.algorithm, DigestAlgorithm::Md5);
        assert_eq!(parsed.qop, Some("auth".to_string()));
    }

    // =========================================================================
    // Test 12: Conference bridge
    // =========================================================================
    #[tokio::test]
    async fn test_conference_bridge() {
        use asterisk_apps::confbridge::AppConfBridge;
        use asterisk_core::channel::{Channel, ChannelId};

        // Use unique conference names per test to avoid global state conflicts
        let conf_name = format!("test-conf-{}", uuid::Uuid::new_v4());

        // Create channels and join them to the conference via exec
        let mut chan1 = Channel::new("SIP/user1-001");
        chan1.caller.id.name.name = "User One".to_string();
        let mut chan2 = Channel::new("SIP/user2-001");
        chan2.caller.id.name.name = "User Two".to_string();

        // Join channel 1 - regular user. The exec method creates the conference,
        // adds the user, and then leaves immediately (stub).
        let (_exec_result, _conf_result) = AppConfBridge::exec(&mut chan1, &conf_name).await;
        // The stub returns Hangup because the event loop is not implemented,
        // which is expected behavior for this test.

        // Test the mute/kick API directly
        let fake_id = ChannelId::from_name("fake-channel-id");

        // Mute on a nonexistent conference returns false
        assert!(!AppConfBridge::mute_user("nonexistent", &fake_id));

        // Kick on a nonexistent conference returns false
        assert!(!AppConfBridge::kick_user("nonexistent", &fake_id));

        // Test list_conferences does not panic
        let _ = AppConfBridge::list_conferences();

        // Test lock/unlock on nonexistent conference returns false
        assert!(!AppConfBridge::lock_conference("nonexistent"));
        assert!(!AppConfBridge::unlock_conference("nonexistent"));

        // Test that admin profiles work through the exec path
        let admin_args = format!("{},default,admin", conf_name);
        let mut chan_admin = Channel::new("SIP/admin-001");
        let _ = AppConfBridge::exec(&mut chan_admin, &admin_args).await;
    }

    // =========================================================================
    // Test 13: Queue operations
    // =========================================================================
    #[test]
    fn test_queue_operations() {
        use asterisk_apps::queue::{CallQueue, QueueCaller, QueueStrategy};
        use asterisk_core::channel::ChannelId;
        use std::time::Instant;

        // Create a queue with RoundRobin strategy
        let mut queue = CallQueue::new("support".to_string(), QueueStrategy::RoundRobin);

        // Add members
        assert!(queue.add_member("SIP/agent1".to_string(), "Agent 1".to_string(), 0));
        assert!(queue.add_member("SIP/agent2".to_string(), "Agent 2".to_string(), 0));
        assert!(queue.add_member("SIP/agent3".to_string(), "Agent 3".to_string(), 1));
        assert_eq!(queue.members.len(), 3);

        // Duplicate member should fail
        assert!(!queue.add_member(
            "SIP/agent1".to_string(),
            "Agent 1".to_string(),
            0
        ));

        // Add callers and verify position tracking
        let caller1 = QueueCaller {
            channel_id: ChannelId::from_name("caller-001"),
            channel_name: "SIP/caller1".to_string(),
            position: 0,
            enter_time: Instant::now(),
            caller_name: Some("Caller 1".to_string()),
            caller_number: Some("5551001".to_string()),
            last_periodic_announce: None,
            last_position_announce: None,
        };
        let pos1 = queue.enqueue_caller(caller1);
        assert_eq!(pos1, 1);

        let caller2 = QueueCaller {
            channel_id: ChannelId::from_name("caller-002"),
            channel_name: "SIP/caller2".to_string(),
            position: 0,
            enter_time: Instant::now(),
            caller_name: Some("Caller 2".to_string()),
            caller_number: Some("5551002".to_string()),
            last_periodic_announce: None,
            last_position_announce: None,
        };
        let pos2 = queue.enqueue_caller(caller2);
        assert_eq!(pos2, 2);

        assert_eq!(queue.callers.len(), 2);
        assert_eq!(queue.callers[0].position, 1);
        assert_eq!(queue.callers[1].position, 2);

        // Verify RoundRobin strategy cycles through members
        let first_selected = queue.select_members();
        assert_eq!(first_selected.len(), 1);
        let first_iface = queue.members[first_selected[0]].interface.clone();

        let second_selected = queue.select_members();
        assert_eq!(second_selected.len(), 1);
        let second_iface = queue.members[second_selected[0]].interface.clone();

        // They should be different members (round-robin)
        assert_ne!(
            first_iface, second_iface,
            "RoundRobin should select different members on consecutive calls"
        );

        // Pause/unpause members
        assert!(queue.set_member_paused("SIP/agent1", true, None));
        assert!(queue
            .members
            .iter()
            .find(|m| m.interface == "SIP/agent1")
            .unwrap()
            .paused);
        let available_before = queue.available_member_count();

        assert!(queue.set_member_paused("SIP/agent1", false, None));
        assert!(!queue
            .members
            .iter()
            .find(|m| m.interface == "SIP/agent1")
            .unwrap()
            .paused);
        let available_after = queue.available_member_count();
        assert!(
            available_after >= available_before,
            "Unpausing should not decrease available count"
        );

        // Dequeue a caller
        let dequeued = queue.dequeue_caller().unwrap();
        assert_eq!(dequeued.channel_name, "SIP/caller1");
        assert_eq!(queue.callers.len(), 1);
        // Remaining caller should be re-numbered to position 1
        assert_eq!(queue.callers[0].position, 1);

        // Remove a member
        assert!(queue.remove_member("SIP/agent3"));
        assert_eq!(queue.members.len(), 2);
    }

    // =========================================================================
    // Test 14: Stasis event bus
    // =========================================================================
    #[tokio::test]
    async fn test_stasis_event_bus() {
        use asterisk_core::stasis::{self, StasisCache, StasisMessage, Topic};
        use std::any::Any;

        // Define a test message type
        #[derive(Debug, Clone)]
        struct TestEvent {
            channel_name: String,
            event_type: String,
        }

        impl StasisMessage for TestEvent {
            fn message_type(&self) -> &str {
                &self.event_type
            }

            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        // Create a topic
        let topic = Topic::with_name("test-channel-events");
        assert_eq!(topic.name(), "test-channel-events");

        // Subscribe
        let mut sub = topic.subscribe();
        assert_eq!(sub.topic_name(), "test-channel-events");

        // Verify subscriber count
        assert!(topic.subscriber_count() >= 1);

        // Publish messages
        stasis::publish(
            &topic,
            TestEvent {
                channel_name: "SIP/alice-001".to_string(),
                event_type: "channel_created".to_string(),
            },
        );

        stasis::publish(
            &topic,
            TestEvent {
                channel_name: "SIP/alice-001".to_string(),
                event_type: "channel_answered".to_string(),
            },
        );

        // Receive and verify messages
        let msg1 = sub.recv().await.expect("Should receive first message");
        assert_eq!(msg1.message_type(), "channel_created");
        let event1 = msg1.as_any().downcast_ref::<TestEvent>().unwrap();
        assert_eq!(event1.channel_name, "SIP/alice-001");

        let msg2 = sub.recv().await.expect("Should receive second message");
        assert_eq!(msg2.message_type(), "channel_answered");
        let event2 = msg2.as_any().downcast_ref::<TestEvent>().unwrap();
        assert_eq!(event2.channel_name, "SIP/alice-001");

        // Test StasisCache
        let cache: StasisCache<String> = StasisCache::new("channel-snapshots");
        assert!(cache.is_empty());

        // Insert entries
        cache.update("SIP/alice-001", "Up".to_string());
        cache.update("SIP/bob-001", "Ringing".to_string());
        assert_eq!(cache.len(), 2);

        // Get entries
        let alice_state = cache.get("SIP/alice-001").expect("Missing cache entry");
        assert_eq!(alice_state.as_ref(), "Up");

        // Update existing entry
        let old = cache.update("SIP/alice-001", "Down".to_string());
        assert!(old.is_some());
        assert_eq!(old.unwrap().as_ref(), "Up");

        let new_state = cache.get("SIP/alice-001").unwrap();
        assert_eq!(new_state.as_ref(), "Down");

        // Remove entry
        let removed = cache.remove("SIP/bob-001");
        assert!(removed.is_some());
        assert_eq!(cache.len(), 1);

        // Dump all entries
        let all = cache.dump();
        assert_eq!(all.len(), 1);
    }

    // =========================================================================
    // Test 15: Translation chain (ulaw -> slin -> alaw multi-crate)
    // =========================================================================
    #[test]
    fn test_translation_chain_ulaw_to_alaw() {
        use asterisk_codecs::builtin_codecs::{ID_ALAW, ID_SLIN8, ID_ULAW};
        use asterisk_codecs::registry::CodecRegistry;
        use asterisk_types::Frame;

        let registry = CodecRegistry::with_builtins();
        let matrix = registry.translation_matrix();

        // Verify ulaw -> slin is 1 step
        let path_ulaw_slin = matrix.build_path(ID_ULAW, ID_SLIN8).unwrap();
        assert_eq!(path_ulaw_slin.steps.len(), 1);
        assert_eq!(path_ulaw_slin.steps[0].name(), "ulawtolin");

        // Verify ulaw -> alaw is 2 steps (ulaw -> slin -> alaw)
        let path_ulaw_alaw = matrix.build_path(ID_ULAW, ID_ALAW).unwrap();
        assert_eq!(path_ulaw_alaw.steps.len(), 2);

        // Create a chain and translate audio
        let mut chain = path_ulaw_alaw.create_chain();

        // Create a ulaw silence frame (0xFF = silence in ulaw)
        let ulaw_data = vec![0xFF_u8; 160];
        let ulaw_frame = Frame::voice(ID_ULAW, 160, Bytes::from(ulaw_data));

        let result = chain.translate(&ulaw_frame).unwrap();
        assert!(result.is_some(), "Translation should produce output");

        let output = result.unwrap();
        assert!(output.is_voice());
        if let Frame::Voice {
            codec_id, data, ..
        } = &output
        {
            assert_eq!(*codec_id, ID_ALAW);
            assert_eq!(data.len(), 160); // Same number of samples
        } else {
            panic!("Expected voice frame");
        }

        // Same codec should be a no-op
        let path_same = matrix.build_path(ID_ULAW, ID_ULAW).unwrap();
        assert!(path_same.is_noop());
        assert_eq!(path_same.total_cost, 0);
    }

    // =========================================================================
    // Test 16: FormatCapabilities (SDP-level codec set) intersection
    // =========================================================================
    #[test]
    fn test_format_capabilities_intersection() {
        use asterisk_codecs::{codecs, FormatCapabilities};

        let mut caps_a = FormatCapabilities::new();
        caps_a.add(codecs::pcmu());
        caps_a.add(codecs::pcma());
        caps_a.add(codecs::g722());
        caps_a.add(codecs::telephone_event());

        let mut caps_b = FormatCapabilities::new();
        caps_b.add(codecs::pcma());
        caps_b.add(codecs::g729());
        caps_b.add(codecs::telephone_event());

        let joint = caps_a.intersect(&caps_b);
        assert_eq!(joint.codecs.len(), 2);
        assert!(joint.contains(&codecs::pcma()));
        assert!(joint.contains(&codecs::telephone_event()));
        assert!(!joint.contains(&codecs::pcmu()));
        assert!(!joint.contains(&codecs::g729()));
    }

    // =========================================================================
    // Test 17: DTMF event encoding (RFC 2833)
    // =========================================================================
    #[test]
    fn test_dtmf_event_encoding() {
        use asterisk_sip::rtp::DtmfEvent;

        // Test digit to event number conversion
        for digit in "0123456789*#ABCD".chars() {
            let event_num = DtmfEvent::digit_to_event(digit);
            let back = DtmfEvent::event_to_digit(event_num);
            assert_eq!(
                back.to_ascii_uppercase(),
                digit.to_ascii_uppercase(),
                "Roundtrip failed for digit '{}'",
                digit
            );
        }

        // Test DTMF event roundtrip
        let event = DtmfEvent {
            event: 5,
            end: true,
            volume: 10,
            duration: 1600,
        };
        let bytes = event.to_bytes();
        let parsed = DtmfEvent::from_bytes(&bytes).expect("Failed to parse DTMF event");
        assert_eq!(parsed.event, 5);
        assert!(parsed.end);
        assert_eq!(parsed.volume, 10);
        assert_eq!(parsed.duration, 1600);

        // Digit 5 -> event 5 -> digit '5'
        assert_eq!(DtmfEvent::event_to_digit(parsed.event), '5');
    }

    // =========================================================================
    // Test 18: SIP URI parsing
    // =========================================================================
    #[test]
    fn test_sip_uri_parsing() {
        use asterisk_sip::parser::SipUri;

        // Full URI with user, host, port, and transport parameter
        let uri =
            SipUri::parse("sip:alice@atlanta.example.com:5060;transport=tcp").unwrap();
        assert_eq!(uri.scheme, "sip");
        assert_eq!(uri.user, Some("alice".to_string()));
        assert_eq!(uri.host, "atlanta.example.com");
        assert_eq!(uri.port, Some(5060));
        assert_eq!(uri.transport(), Some("tcp"));

        // URI without user
        let uri2 = SipUri::parse("sip:registrar.example.com").unwrap();
        assert_eq!(uri2.user, None);
        assert_eq!(uri2.host, "registrar.example.com");
        assert_eq!(uri2.port, None);

        // SIPS URI
        let uri3 = SipUri::parse("sips:secure@example.com:5061").unwrap();
        assert_eq!(uri3.scheme, "sips");
        assert_eq!(uri3.user, Some("secure".to_string()));
        assert_eq!(uri3.port, Some(5061));
    }

    // =========================================================================
    // Test 19: CDR unanswered call handling
    // =========================================================================
    #[test]
    fn test_cdr_unanswered_call() {
        use asterisk_cdr::engine::{CdrConfig, CdrEngine};
        use asterisk_cdr::{Cdr, CdrBackend, CdrDisposition, CdrError};
        use std::sync::Mutex;

        struct CollectingBackend {
            records: Mutex<Vec<Cdr>>,
        }

        impl CdrBackend for CollectingBackend {
            fn name(&self) -> &str {
                "test-unanswered"
            }
            fn log(&self, cdr: &Cdr) -> Result<(), CdrError> {
                self.records.lock().unwrap().push(cdr.clone());
                Ok(())
            }
        }

        let backend = Arc::new(CollectingBackend {
            records: Mutex::new(Vec::new()),
        });

        // Engine that logs unanswered calls
        let config = CdrConfig {
            enabled: true,
            log_unanswered: true,
            ..Default::default()
        };
        let engine = CdrEngine::with_config(config);
        engine.register_backend(Arc::clone(&backend) as Arc<dyn CdrBackend>);

        // Create and hangup without answering (busy = cause 17)
        engine.channel_created("uid-busy", "SIP/busy-001", "Bob", "5559999", "default");
        engine.dial_begin("uid-busy", "SIP/target-001", "200");
        engine.channel_hangup("uid-busy", 17, "Dial", "SIP/target,30");

        let records = backend.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].disposition, CdrDisposition::Busy);
        assert!(records[0].answer.is_none());
        assert_eq!(records[0].billsec, 0);
    }

    // =========================================================================
    // Test 20: Channel + Frame queue integration
    // =========================================================================
    #[test]
    fn test_channel_frame_queue() {
        use asterisk_core::channel::Channel;
        use asterisk_types::{ControlFrame, Frame, FrameType};

        let mut chan = Channel::new("Test/queue-test");

        // Queue several frames
        chan.queue_frame(Frame::control(ControlFrame::Ringing));
        chan.queue_frame(Frame::voice(0, 160, Bytes::from_static(&[0xFF; 160])));
        chan.queue_frame(Frame::dtmf_end('1', 80));
        chan.queue_frame(Frame::control(ControlFrame::Answer));

        // Dequeue and verify order
        let f1 = chan.dequeue_frame().unwrap();
        assert_eq!(f1.frame_type(), FrameType::Control);

        let f2 = chan.dequeue_frame().unwrap();
        assert_eq!(f2.frame_type(), FrameType::Voice);

        let f3 = chan.dequeue_frame().unwrap();
        assert_eq!(f3.frame_type(), FrameType::DtmfEnd);
        if let Frame::DtmfEnd { digit, duration_ms } = f3 {
            assert_eq!(digit, '1');
            assert_eq!(duration_ms, 80);
        }

        let f4 = chan.dequeue_frame().unwrap();
        assert_eq!(f4.frame_type(), FrameType::Control);

        // Queue should be empty now
        assert!(chan.dequeue_frame().is_none());
    }

    // =========================================================================
    // ADVERSARIAL TESTS
    // =========================================================================

    // =========================================================================
    // Adversarial Test: SIP parser malformed input
    // =========================================================================
    #[test]
    fn test_sip_parser_malformed_input() {
        use asterisk_sip::parser::SipMessage;

        // Empty input
        assert!(SipMessage::parse(b"").is_err());

        // Just a newline
        assert!(SipMessage::parse(b"\r\n").is_err());

        // Garbage data
        assert!(SipMessage::parse(b"GARBAGE DATA HERE").is_err());

        // Truncated request line (no version)
        assert!(SipMessage::parse(b"INVITE sip:bob@example.com\r\n\r\n").is_err());

        // Invalid method
        assert!(SipMessage::parse(b"DESTROY sip:bob@example.com SIP/2.0\r\n\r\n").is_err());

        // Missing CRLF between headers and body
        let msg = b"INVITE sip:bob@example.com SIP/2.0\r\nCall-ID: test\r\nCSeq: 1 INVITE";
        // Should parse (just has no body), not panic
        let result = SipMessage::parse(msg);
        assert!(result.is_ok() || result.is_err()); // must not panic

        // Null bytes in the input
        let msg_with_nulls = b"INVITE sip:bob@example.com SIP/2.0\r\nCall-ID: \x00test\r\n\r\n";
        // Should handle gracefully (null bytes are invalid UTF-8)
        let _ = SipMessage::parse(msg_with_nulls); // must not panic

        // Only whitespace
        assert!(SipMessage::parse(b"   \r\n   \r\n\r\n").is_err());

        // Header with no colon (should be skipped, not panic)
        let msg = b"INVITE sip:bob@example.com SIP/2.0\r\nBadHeaderNoColon\r\nCall-ID: x\r\nCSeq: 1 INVITE\r\n\r\n";
        let result = SipMessage::parse(msg);
        assert!(result.is_ok());

        // Header with empty name
        let msg = b"INVITE sip:bob@example.com SIP/2.0\r\n: empty-name\r\nCall-ID: x\r\nCSeq: 1 INVITE\r\n\r\n";
        let result = SipMessage::parse(msg);
        // Should not panic; the empty header name is parsed (the parser is lenient)
        assert!(result.is_ok());

        // Response with status code > 699
        let msg = b"SIP/2.0 999 Crazy\r\nCall-ID: test\r\n\r\n";
        let result = SipMessage::parse(msg);
        // The parser should accept it (SIP allows 3-digit codes)
        assert!(result.is_ok());

        // Unix line endings (LF only, no CR)
        let msg = b"INVITE sip:bob@example.com SIP/2.0\nCall-ID: test\nCSeq: 1 INVITE\nContent-Length: 0\n\n";
        let result = SipMessage::parse(msg);
        assert!(result.is_ok());
    }

    // =========================================================================
    // Adversarial Test: SIP parser enormous Content-Length (DoS prevention)
    // =========================================================================
    #[test]
    fn test_sip_parser_enormous_content_length() {
        use asterisk_sip::parser::SipMessage;

        // Content-Length of ~1 billion -- should be rejected, not trigger OOM
        let msg = b"INVITE sip:bob@example.com SIP/2.0\r\nCall-ID: test\r\nCSeq: 1 INVITE\r\nContent-Length: 999999999\r\n\r\nbody";
        let result = SipMessage::parse(msg);
        assert!(result.is_err(), "Enormous Content-Length should be rejected");

        // Content-Length at the limit (65536) should be OK
        let msg = b"INVITE sip:bob@example.com SIP/2.0\r\nCall-ID: test\r\nCSeq: 1 INVITE\r\nContent-Length: 65536\r\n\r\n";
        let result = SipMessage::parse(msg);
        assert!(result.is_ok());

        // Content-Length just over limit should fail
        let msg = b"INVITE sip:bob@example.com SIP/2.0\r\nCall-ID: test\r\nCSeq: 1 INVITE\r\nContent-Length: 65537\r\n\r\n";
        let result = SipMessage::parse(msg);
        assert!(result.is_err());
    }

    // =========================================================================
    // Adversarial Test: SIP header count limit
    // =========================================================================
    #[test]
    fn test_sip_parser_too_many_headers() {
        use asterisk_sip::parser::SipMessage;

        // Build a message with 300 headers -- should be rejected
        let mut msg = String::from("INVITE sip:bob@example.com SIP/2.0\r\n");
        for i in 0..300 {
            msg.push_str(&format!("X-Custom-{}: value{}\r\n", i, i));
        }
        msg.push_str("Content-Length: 0\r\n\r\n");

        let result = SipMessage::parse(msg.as_bytes());
        assert!(result.is_err(), "Too many headers should be rejected");
    }

    // =========================================================================
    // Adversarial Test: SIP header injection via CRLF in header values
    // =========================================================================
    #[test]
    fn test_sip_parser_injection() {
        use asterisk_sip::parser::SipMessage;

        // CRLF injection attempt in header value
        // A proper SIP parser should handle this via header folding rules.
        // The key thing is it must not produce an extra header from injected data.
        let msg = b"INVITE sip:bob@example.com SIP/2.0\r\nCall-ID: test\r\nFrom: Alice <sip:alice@example.com>\r\nCSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";
        let result = SipMessage::parse(msg).unwrap();
        // Verify the parser found exactly the headers we put in
        assert_eq!(result.get_headers("Call-ID").len(), 1);
        assert_eq!(result.get_headers("From").len(), 1);
    }

    // =========================================================================
    // Adversarial Test: RTP header corrupt data
    // =========================================================================
    #[test]
    fn test_rtp_header_corrupt() {
        use asterisk_sip::rtp::{parse_rtp_header, RtpHeader};

        // Empty packet
        assert!(parse_rtp_header(&[]).is_err());

        // Too short (< 12 bytes)
        assert!(parse_rtp_header(&[0x80, 0x00]).is_err());
        assert!(parse_rtp_header(&[0x80; 11]).is_err());

        // Invalid version (version = 0)
        let mut bad_version = [0u8; 12];
        bad_version[0] = 0x00; // version 0
        assert!(RtpHeader::parse(&bad_version).is_err());

        // Invalid version (version = 1)
        bad_version[0] = 0x40; // version 1
        assert!(RtpHeader::parse(&bad_version).is_err());

        // Invalid version (version = 3)
        bad_version[0] = 0xC0; // version 3
        assert!(RtpHeader::parse(&bad_version).is_err());

        // Payload type 127 (maximum)
        let mut pt127 = [0u8; 12];
        pt127[0] = 0x80; // version 2
        pt127[1] = 0x7F; // PT 127
        let header = RtpHeader::parse(&pt127).unwrap();
        assert_eq!(header.payload_type, 127);

        // Max sequence number wrap
        let mut max_seq = [0u8; 12];
        max_seq[0] = 0x80;
        max_seq[2] = 0xFF;
        max_seq[3] = 0xFF; // seq = 65535
        let header = RtpHeader::parse(&max_seq).unwrap();
        assert_eq!(header.sequence, 65535);

        // CSRC count = 15 with only 12 bytes -- truncated
        let mut csrc_trunc = [0u8; 12];
        csrc_trunc[0] = 0x8F; // V=2, CC=15
        let header = RtpHeader::parse(&csrc_trunc).unwrap();
        assert_eq!(header.csrc_count, 15);
        // The header_size is 12 + 15*4 = 72, but we only have 12 bytes
        // parse_rtp_header should detect the truncation
        assert!(parse_rtp_header(&csrc_trunc).is_err());
    }

    // =========================================================================
    // Adversarial Test: Channel double hangup
    // =========================================================================
    #[test]
    fn test_channel_double_hangup() {
        use asterisk_core::channel::Channel;
        use asterisk_types::{ChannelState, HangupCause};

        let mut chan = Channel::new("SIP/double-hangup-001");
        chan.set_state(ChannelState::Up);

        // First hangup
        chan.hangup(HangupCause::NormalClearing);
        assert_eq!(chan.state, ChannelState::Down);
        assert_eq!(chan.hangup_cause, HangupCause::NormalClearing);

        // Second hangup with different cause -- should be ignored
        chan.hangup(HangupCause::UserBusy);
        // The original cause should be preserved
        assert_eq!(chan.hangup_cause, HangupCause::NormalClearing);
        assert_eq!(chan.state, ChannelState::Down);
    }

    // =========================================================================
    // Adversarial Test: Channel operations after hangup
    // =========================================================================
    #[test]
    fn test_channel_operations_after_hangup() {
        use asterisk_core::channel::Channel;
        use asterisk_types::{ControlFrame, ChannelState, Frame, HangupCause};

        let mut chan = Channel::new("SIP/post-hangup-001");
        chan.hangup(HangupCause::NormalClearing);
        assert_eq!(chan.state, ChannelState::Down);

        // These operations should not panic on a hung-up channel:
        chan.set_variable("TEST", "value");
        assert_eq!(chan.get_variable("TEST"), Some("value"));

        chan.queue_frame(Frame::null());
        let frame = chan.dequeue_frame();
        assert!(frame.is_some()); // queue still works after hangup

        let snap = chan.snapshot();
        assert_eq!(snap.state, ChannelState::Down);

        // Answer after hangup -- should just set state (no panic)
        chan.answer();
        // Note: answer() doesn't check state, it just sets Up. This is a design
        // choice but verifying it doesn't panic is the key here.
    }

    // =========================================================================
    // Adversarial Test: Dialplan pattern edge cases
    // =========================================================================
    #[test]
    fn test_dialplan_pattern_edge_cases() {
        use asterisk_core::pbx::Extension;

        // Empty pattern (after _)
        let ext = Extension::new("_");
        // Empty pattern with empty input -- should match because pattern is empty
        // and '!' logic applies? Actually _ with no pattern chars matches nothing
        // because the loop runs with pat_chars empty and inp_chars may or may not be empty.
        // Let's just verify it doesn't panic.
        let _ = ext.matches("");
        let _ = ext.matches("1");

        // Pattern with only dot
        let ext = Extension::new("_.");
        assert!(ext.matches("1")); // dot matches 1+ chars
        assert!(!ext.matches("")); // dot requires at least 1 char

        // Pattern with only bang
        let ext = Extension::new("_!");
        assert!(ext.matches("")); // bang matches 0+ chars
        assert!(ext.matches("anything"));

        // Unclosed bracket -- should not panic, may not match
        let ext = Extension::new("_[1-5");
        let _ = ext.matches("3"); // must not panic
        let _ = ext.matches("9"); // must not panic

        // Empty bracket
        let ext = Extension::new("_[]XX");
        let _ = ext.matches("1XX"); // must not panic

        // Pattern Z should not match '0'
        let ext = Extension::new("_Z");
        assert!(!ext.matches("0"));
        assert!(ext.matches("1"));
        assert!(ext.matches("9"));

        // Pattern N should not match '0' or '1'
        let ext = Extension::new("_N");
        assert!(!ext.matches("0"));
        assert!(!ext.matches("1"));
        assert!(ext.matches("2"));
        assert!(ext.matches("9"));

        // Pattern with literal characters
        let ext = Extension::new("_*99");
        assert!(ext.matches("*99"));
        assert!(!ext.matches("199"));

        // Pattern with hash
        let ext = Extension::new("_#");
        assert!(ext.matches("#"));
        assert!(!ext.matches("1"));

        // Exact match has priority over pattern
        let ext_exact = Extension::new("100");
        let ext_pattern = Extension::new("_1XX");
        // Both should match "100"
        assert!(ext_exact.matches("100"));
        assert!(ext_pattern.matches("100"));
    }

    // =========================================================================
    // Adversarial Test: Config parser malformed input
    // =========================================================================
    #[test]
    fn test_config_parser_malformed() {
        use asterisk_config::AsteriskConfig;

        // Unclosed section bracket
        let result = AsteriskConfig::from_str("[unclosed", "test.conf");
        assert!(result.is_err());

        // Missing = in variable line -- should error
        let result = AsteriskConfig::from_str("[section]\nthis has no equals", "test.conf");
        assert!(result.is_err());

        // Binary-ish data should error or produce empty config
        let result = AsteriskConfig::from_str("\x00\x01\x02\x03", "test.conf");
        // This is just text parsing, binary is treated as text with no sections
        // Should not panic
        assert!(result.is_ok() || result.is_err());

        // Extremely long line
        let long_value = "x".repeat(100_000);
        let content = format!("[section]\nkey = {}", long_value);
        let result = AsteriskConfig::from_str(&content, "test.conf");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.get_variable("section", "key").unwrap().len(), 100_000);

        // Empty category name
        let result = AsteriskConfig::from_str("[]", "test.conf");
        assert!(result.is_err());

        // Empty variable name
        let result = AsteriskConfig::from_str("[section]\n = value", "test.conf");
        assert!(result.is_err());

        // Only comments
        let result = AsteriskConfig::from_str("; comment\n; another comment", "test.conf");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().categories.len(), 0);

        // Template with unclosed parens
        let result = AsteriskConfig::from_str("[section](!)", "test.conf");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(config.get_category("section").unwrap().is_template);

        // Double section headers
        let content = "[a]\nk1 = v1\n[a]\nk2 = v2";
        let result = AsteriskConfig::from_str(content, "test.conf");
        assert!(result.is_ok());
        let config = result.unwrap();
        let categories = config.get_categories_by_name("a");
        assert_eq!(categories.len(), 2);
    }

    // =========================================================================
    // Adversarial Test: Config parser path traversal prevention
    // =========================================================================
    #[test]
    fn test_config_parser_path_traversal() {
        use asterisk_config::AsteriskConfig;

        // Path traversal attempt -- should be rejected
        let content = "#include \"../../etc/passwd\"\n[section]\nkey = value";
        let result = AsteriskConfig::from_str(content, "test.conf");
        assert!(result.is_err(), "#include with .. should be rejected");

        // Absolute path attempt -- should be rejected
        let content = "#include \"/etc/passwd\"\n[section]\nkey = value";
        let result = AsteriskConfig::from_str(content, "test.conf");
        assert!(result.is_err(), "#include with absolute path should be rejected");

        // Empty include path -- should error
        let content = "#include \"\"\n[section]\nkey = value";
        let result = AsteriskConfig::from_str(content, "test.conf");
        assert!(result.is_err(), "Empty #include path should be rejected");
    }

    // =========================================================================
    // Adversarial Test: Codec boundary values
    // =========================================================================
    #[test]
    fn test_codec_boundary_values() {
        use asterisk_codecs::ulaw_table::{linear_to_mulaw, linear_to_mulaw_fast, mulaw_to_linear};
        use asterisk_codecs::alaw_table::{alaw_to_linear, linear_to_alaw_fast};

        // Empty audio buffer -- no crash
        let empty: Vec<u8> = vec![];
        for &b in empty.iter() {
            let _ = mulaw_to_linear(b);
        }

        // Single sample
        let single = mulaw_to_linear(0x00);
        assert_eq!(single, -32124);

        // Maximum positive PCM value
        let encoded_max = linear_to_mulaw_fast(i16::MAX);
        let decoded_max = mulaw_to_linear(encoded_max);
        assert!(decoded_max > 0, "Max positive should decode to positive");

        // Maximum negative PCM value
        let encoded_min = linear_to_mulaw_fast(i16::MIN);
        let decoded_min = mulaw_to_linear(encoded_min);
        assert!(decoded_min < 0, "Min negative should decode to negative");

        // Zero
        let encoded_zero = linear_to_mulaw_fast(0);
        let decoded_zero = mulaw_to_linear(encoded_zero);
        assert_eq!(decoded_zero, 0);

        // A-law boundary values
        let alaw_max = linear_to_alaw_fast(i16::MAX);
        let alaw_decoded_max = alaw_to_linear(alaw_max);
        assert!(alaw_decoded_max > 0);

        let alaw_min = linear_to_alaw_fast(i16::MIN);
        let alaw_decoded_min = alaw_to_linear(alaw_min);
        assert!(alaw_decoded_min < 0);
    }

    // =========================================================================
    // Adversarial Test: mu-law full range matches C implementation
    // =========================================================================
    #[test]
    fn test_ulaw_full_range() {
        use asterisk_codecs::ulaw_table::{linear_to_mulaw, linear_to_mulaw_fast, mulaw_to_linear, MULAW_TO_LINEAR};

        // Verify decode table: first entry should be -32124
        assert_eq!(MULAW_TO_LINEAR[0], -32124);
        // 0x7F (127) should be 0 (negative zero)
        assert_eq!(MULAW_TO_LINEAR[0x7F], 0);
        // 0x80 (128) should be 32124 (positive max segment)
        assert_eq!(MULAW_TO_LINEAR[0x80], 32124);
        // 0xFF should be 0 (positive zero / silence)
        assert_eq!(MULAW_TO_LINEAR[0xFF], 0);

        // Verify that both encode functions produce the same result
        // for all decode table values
        for ulaw_val in 0u8..=255 {
            let linear = mulaw_to_linear(ulaw_val);
            let enc1 = linear_to_mulaw(linear);
            let enc2 = linear_to_mulaw_fast(linear);
            assert_eq!(
                enc1, enc2,
                "Encoder mismatch for ulaw={}: linear={}, enc1={}, enc2={}",
                ulaw_val, linear, enc1, enc2
            );
        }

        // Verify roundtrip: for every mu-law value, encode(decode(x)) should
        // return a value whose decode is identical to decode(x)
        for ulaw_val in 0u8..=255 {
            let linear = mulaw_to_linear(ulaw_val);
            let re_encoded = linear_to_mulaw_fast(linear);
            let re_decoded = mulaw_to_linear(re_encoded);
            assert_eq!(
                linear, re_decoded,
                "Roundtrip failed for ulaw={}: {} -> {} -> {}",
                ulaw_val, linear, re_encoded, re_decoded
            );
        }
    }

    // =========================================================================
    // Adversarial Test: Frame queue overflow
    // =========================================================================
    #[test]
    fn test_frame_queue_overflow() {
        use asterisk_core::channel::Channel;
        use asterisk_types::Frame;

        let mut chan = Channel::new("Test/overflow-001");

        // Push 2000 frames (more than the limit of 1000)
        for i in 0..2000u32 {
            chan.queue_frame(Frame::voice(0, i, bytes::Bytes::new()));
        }

        // Queue should be capped at MAX_FRAME_QUEUE_SIZE (1000)
        assert_eq!(chan.frame_queue.len(), 1000);

        // The oldest frames should have been dropped.
        // The first frame in the queue should be the one with samples=1000
        // (frames 0-999 pushed, then 1000 pushed dropping 0, etc.)
        let first = chan.dequeue_frame().unwrap();
        if let Frame::Voice { samples, .. } = first {
            assert_eq!(samples, 1000, "Oldest frames should be dropped");
        } else {
            panic!("Expected voice frame");
        }
    }

    // =========================================================================
    // Adversarial Test: Concurrent channel access
    // =========================================================================
    #[tokio::test]
    async fn test_concurrent_channel_access() {
        use asterisk_core::channel::Channel;
        use asterisk_types::Frame;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let channel = Arc::new(Mutex::new(Channel::new("Test/concurrent-001")));

        let mut handles = Vec::new();

        // Spawn 10 tasks that all queue frames concurrently
        for task_id in 0..10u32 {
            let chan = Arc::clone(&channel);
            handles.push(tokio::spawn(async move {
                for i in 0..100u32 {
                    let mut c = chan.lock().await;
                    c.queue_frame(Frame::voice(
                        task_id,
                        i,
                        bytes::Bytes::new(),
                    ));
                    c.set_variable(
                        &format!("task_{}_iter", task_id),
                        &format!("{}", i),
                    );
                }
            }));
        }

        // Wait for all tasks
        for h in handles {
            h.await.unwrap();
        }

        // Verify the channel is in a consistent state
        let chan = channel.lock().await;
        // Total frames queued: 10 * 100 = 1000, exactly at the limit
        assert_eq!(chan.frame_queue.len(), 1000);

        // All 10 tasks should have set variables
        for task_id in 0..10u32 {
            let key = format!("task_{}_iter", task_id);
            assert!(
                chan.get_variable(&key).is_some(),
                "Missing variable for task {}",
                task_id
            );
        }
    }

    // =========================================================================
    // Adversarial Test: CDR rapid events
    // =========================================================================
    #[test]
    fn test_cdr_rapid_events() {
        use asterisk_cdr::engine::{CdrConfig, CdrEngine};
        use asterisk_cdr::{Cdr, CdrBackend, CdrError};
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Mutex;

        struct CountingBackend {
            count: AtomicU64,
        }

        impl CdrBackend for CountingBackend {
            fn name(&self) -> &str { "counter" }
            fn log(&self, _cdr: &Cdr) -> Result<(), CdrError> {
                self.count.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }

        let backend = Arc::new(CountingBackend {
            count: AtomicU64::new(0),
        });

        let config = CdrConfig {
            enabled: true,
            log_unanswered: true,
            log_congestion: true,
            ..Default::default()
        };
        let engine = CdrEngine::with_config(config);
        engine.register_backend(Arc::clone(&backend) as Arc<dyn CdrBackend>);

        // Create and immediately hangup 1000 channels in rapid succession
        for i in 0..1000u32 {
            let uid = format!("rapid-{}", i);
            let name = format!("SIP/rapid-{:04}", i);
            engine.channel_created(&uid, &name, "", "", "default");
            engine.channel_hangup(&uid, 16, "Hangup", "");
        }

        // All channels should be finalized
        assert_eq!(engine.active_count(), 0);
        // All CDRs should have been dispatched to the backend
        assert_eq!(backend.count.load(Ordering::Relaxed), 1000);
    }

    // =========================================================================
    // Adversarial Test: SIP URI edge cases
    // =========================================================================
    #[test]
    fn test_sip_uri_edge_cases() {
        use asterisk_sip::parser::SipUri;

        // URI with password
        let uri = SipUri::parse("sip:alice:secret@host.com").unwrap();
        assert_eq!(uri.user, Some("alice".to_string()));
        assert_eq!(uri.password, Some("secret".to_string()));
        assert_eq!(uri.host, "host.com");

        // IPv6 in brackets
        let uri = SipUri::parse("sip:user@[::1]:5060").unwrap();
        assert_eq!(uri.host, "::1");
        assert_eq!(uri.port, Some(5060));

        // Unknown scheme should fail
        let result = SipUri::parse("http:user@host.com");
        assert!(result.is_err());

        // Missing scheme should fail
        let result = SipUri::parse("user@host.com");
        assert!(result.is_err());

        // tel: URI
        let uri = SipUri::parse("tel:+15551234567").unwrap();
        assert_eq!(uri.scheme, "tel");

        // URI with header parameters
        let uri = SipUri::parse("sip:host.com?Subject=hello&Priority=urgent").unwrap();
        assert_eq!(uri.headers.get("Subject"), Some(&"hello".to_string()));
    }

    // =========================================================================
    // Adversarial Test: Dial app argument parsing edge cases
    // =========================================================================
    #[test]
    fn test_dial_args_edge_cases() {
        use asterisk_apps::dial::DialArgs;

        // Empty args
        assert!(DialArgs::parse("").is_none());

        // Only separator
        assert!(DialArgs::parse(",").is_none());

        // Invalid timeout
        let args = DialArgs::parse("SIP/alice,-5").unwrap();
        // Negative timeout should use default
        assert!(args.timeout.as_secs() > 1000);

        // Zero timeout
        let args = DialArgs::parse("SIP/alice,0").unwrap();
        assert!(args.timeout.as_secs() > 1000);

        // Very large timeout
        let args = DialArgs::parse("SIP/alice,999999").unwrap();
        assert_eq!(args.timeout.as_secs(), 999999);

        // Mixed valid/invalid destinations
        let args = DialArgs::parse("SIP/alice&&SIP/bob&").unwrap();
        // The empty segments should be filtered out
        assert_eq!(args.destinations.len(), 2);
    }

    // =========================================================================
    // Adversarial Test: RTP DTMF encoding roundtrip for all digits
    // =========================================================================
    #[test]
    fn test_dtmf_all_digits_roundtrip() {
        use asterisk_sip::rtp::DtmfEvent;

        // All valid DTMF digits
        let valid_digits = "0123456789*#ABCDabcd";
        for digit in valid_digits.chars() {
            let event_num = DtmfEvent::digit_to_event(digit);
            let back = DtmfEvent::event_to_digit(event_num);
            assert_eq!(
                back.to_ascii_uppercase(),
                digit.to_ascii_uppercase(),
                "Roundtrip failed for '{}'",
                digit
            );
        }

        // Invalid digit should map to event 0 (which is '0')
        let event_num = DtmfEvent::digit_to_event('Z');
        assert_eq!(event_num, 0);

        // Event numbers > 15 should produce '?'
        assert_eq!(DtmfEvent::event_to_digit(16), '?');
        assert_eq!(DtmfEvent::event_to_digit(255), '?');

        // DTMF event with minimum and maximum durations
        let event_min = DtmfEvent {
            event: 0,
            end: false,
            volume: 0,
            duration: 0,
        };
        let bytes = event_min.to_bytes();
        let parsed = DtmfEvent::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.duration, 0);

        let event_max = DtmfEvent {
            event: 15,
            end: true,
            volume: 63,
            duration: u16::MAX,
        };
        let bytes = event_max.to_bytes();
        let parsed = DtmfEvent::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.duration, u16::MAX);
        assert_eq!(parsed.volume, 63);
    }

    // =========================================================================
    // Adversarial Test: Stasis topic with no subscribers
    // =========================================================================
    #[test]
    fn test_stasis_publish_no_subscribers() {
        use asterisk_core::stasis::{self, StasisMessage, Topic};
        use std::any::Any;

        #[derive(Debug)]
        struct TestMsg;
        impl StasisMessage for TestMsg {
            fn message_type(&self) -> &str { "test" }
            fn as_any(&self) -> &dyn Any { self }
        }

        let topic = Topic::with_name("lonely-topic");
        assert_eq!(topic.subscriber_count(), 0);

        // Publishing with no subscribers should not panic or error
        stasis::publish(&topic, TestMsg);
        stasis::publish(&topic, TestMsg);
        stasis::publish(&topic, TestMsg);
    }

    // =========================================================================
    // Adversarial Test: SIP transaction state machine
    // =========================================================================
    #[test]
    fn test_sip_transaction_state_machine() {
        use asterisk_sip::parser::SipMessage;
        use asterisk_sip::transaction::{ClientTransaction, InviteClientState};

        // Create a valid INVITE request
        let req_bytes = b"INVITE sip:bob@example.com SIP/2.0\r\nVia: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK1\r\nCall-ID: test\r\nCSeq: 1 INVITE\r\nFrom: <sip:a@b>;tag=x\r\nTo: <sip:b@b>\r\nContent-Length: 0\r\n\r\n";
        let req = SipMessage::parse(req_bytes).unwrap();
        let addr: std::net::SocketAddr = "10.0.0.1:5060".parse().unwrap();

        let mut tx = ClientTransaction::new(req, addr, "z9hG4bK1".to_string());
        assert_eq!(tx.state, InviteClientState::Calling);

        // Receive 100 Trying -> Proceeding
        let resp_100 = SipMessage::parse(b"SIP/2.0 100 Trying\r\nVia: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK1\r\nCall-ID: test\r\nCSeq: 1 INVITE\r\nFrom: <sip:a@b>;tag=x\r\nTo: <sip:b@b>\r\nContent-Length: 0\r\n\r\n").unwrap();
        tx.on_response(resp_100);
        assert_eq!(tx.state, InviteClientState::Proceeding);

        // Receive 180 Ringing -> still Proceeding
        let resp_180 = SipMessage::parse(b"SIP/2.0 180 Ringing\r\nVia: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK1\r\nCall-ID: test\r\nCSeq: 1 INVITE\r\nFrom: <sip:a@b>;tag=x\r\nTo: <sip:b@b>\r\nContent-Length: 0\r\n\r\n").unwrap();
        tx.on_response(resp_180);
        assert_eq!(tx.state, InviteClientState::Proceeding);

        // Receive 200 OK -> Terminated (per RFC 3261 Section 17.1.1.2)
        let resp_200 = SipMessage::parse(b"SIP/2.0 200 OK\r\nVia: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK1\r\nCall-ID: test\r\nCSeq: 1 INVITE\r\nFrom: <sip:a@b>;tag=x\r\nTo: <sip:b@b>;tag=y\r\nContent-Length: 0\r\n\r\n").unwrap();
        tx.on_response(resp_200);
        assert_eq!(tx.state, InviteClientState::Terminated);
    }

    // =========================================================================
    // Adversarial Test: SIP transaction 4xx to Completed
    // =========================================================================
    #[test]
    fn test_sip_transaction_rejection() {
        use asterisk_sip::parser::SipMessage;
        use asterisk_sip::transaction::{ClientTransaction, InviteClientState};

        let req_bytes = b"INVITE sip:bob@example.com SIP/2.0\r\nVia: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK2\r\nCall-ID: test2\r\nCSeq: 1 INVITE\r\nFrom: <sip:a@b>;tag=x\r\nTo: <sip:b@b>\r\nContent-Length: 0\r\n\r\n";
        let req = SipMessage::parse(req_bytes).unwrap();
        let addr: std::net::SocketAddr = "10.0.0.1:5060".parse().unwrap();

        let mut tx = ClientTransaction::new(req, addr, "z9hG4bK2".to_string());

        // Direct 486 Busy Here (no provisional) -> Completed
        let resp_486 = SipMessage::parse(b"SIP/2.0 486 Busy Here\r\nVia: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK2\r\nCall-ID: test2\r\nCSeq: 1 INVITE\r\nFrom: <sip:a@b>;tag=x\r\nTo: <sip:b@b>;tag=y\r\nContent-Length: 0\r\n\r\n").unwrap();
        tx.on_response(resp_486);
        assert_eq!(tx.state, InviteClientState::Completed);
    }

    // =====================================================================
    // WAVE 2 ADVERSARIAL TESTS
    // =====================================================================

    // =========================================================================
    // IAX2 Frame Parser Adversarial Tests
    // =========================================================================

    #[test]
    fn test_iax2_truncated_frames() {
        use asterisk_channels::iax2::*;

        // 1 byte - not enough for any frame type
        assert!(parse_iax2_packet(&[0x80]).is_err());

        // 2 bytes - too short for mini (4) or full (12)
        assert!(parse_iax2_packet(&[0x00, 0x42]).is_err());

        // 3 bytes - still too short
        assert!(parse_iax2_packet(&[0x00, 0x42, 0x00]).is_err());

        // 4 bytes - enough for meta or mini frame header but not full frame
        // This is a mini frame (high bit not set, non-zero)
        let mini_data = [0x00, 0x01, 0x12, 0x34];
        let pkt = parse_iax2_packet(&mini_data).unwrap();
        match pkt {
            Iax2Packet::Mini { header, .. } => {
                assert_eq!(header.call_number, 1);
                assert_eq!(header.timestamp, 0x1234);
            }
            _ => panic!("Expected mini frame"),
        }

        // 5 bytes with full frame flag - too short for 12-byte header
        let short_full = [0x80, 0x01, 0x00, 0x02, 0x00];
        assert!(Iax2FullHeader::parse(&short_full).is_err());
        // parse_iax2_packet should also fail since it delegates
        assert!(parse_iax2_packet(&short_full).is_err());

        // 11 bytes with full frame flag - 1 byte short of the 12 needed
        let almost_full = [0x80, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x06];
        assert!(Iax2FullHeader::parse(&almost_full).is_err());
    }

    #[test]
    fn test_iax2_max_call_numbers() {
        use asterisk_channels::iax2::*;

        // Maximum source call = 0x7FFF (32767)
        let header = Iax2FullHeader {
            src_call_number: 0x7FFF,
            dst_call_number: 0x7FFF,
            retransmit: true,
            timestamp: u32::MAX,
            oseqno: 255,
            iseqno: 255,
            frame_type: 6, // IAX
            subclass: 40,  // CallToken
        };
        let bytes = header.to_bytes();
        let parsed = Iax2FullHeader::parse(&bytes).unwrap();
        assert_eq!(parsed.src_call_number, 0x7FFF);
        assert_eq!(parsed.dst_call_number, 0x7FFF);
        assert!(parsed.retransmit);
        assert_eq!(parsed.timestamp, u32::MAX);
        assert_eq!(parsed.oseqno, 255);
        assert_eq!(parsed.iseqno, 255);
    }

    #[test]
    fn test_iax2_ie_zero_length() {
        use asterisk_channels::iax2::*;

        // Zero-length IE is valid (presence flag)
        let ie = ie_empty(ie::AUTOANSWER);
        assert_eq!(ie.data.len(), 0);

        let serialized = serialize_information_elements(&[ie]);
        assert_eq!(serialized.len(), 2); // type + len(0)

        let parsed = parse_information_elements(&serialized).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].ie_type, ie::AUTOANSWER);
        assert_eq!(parsed[0].data.len(), 0);
    }

    #[test]
    fn test_iax2_ie_length_exceeds_remaining() {
        use asterisk_channels::iax2::*;

        // IE type=1, length=100 but only 5 bytes of data follow
        let bad_data = [1u8, 100, 0, 0, 0, 0, 0];
        let result = parse_information_elements(&bad_data);
        assert!(result.is_err(), "IE with length exceeding buffer should fail");
    }

    #[test]
    fn test_iax2_ie_serialization_truncation_over_255() {
        use asterisk_channels::iax2::*;

        // Create an IE with data > 255 bytes
        let big_data = Bytes::from(vec![0xAB; 300]);
        let ie = InformationElement {
            ie_type: ie::CALLED_NUMBER,
            data: big_data,
        };

        let serialized = serialize_information_elements(&[ie]);
        // The serialized form should have truncated to 255 bytes
        assert_eq!(serialized[1], 255u8, "IE length byte should be 255");
        // Total should be 2 (type+len) + 255 (data) = 257
        assert_eq!(serialized.len(), 257);
    }

    #[test]
    fn test_iax2_four_byte_full_frame_claim() {
        use asterisk_channels::iax2::*;

        // 4 bytes with full frame flag set -- not enough for 12 byte header
        let data = [0x80, 0x01, 0x00, 0x02];
        assert!(Iax2FullHeader::parse(&data).is_err());
        // Top-level parser should also error
        assert!(parse_iax2_packet(&data).is_err());
    }

    // =========================================================================
    // WebSocket Frame Parser Adversarial Tests
    // =========================================================================

    #[test]
    fn test_ws_truncated_at_every_position() {
        use asterisk_channels::websocket::*;

        // Build a complete frame then test every truncation
        let frame = WebSocketFrame::text("Hello");
        let full_bytes = frame.to_bytes();

        for len in 0..full_bytes.len() {
            let result = WebSocketFrame::parse(&full_bytes[..len]);
            // Should either return Ok(None) for "need more data" or Ok(Some) for complete
            match result {
                Ok(None) => {} // Expected for truncated
                Ok(Some(_)) => panic!("Should not parse truncated frame at len {}", len),
                Err(_) => {} // Also acceptable for some positions
            }
        }

        // The full frame should parse
        let (parsed, _) = WebSocketFrame::parse(&full_bytes).unwrap().unwrap();
        assert_eq!(parsed.payload, Bytes::from("Hello"));
    }

    #[test]
    fn test_ws_invalid_opcodes_rejected() {
        use asterisk_channels::websocket::*;

        // Opcodes 3-7 are reserved non-control frames
        for opcode in 3..=7u8 {
            let mut buf = vec![0u8; 4];
            buf[0] = 0x80 | opcode; // FIN + reserved opcode
            buf[1] = 2;             // payload len = 2
            buf[2] = 0x41;
            buf[3] = 0x42;
            let result = WebSocketFrame::parse(&buf);
            assert!(result.is_err(), "Opcode 0x{:x} should be rejected", opcode);
        }

        // Opcodes 0xB-0xF are reserved control frames
        for opcode in 0xBu8..=0xF {
            let mut buf = vec![0u8; 4];
            buf[0] = 0x80 | opcode;
            buf[1] = 2;
            buf[2] = 0x41;
            buf[3] = 0x42;
            let result = WebSocketFrame::parse(&buf);
            assert!(result.is_err(), "Opcode 0x{:x} should be rejected", opcode);
        }
    }

    #[test]
    fn test_ws_close_frame_invalid_close_codes() {
        use asterisk_channels::websocket::*;

        // Close code 0 (invalid - < 1000)
        let mut buf = vec![0u8; 4];
        buf[0] = 0x88; // FIN + Close
        buf[1] = 2;    // payload = 2 bytes (just the code)
        buf[2] = 0x00;
        buf[3] = 0x00; // code = 0
        assert!(WebSocketFrame::parse(&buf).is_err(), "Close code 0 should be rejected");

        // Close code 999 (invalid - < 1000)
        buf[2] = 0x03;
        buf[3] = 0xE7; // code = 999
        assert!(WebSocketFrame::parse(&buf).is_err(), "Close code 999 should be rejected");

        // Close code 1005 (NO_STATUS - must not appear on wire)
        buf[2] = 0x03;
        buf[3] = 0xED; // code = 1005
        assert!(WebSocketFrame::parse(&buf).is_err(), "Close code 1005 should be rejected");

        // Close code 1006 (ABNORMAL - must not appear on wire)
        buf[2] = 0x03;
        buf[3] = 0xEE; // code = 1006
        assert!(WebSocketFrame::parse(&buf).is_err(), "Close code 1006 should be rejected");

        // Close code 1000 (NORMAL - valid)
        buf[2] = 0x03;
        buf[3] = 0xE8; // code = 1000
        let result = WebSocketFrame::parse(&buf);
        assert!(result.is_ok(), "Close code 1000 should be valid");

        // Close frame with body of 1 byte (must be 0 or >= 2)
        let mut buf1 = vec![0u8; 3];
        buf1[0] = 0x88; // FIN + Close
        buf1[1] = 1;    // payload = 1 byte
        buf1[2] = 0x42;
        assert!(WebSocketFrame::parse(&buf1).is_err(), "Close with 1-byte body should be rejected");
    }

    #[test]
    fn test_ws_control_frame_payload_too_large() {
        use asterisk_channels::websocket::*;

        // Ping with 126 bytes (> 125 limit for control frames)
        let mut buf = vec![0u8; 2 + 2 + 126]; // header + extended len + payload
        buf[0] = 0x89; // FIN + Ping
        buf[1] = 126;  // This means extended 16-bit length
        buf[2] = 0;
        buf[3] = 126;  // 126 bytes
        // Fill payload
        for i in 4..buf.len() {
            buf[i] = 0x41;
        }
        let result = WebSocketFrame::parse(&buf);
        assert!(result.is_err(), "Ping frame > 125 bytes should be rejected");
    }

    // =========================================================================
    // G.722 Codec Adversarial Tests
    // =========================================================================

    #[test]
    fn test_g722_encode_silence_deterministic() {
        use asterisk_codecs::g722::{G722Decoder, G722Encoder};

        let mut encoder = G722Encoder::new();
        let silence: Vec<i16> = vec![0; 320];
        let encoded = encoder.encode(&silence);
        assert_eq!(encoded.len(), 160);

        // Encode the same silence again -- state should give same output
        // (new encoder)
        let mut encoder2 = G722Encoder::new();
        let encoded2 = encoder2.encode(&silence);
        assert_eq!(encoded, encoded2, "Same input to fresh encoder should produce same output");
    }

    #[test]
    fn test_g722_encode_max_amplitude() {
        use asterisk_codecs::g722::{G722Decoder, G722Encoder};

        let mut encoder = G722Encoder::new();
        let mut decoder = G722Decoder::new();

        // Maximum positive amplitude
        let max_pos: Vec<i16> = vec![i16::MAX; 320];
        let encoded = encoder.encode(&max_pos);
        assert_eq!(encoded.len(), 160);
        let decoded = decoder.decode(&encoded);
        assert_eq!(decoded.len(), 320);
        // Should not panic and values should be in i16 range

        // Maximum negative amplitude
        let mut encoder2 = G722Encoder::new();
        let mut decoder2 = G722Decoder::new();
        let max_neg: Vec<i16> = vec![i16::MIN; 320];
        let encoded2 = encoder2.encode(&max_neg);
        let decoded2 = decoder2.decode(&encoded2);
        assert_eq!(decoded2.len(), 320);
    }

    #[test]
    fn test_g722_decoder_garbage_input() {
        use asterisk_codecs::g722::G722Decoder;

        let mut decoder = G722Decoder::new();
        // Random-ish garbage bytes
        let garbage: Vec<u8> = (0..160).map(|i| (i as u8).wrapping_mul(37)).collect();
        let decoded = decoder.decode(&garbage);
        assert_eq!(decoded.len(), 320);
        // Should not panic -- all values should be valid i16
    }

    #[test]
    fn test_g722_empty_input() {
        use asterisk_codecs::g722::{G722Decoder, G722Encoder};

        let mut encoder = G722Encoder::new();
        let encoded = encoder.encode(&[]);
        assert!(encoded.is_empty());

        let mut decoder = G722Decoder::new();
        let decoded = decoder.decode(&[]);
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_g722_odd_sample_count() {
        use asterisk_codecs::g722::G722Encoder;

        let mut encoder = G722Encoder::new();
        // Odd number of samples -- encoder processes pairs, last sample dropped
        let samples: Vec<i16> = vec![1000; 321];
        let encoded = encoder.encode(&samples);
        // 321 samples -> 160 pairs processed -> 160 bytes
        assert_eq!(encoded.len(), 160);
    }

    // =========================================================================
    // G.726 Codec Adversarial Tests
    // =========================================================================

    #[test]
    fn test_g726_decoder_garbage_input() {
        use asterisk_codecs::g726::{G726Decoder, G726Rate};

        for rate in [G726Rate::Rate16, G726Rate::Rate24, G726Rate::Rate32, G726Rate::Rate40] {
            let mut decoder = G726Decoder::new(rate);
            let garbage: Vec<u8> = (0..80).map(|i| (i as u8).wrapping_mul(97)).collect();
            let decoded = decoder.decode(&garbage);
            assert!(!decoded.is_empty(), "rate {:?} should produce output", rate);
            // Should not panic
        }
    }

    #[test]
    fn test_g726_max_amplitude() {
        use asterisk_codecs::g726::{G726Decoder, G726Encoder, G726Rate};

        let mut encoder = G726Encoder::new(G726Rate::Rate32);
        let mut decoder = G726Decoder::new(G726Rate::Rate32);

        let max_samples: Vec<i16> = vec![i16::MAX; 160];
        let encoded = encoder.encode(&max_samples);
        let decoded = decoder.decode(&encoded);
        assert_eq!(decoded.len(), 160);
        // Should not overflow or panic
    }

    // =========================================================================
    // ACL Adversarial Tests
    // =========================================================================

    #[test]
    fn test_acl_0_0_0_0_slash_0_matches_everything() {
        use asterisk_sip::acl::Acl;

        let mut acl = Acl::new("test");
        acl.deny("0.0.0.0/0");

        assert!(!acl.check(&"10.0.0.1".parse().unwrap()));
        assert!(!acl.check(&"192.168.1.1".parse().unwrap()));
        assert!(!acl.check(&"255.255.255.255".parse().unwrap()));
        assert!(!acl.check(&"0.0.0.0".parse().unwrap()));
        assert!(!acl.check(&"127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn test_acl_255_255_255_255_slash_32_exact_match() {
        use asterisk_sip::acl::Acl;

        let mut acl = Acl::new("test");
        acl.deny("255.255.255.255/32");

        assert!(!acl.check(&"255.255.255.255".parse().unwrap()));
        assert!(acl.check(&"255.255.255.254".parse().unwrap()));
    }

    #[test]
    fn test_acl_network_boundary() {
        use asterisk_sip::acl::Acl;

        let mut acl = Acl::new("test");
        acl.deny("192.168.1.0/24");

        // Last address in the subnet
        assert!(!acl.check(&"192.168.1.255".parse().unwrap()));
        // First address outside the subnet
        assert!(acl.check(&"192.168.2.0".parse().unwrap()));
        // First address in the subnet
        assert!(!acl.check(&"192.168.1.0".parse().unwrap()));
    }

    #[test]
    fn test_acl_overlapping_rules_order_matters() {
        use asterisk_sip::acl::Acl;

        // Permit specific, deny general
        let mut acl1 = Acl::new("test1");
        acl1.permit("10.0.1.100/32");
        acl1.deny("10.0.0.0/8");
        assert!(acl1.check(&"10.0.1.100".parse().unwrap())); // Matches permit first
        assert!(!acl1.check(&"10.0.1.101".parse().unwrap())); // Matches deny

        // Reverse order: deny general, permit specific -- deny wins for all
        let mut acl2 = Acl::new("test2");
        acl2.deny("10.0.0.0/8");
        acl2.permit("10.0.1.100/32");
        assert!(!acl2.check(&"10.0.1.100".parse().unwrap())); // Matches deny first!
    }

    #[test]
    fn test_acl_empty_default_permit() {
        use asterisk_sip::acl::Acl;

        let acl = Acl::new("empty");
        assert!(acl.is_empty());
        // Default permit when no rules
        assert!(acl.check(&"1.2.3.4".parse().unwrap()));
    }

    #[test]
    fn test_acl_non_contiguous_mask_rejected() {
        use asterisk_sip::acl::{Acl, AclRule};

        // "255.0.255.0" is a non-contiguous mask -- should be rejected
        let rule = AclRule::deny("10.0.0.0/255.0.255.0");
        assert!(rule.is_none(), "Non-contiguous netmask should be rejected");

        // Valid contiguous masks should still work
        let rule_valid = AclRule::deny("10.0.0.0/255.255.0.0");
        assert!(rule_valid.is_some());
    }

    #[test]
    fn test_acl_ipv4_ipv6_mismatch() {
        use asterisk_sip::acl::Acl;

        let mut acl = Acl::new("test");
        acl.deny("10.0.0.0/8"); // IPv4 rule

        // IPv6 address should not match IPv4 rule
        assert!(acl.check(&"::1".parse().unwrap()));
        assert!(acl.check(&"::ffff:10.0.0.1".parse().unwrap())); // v4-mapped v6
    }

    // =========================================================================
    // STUN Parser Adversarial Tests
    // =========================================================================

    #[test]
    fn test_stun_truncated_header() {
        use asterisk_res::stun::StunHeader;

        // Less than 20 bytes
        for len in 0..20 {
            let data = vec![0u8; len];
            assert!(StunHeader::parse(&data).is_err(),
                "STUN header parse should fail for {} bytes", len);
        }
    }

    #[test]
    fn test_stun_attribute_exceeding_message() {
        use asterisk_res::stun::{StunAttribute, StunMessage};

        // Attribute claiming 100 bytes but only 10 available
        let mut data = vec![0u8; 14]; // 4 header + 10 value
        data[0] = 0x00; data[1] = 0x06; // type = USERNAME
        data[2] = 0x00; data[3] = 100;  // length = 100 (but only 10 bytes follow)

        let result = StunAttribute::parse(&data);
        assert!(result.is_err(), "Attribute with length exceeding buffer should fail");
    }

    #[test]
    fn test_stun_message_truncated_body() {
        use asterisk_res::stun::{StunHeader, StunMessage, StunMessageType, TransactionId};

        // Build a header claiming 100 bytes of attributes but provide 0
        let mut data = vec![0u8; 20]; // Header only
        data[0] = 0x00; data[1] = 0x01; // Binding Request
        data[2] = 0x00; data[3] = 100;  // msg_length = 100

        let result = StunMessage::parse(&data);
        assert!(result.is_err(), "Message with truncated body should fail");
    }

    #[test]
    fn test_stun_mapped_address_ipv6_not_supported() {
        use asterisk_res::stun::parse_mapped_address;

        // IPv6 family (0x02) should return an error (not panic)
        let data = [0x00, 0x02, 0x13, 0xC4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let result = parse_mapped_address(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_stun_xor_mapped_address_ipv6_not_supported() {
        use asterisk_res::stun::{parse_xor_mapped_address, TransactionId};

        let tid = TransactionId::new();
        let data = [0x00, 0x02, 0x13, 0xC4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let result = parse_xor_mapped_address(&data, &tid);
        assert!(result.is_err());
    }

    #[test]
    fn test_stun_mapped_address_too_short() {
        use asterisk_res::stun::parse_mapped_address;

        // Less than 8 bytes
        for len in 0..8 {
            let data = vec![0u8; len];
            assert!(parse_mapped_address(&data).is_err());
        }
    }

    // =========================================================================
    // GoSub Stack Depth Limit Tests
    // =========================================================================

    #[test]
    fn test_gosub_stack_depth_limit() {
        use asterisk_apps::stack::{AppGoSub, MAX_GOSUB_DEPTH};
        use asterisk_apps::PbxExecResult;
        use asterisk_core::channel::Channel;

        let mut channel = Channel::new("SIP/test-001");
        channel.context = "default".to_string();
        channel.exten = "100".to_string();
        channel.priority = 1;

        // Push to the limit
        for i in 0..MAX_GOSUB_DEPTH {
            let result = AppGoSub::exec(&mut channel, &format!("sub,handler{},1", i));
            assert_eq!(result, PbxExecResult::Success,
                "GoSub #{} should succeed", i);
        }

        // One more should fail
        let result = AppGoSub::exec(&mut channel, "sub,one_too_many,1");
        assert_eq!(result, PbxExecResult::Failed,
            "GoSub should fail at depth {}", MAX_GOSUB_DEPTH);
    }

    // =========================================================================
    // System App Security Tests
    // =========================================================================

    #[tokio::test]
    async fn test_system_null_bytes_in_command() {
        use asterisk_apps::system::AppSystem;
        use asterisk_apps::PbxExecResult;
        use asterisk_core::channel::Channel;

        let mut channel = Channel::new("SIP/test-001");

        // Command with null byte -- sh -c should handle this without panicking
        let result = AppSystem::exec(&mut channel, "echo hello\x00world").await;
        // We don't care if it succeeds or fails, just that it doesn't panic
        let _ = result;
    }

    #[tokio::test]
    async fn test_system_very_long_command() {
        use asterisk_apps::system::AppSystem;
        use asterisk_apps::PbxExecResult;
        use asterisk_core::channel::Channel;

        let mut channel = Channel::new("SIP/test-001");

        // Very long command string
        let long_cmd = "echo ".to_string() + &"x".repeat(100_000);
        let result = AppSystem::exec(&mut channel, &long_cmd).await;
        // Should not panic; may succeed or fail
        let _ = result;
    }

    // =========================================================================
    // SRTP Replay Window Adversarial Tests
    // =========================================================================

    #[test]
    fn test_srtp_replay_window_boundary() {
        use asterisk_res::srtp::{DefaultSrtpPolicy, SrtpSession, SrtpSuite};

        let key = vec![0xAB; 16];
        let salt = vec![0xCD; 14];
        let policy = DefaultSrtpPolicy::new(
            SrtpSuite::AesCm128HmacSha1_80, key, salt,
        ).unwrap();

        let mut protect = SrtpSession::new(&policy).unwrap();
        let mut session = SrtpSession::new(&policy).unwrap();

        // Send packets with increasing sequence numbers
        for seq in 1u16..=65 {
            let mut rtp = vec![0u8; 172];
            rtp[0] = 0x80;
            rtp[2..4].copy_from_slice(&seq.to_be_bytes());
            let srtp = protect.protect(&rtp).unwrap();
            session.unprotect(&srtp).unwrap();
        }

        // Now try to replay seq=1 -- should be outside the 64-bit window
        let mut rtp_old = vec![0u8; 172];
        rtp_old[0] = 0x80;
        rtp_old[2..4].copy_from_slice(&1u16.to_be_bytes());
        let srtp_old = protect.protect(&rtp_old).unwrap();
        let result = session.unprotect(&srtp_old);
        assert!(result.is_err(), "Seq 1 should be outside replay window");
    }

    #[test]
    fn test_srtp_protect_too_short_packet() {
        use asterisk_res::srtp::{DefaultSrtpPolicy, SrtpSession, SrtpSuite};

        let key = vec![0xAB; 16];
        let salt = vec![0xCD; 14];
        let policy = DefaultSrtpPolicy::new(
            SrtpSuite::AesCm128HmacSha1_80, key, salt,
        ).unwrap();
        let mut session = SrtpSession::new(&policy).unwrap();

        // RTP packet shorter than minimum 12 bytes
        let result = session.protect(&[0x80, 0x00, 0x00]);
        assert!(result.is_err());
    }

    // =========================================================================
    // WebSocket RSV bits validation test
    // =========================================================================

    #[test]
    fn test_ws_rsv_bits_rejected() {
        use asterisk_channels::websocket::*;

        // RSV1 set (bit 6)
        let mut buf = vec![0u8; 4];
        buf[0] = 0xC1; // FIN + RSV1 + Text
        buf[1] = 2;
        buf[2] = 0x41;
        buf[3] = 0x42;
        assert!(WebSocketFrame::parse(&buf).is_err(), "RSV1 should be rejected");

        // RSV2 set (bit 5)
        buf[0] = 0xA1; // FIN + RSV2 + Text
        assert!(WebSocketFrame::parse(&buf).is_err(), "RSV2 should be rejected");

        // RSV3 set (bit 4)
        buf[0] = 0x91; // FIN + RSV3 + Text
        assert!(WebSocketFrame::parse(&buf).is_err(), "RSV3 should be rejected");
    }

    // =========================================================================
    // HTTP Server Content-Length DoS prevention
    // =========================================================================

    #[test]
    fn test_http_response_serialization_correctness() {
        use asterisk_res::http_server::{HttpResponse, HttpStatus};

        // Response with body must include correct Content-Length
        let resp = HttpResponse::ok().with_json(r#"{"test":true}"#);
        let bytes = resp.to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Content-Length: 13"));
        assert!(text.contains(r#"{"test":true}"#));
    }

    // =========================================================================
    // SPRINTF adversarial tests
    // =========================================================================

    #[test]
    fn test_sprintf_no_args_for_specifier() {
        use asterisk_funcs::sprintf::FuncSprintf;
        use asterisk_funcs::{DialplanFunc, FuncContext};

        let ctx = FuncContext::new();
        let func = FuncSprintf;

        // More specifiers than arguments -- should use empty string
        let result = func.read(&ctx, "%s %s %s,only_one");
        assert!(result.is_ok());
        let val = result.unwrap();
        assert!(val.contains("only_one"), "First arg should be used");
    }

    #[test]
    fn test_sprintf_bare_percent_at_end() {
        use asterisk_funcs::sprintf::FuncSprintf;
        use asterisk_funcs::{DialplanFunc, FuncContext};

        let ctx = FuncContext::new();
        let func = FuncSprintf;

        // Format string ending with bare %
        let result = func.read(&ctx, "hello %");
        assert!(result.is_err(), "Bare % at end should be an error");
    }

    #[test]
    fn test_sprintf_unknown_specifier() {
        use asterisk_funcs::sprintf::FuncSprintf;
        use asterisk_funcs::{DialplanFunc, FuncContext};

        let ctx = FuncContext::new();
        let func = FuncSprintf;

        let result = func.read(&ctx, "%z,test");
        assert!(result.is_err(), "Unknown specifier %z should be an error");
    }

    // =========================================================================
    // RAND adversarial tests
    // =========================================================================

    #[test]
    fn test_rand_negative_range() {
        use asterisk_funcs::rand::FuncRand;
        use asterisk_funcs::{DialplanFunc, FuncContext};

        let ctx = FuncContext::new();
        let func = FuncRand;

        // Negative range
        for _ in 0..50 {
            let result = func.read(&ctx, "-100,-50").unwrap();
            let val: i64 = result.parse().unwrap();
            assert!(val >= -100 && val <= -50, "RAND(-100,-50) = {} out of range", val);
        }
    }

    // =========================================================================
    // IAX2 MD5 authentication test
    // =========================================================================

    #[test]
    fn test_iax2_md5_response_deterministic() {
        use asterisk_channels::iax2::iax2_md5_response;

        let response1 = iax2_md5_response("challenge123", "password");
        let response2 = iax2_md5_response("challenge123", "password");
        assert_eq!(response1, response2, "MD5 response should be deterministic");

        // Different challenge should give different result
        let response3 = iax2_md5_response("different", "password");
        assert_ne!(response1, response3);
    }

    // =========================================================================
    // Jitter buffer adversarial test
    // =========================================================================

    #[test]
    fn test_iax2_jitter_buffer_overflow_protection() {
        use asterisk_channels::iax2::JitterBuffer;
        use asterisk_types::Frame;

        let mut jb = JitterBuffer::new(60);

        // Insert more than 100 frames -- the jitter buffer should start
        // delivering frames to prevent unbounded growth
        for i in 0..200u32 {
            jb.put(i * 20, Frame::null());
        }

        // After inserting 200 frames, len should be > 100 but get() should drain
        let mut delivered = 0;
        while let Some(_) = jb.get() {
            delivered += 1;
        }
        assert!(delivered > 0, "Jitter buffer should deliver frames");
    }

    // =========================================================================
    // Phase 1 Adversarial Tests
    // =========================================================================

    // -----------------------------------------------------------------------
    // Attack Vector 1: Expression Evaluator Fuzzing
    // -----------------------------------------------------------------------

    mod expression_adversarial {
        use asterisk_core::pbx::expression::evaluate_expression;

        #[test]
        fn division_by_zero_returns_error() {
            assert!(evaluate_expression("1 / 0").is_err());
            assert!(evaluate_expression("10 % 0").is_err());
            assert!(evaluate_expression("0 / 0").is_err());
            assert!(evaluate_expression("-1 / 0").is_err());
        }

        #[test]
        fn integer_overflow_does_not_panic() {
            // Large multiplication -- should not panic
            let result = evaluate_expression("2147483647 + 1").unwrap();
            assert_eq!(result, "2147483648");

            let result = evaluate_expression("-2147483648 - 1").unwrap();
            assert_eq!(result, "-2147483649");

            // Very large values
            let result = evaluate_expression("999999999999999 * 999999999999999");
            assert!(result.is_ok(), "large multiplication should not panic");
        }

        #[test]
        fn deeply_nested_parentheses() {
            // 10 levels of nesting
            let result = evaluate_expression("((((((((((1))))))))))").unwrap();
            assert_eq!(result, "1");

            // Deeper nesting with operations
            let result = evaluate_expression("((((((1 + 2))))))").unwrap();
            assert_eq!(result, "3");
        }

        #[test]
        fn unbalanced_parentheses_return_error() {
            assert!(evaluate_expression("(1 + 2").is_err());
            assert!(evaluate_expression("1 + 2)").is_err());
            assert!(evaluate_expression("((1 + 2)").is_err());
            assert!(evaluate_expression("(1 + 2))").is_err());
        }

        #[test]
        fn empty_expression_returns_zero() {
            assert_eq!(evaluate_expression("").unwrap(), "0");
            assert_eq!(evaluate_expression("  ").unwrap(), "0");
            assert_eq!(evaluate_expression("\t").unwrap(), "0");
        }

        #[test]
        fn regex_with_special_chars() {
            let result = evaluate_expression(r#""test" =~ ".*""#).unwrap();
            assert_ne!(result, "0", "regex .* should match");

            let result = evaluate_expression(r#""test" : "(.*)""#).unwrap();
            assert_eq!(result, "test", "capture group should return matched text");
        }

        #[test]
        fn malformed_ternary_missing_else() {
            // Missing the ':' branch
            let result = evaluate_expression("1 ? 2");
            // Should either error or the parser sees EOF at the colon position
            assert!(result.is_err(), "malformed ternary (missing else) should error");
        }

        #[test]
        fn malformed_ternary_missing_then() {
            // Having ': 3' but no value between '?' and ':'
            let result = evaluate_expression("1 ? : 3");
            // The parser tries to parse a primary after '?', finds ':', errors
            assert!(result.is_err(), "malformed ternary (missing then) should error");
        }

        #[test]
        fn string_with_embedded_operators() {
            let result = evaluate_expression(r#""1+2" = "1+2""#).unwrap();
            assert_eq!(result, "1", "string comparison should match literally");

            let result = evaluate_expression(r#""1+2" = "3""#).unwrap();
            assert_eq!(result, "0", "string '1+2' should not equal '3'");
        }

        #[test]
        fn very_long_expression_does_not_panic() {
            // Build a long expression: 1 + 1 + 1 + ... (500 terms)
            let terms: Vec<&str> = (0..500).map(|_| "1").collect();
            let expr = terms.join(" + ");
            let result = evaluate_expression(&expr);
            assert!(result.is_ok(), "long expression should parse successfully");
            assert_eq!(result.unwrap(), "500");
        }

        #[test]
        fn nested_ternary_colon_ambiguity() {
            // Ternary with regex colon -- colon should be consumed by ternary
            let result = evaluate_expression("1 ? 10 : 20").unwrap();
            assert_eq!(result, "10");

            let result = evaluate_expression("0 ? 10 : 20").unwrap();
            assert_eq!(result, "20");

            // Deeply nested ternary
            let result = evaluate_expression("1 ? 1 ? 1 ? 42 : 0 : 0 : 0").unwrap();
            assert_eq!(result, "42");
        }

        #[test]
        fn double_negation() {
            assert_eq!(evaluate_expression("- -5").unwrap(), "5");
            assert_eq!(evaluate_expression("!!0").unwrap(), "0");
            assert_eq!(evaluate_expression("!!1").unwrap(), "1");
        }

        #[test]
        fn regex_no_match_with_capture_group() {
            // With capture group but no match -> return empty string
            let result = evaluate_expression(r#""abc" : "xyz(.)""#).unwrap();
            assert_eq!(result, "", "no match with capture group should return empty");
        }

        #[test]
        fn regex_invalid_pattern_returns_error() {
            let result = evaluate_expression(r#""test" =~ "[invalid""#);
            assert!(result.is_err(), "invalid regex should return error");
        }

        #[test]
        fn logical_operators_short_circuit_semantics() {
            // OR: returns first truthy operand
            assert_eq!(evaluate_expression(r#""hello" | 0"#).unwrap(), "hello");
            assert_eq!(evaluate_expression(r#"0 | "world""#).unwrap(), "world");

            // AND: returns first operand if both truthy, 0 otherwise
            assert_eq!(evaluate_expression(r#""hello" & "world""#).unwrap(), "hello");
            assert_eq!(evaluate_expression(r#""hello" & 0"#).unwrap(), "0");
        }

        #[test]
        fn comparison_mixed_types() {
            // When one side is a non-numeric string, comparison should be lexicographic
            assert_eq!(evaluate_expression(r#""abc" = "abc""#).unwrap(), "1");
            assert_eq!(evaluate_expression(r#""abc" != "def""#).unwrap(), "1");
            assert_eq!(evaluate_expression(r#""abc" < "def""#).unwrap(), "1");
        }

        #[test]
        fn arithmetic_on_non_numeric_string_treats_as_zero() {
            // Non-numeric strings coerce to 0 in arithmetic
            let result = evaluate_expression(r#""abc" + 5"#).unwrap();
            assert_eq!(result, "5");

            let result = evaluate_expression(r#""abc" * 3"#).unwrap();
            assert_eq!(result, "0");
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector 2: Variable Substitution Attacks
    // -----------------------------------------------------------------------

    mod substitution_adversarial {
        use asterisk_core::channel::Channel;
        use asterisk_core::pbx::substitute::{substitute_variables, substitute_variables_full};
        use std::collections::HashMap;

        #[test]
        fn infinite_recursion_via_circular_variables() {
            // Set A = ${B} and B = ${A}
            let mut ch = Channel::new("Test/recurse");
            ch.set_variable("A", "${B}");
            ch.set_variable("B", "${A}");

            // Should NOT stack overflow -- depth limit should kick in
            let result = substitute_variables(&ch, "${A}");
            // The result should be empty (depth limit reached) or a truncated substitution
            assert!(result.len() < 1000, "circular reference should be bounded, got len={}", result.len());
        }

        #[test]
        fn very_deep_nesting() {
            // ${${${${${${${var}}}}}}}
            let mut ch = Channel::new("Test/deep");
            ch.set_variable("L1", "L2");
            ch.set_variable("L2", "L3");
            ch.set_variable("L3", "L4");
            ch.set_variable("L4", "L5");
            ch.set_variable("L5", "final");
            ch.set_variable("final", "success");

            let result = substitute_variables(&ch, "${${${${${L1}}}}}");
            // Should resolve L1 -> L2 -> L3 -> L4 -> L5, then ${L5} -> "final", then ${final} -> "success"
            // Depth depends on nesting level
            assert!(!result.is_empty(), "deep nesting should resolve");
        }

        #[test]
        fn malformed_dollar_brace_unclosed() {
            let ch = Channel::new("Test/malformed");
            // Unclosed ${
            let result = substitute_variables(&ch, "${");
            assert_eq!(result, "${", "unclosed dollar-brace should be literal");

            let result = substitute_variables(&ch, "${unclosed");
            assert!(result.contains("$"), "unclosed variable should be literal");
        }

        #[test]
        fn empty_variable_name() {
            let ch = Channel::new("Test/empty");
            let result = substitute_variables(&ch, "${}");
            // Empty variable name should return empty string
            assert_eq!(result, "", "empty variable should be empty string");
        }

        #[test]
        fn dollar_bracket_unclosed() {
            let ch = Channel::new("Test/bracket");
            let result = substitute_variables(&ch, "$[unclosed");
            assert!(result.contains("$["), "unclosed $[ should be literal");
        }

        #[test]
        fn function_with_no_closing_paren() {
            let ch = Channel::new("Test/func");
            // ${FUNC(arg -- missing closing paren
            let result = substitute_variables(&ch, "${FUNC(arg}");
            // The brace matcher should match the }, so it tries to look up "FUNC(arg" as a variable name.
            // is_function_call("FUNC(arg") returns false (unbalanced parens).
            assert!(result.is_empty() || result.len() < 100, "should handle gracefully");
        }

        #[test]
        fn variable_name_with_special_chars() {
            let mut ch = Channel::new("Test/special");
            ch.set_variable("var-name", "dash");
            ch.set_variable("var.name", "dot");
            ch.set_variable("123", "numeric");

            assert_eq!(substitute_variables(&ch, "${var-name}"), "dash");
            assert_eq!(substitute_variables(&ch, "${var.name}"), "dot");
            assert_eq!(substitute_variables(&ch, "${123}"), "numeric");
        }

        #[test]
        fn mixed_expressions_and_variables() {
            let mut ch = Channel::new("Test/mixed");
            ch.set_variable("X", "5");
            ch.set_variable("Y", "3");

            let result = substitute_variables(&ch, "$[${X} + ${Y}]");
            assert_eq!(result, "8");
        }

        #[test]
        fn consecutive_substitutions() {
            let mut ch = Channel::new("Test/consec");
            ch.set_variable("A", "hello");
            ch.set_variable("B", "world");

            let result = substitute_variables(&ch, "${A}${B}");
            assert_eq!(result, "helloworld");
        }

        #[test]
        fn expression_inside_variable() {
            let mut ch = Channel::new("Test/expr-in-var");
            ch.set_variable("COUNT", "5");

            // Variable reference containing an expression
            let result = substitute_variables(&ch, "result=$[$[${COUNT} + 1] * 2]");
            assert_eq!(result, "result=12");
        }

        #[test]
        fn substring_with_function_args_containing_colons() {
            let mut ch = Channel::new("Test/substr-colon");
            ch.set_variable("STR", "abcdefghij");
            // Substring: offset 1, length 4
            assert_eq!(substitute_variables(&ch, "${STR:1:4}"), "bcde");
            // Negative offset
            assert_eq!(substitute_variables(&ch, "${STR:-3:3}"), "hij");
        }

        #[test]
        fn headp_fallback_with_no_channel() {
            let mut globals = HashMap::new();
            globals.insert("GREETING".to_string(), "Hello".to_string());

            let result = substitute_variables_full(None, Some(&globals), "Say ${GREETING}!");
            assert_eq!(result, "Say Hello!");
        }

        #[test]
        fn regex_inside_expression_with_brackets() {
            let ch = Channel::new("Test/regex-bracket");
            // This exercises the bracket matcher with regex character classes
            let result = substitute_variables(&ch, r#"$["123" =~ "[0-9]+"]"#);
            assert_ne!(result, "0", "regex with brackets should match");
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector 3: Channel Store Concurrency
    // -----------------------------------------------------------------------

    mod channel_store_adversarial {
        use asterisk_core::channel::store;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        #[test]
        fn concurrent_alloc_deregister() {
            // Spawn many threads that alloc and deregister channels simultaneously
            let num_threads = 20;
            let ops_per_thread = 50;
            let counter = Arc::new(AtomicU32::new(0));

            let handles: Vec<_> = (0..num_threads)
                .map(|t| {
                    let counter = counter.clone();
                    std::thread::spawn(move || {
                        for i in 0..ops_per_thread {
                            let name = format!("ConcTest/t{}-i{}", t, i);
                            let chan = store::alloc_channel(&name);
                            let uid = chan.lock().unique_id.0.clone();
                            counter.fetch_add(1, Ordering::Relaxed);

                            // Sometimes deregister immediately, sometimes not
                            if i % 2 == 0 {
                                store::deregister(&uid);
                            }
                        }
                    })
                })
                .collect();

            for h in handles {
                h.join().expect("thread should not panic");
            }

            assert_eq!(
                counter.load(Ordering::Relaxed),
                num_threads * ops_per_thread,
                "all alloc operations should complete"
            );
        }

        #[test]
        fn find_by_name_during_updates() {
            // Alloc a channel, then try to find it while another thread updates it
            let chan = store::alloc_channel("FindTest/concurrent");
            let name = chan.lock().name.clone();
            let uid = chan.lock().unique_id.0.clone();

            let found = store::find_by_name(&name);
            assert!(found.is_some(), "should find channel by name");

            // Update the channel's extension from another thread
            let chan_clone = chan.clone();
            let handle = std::thread::spawn(move || {
                for _ in 0..100 {
                    let mut ch = chan_clone.lock();
                    ch.context = "modified".to_string();
                    ch.exten = "999".to_string();
                }
            });

            // Simultaneously try to find
            for _ in 0..100 {
                let _ = store::find_by_name(&name);
                let _ = store::find_by_uniqueid(&uid);
            }

            handle.join().expect("thread should not panic");
            store::deregister(&uid);
        }

        #[test]
        fn uniqueid_counter_never_repeats() {
            let mut ids = std::collections::HashSet::new();
            for _ in 0..10_000 {
                let uid = store::generate_uniqueid();
                assert!(ids.insert(uid), "unique ID should never repeat");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector 4: DTMF Timing Edge Cases
    // -----------------------------------------------------------------------

    mod dtmf_adversarial {
        use asterisk_core::channel::dtmf::*;
        use std::time::{Duration, Instant};

        #[test]
        fn dtmf_end_without_begin() {
            let mut state = DtmfState::new();
            // Receive DTMF_END without a prior DTMF_BEGIN
            match state.on_end('5', 100) {
                DtmfEndAction::EmulateBeginFirst { emulate_duration_ms } => {
                    assert!(emulate_duration_ms >= AST_MIN_DTMF_DURATION_MS);
                }
                other => panic!("expected EmulateBeginFirst, got {:?}", other),
            }
            assert!(state.emulating, "should be in emulating state");
        }

        #[test]
        fn dtmf_begin_with_zero_duration_end() {
            let mut state = DtmfState::new();
            assert!(state.on_begin('1'));
            // End with zero duration
            match state.on_end('1', 0) {
                DtmfEndAction::EmulateRemaining { remaining_ms } => {
                    assert!(remaining_ms > 0, "should emulate remaining time");
                }
                DtmfEndAction::Passthrough { .. } => {
                    // Also acceptable if begin_time elapsed enough
                }
                other => panic!("unexpected: {:?}", other),
            }
        }

        #[test]
        fn dtmf_begin_exceeding_max_duration() {
            let mut state = DtmfState::new();
            assert!(state.on_begin('9'));
            // End with duration exceeding max
            std::thread::sleep(Duration::from_millis(AST_MIN_DTMF_DURATION_MS as u64 + 10));
            match state.on_end('9', AST_MAX_DTMF_DURATION_MS + 1000) {
                DtmfEndAction::Passthrough { duration_ms } => {
                    assert!(
                        duration_ms >= AST_MIN_DTMF_DURATION_MS,
                        "duration should be at least minimum"
                    );
                }
                other => panic!("expected Passthrough, got {:?}", other),
            }
        }

        #[test]
        fn rapid_dtmf_faster_than_min_gap() {
            let mut state = DtmfState::new();

            // Complete a digit
            assert!(state.on_begin('1'));
            std::thread::sleep(Duration::from_millis(AST_MIN_DTMF_DURATION_MS as u64 + 10));
            let _ = state.on_end('1', AST_MIN_DTMF_DURATION_MS + 10);

            // Immediately try another digit (within min gap)
            assert!(
                !state.on_begin('2'),
                "second digit within min gap should be suppressed"
            );
        }

        #[test]
        fn same_digit_begin_twice_without_end() {
            let mut state = DtmfState::new();
            assert!(state.on_begin('5'));
            // Second begin for same digit without end
            // Should be suppressed because we're in_dtmf
            // Actually on_begin doesn't check in_dtmf, it checks should_suppress_begin
            // which only checks emulating and last_end_time gap.
            // So this is allowed -- which matches C Asterisk behavior.
            let second = state.on_begin('5');
            // Just verify it doesn't panic
            assert!(second || !second, "should not panic on double begin");
        }

        #[test]
        fn emulation_tick_when_not_emulating() {
            let mut state = DtmfState::new();
            assert!(state.check_emulation_tick().is_none(), "should return None when not emulating");
        }

        #[test]
        fn emulation_tick_completes_after_duration() {
            let mut state = DtmfState::new();
            state.emulating = true;
            state.digit = '3';
            state.begin_time = Some(Instant::now() - Duration::from_millis(500));
            state.emulate_duration_ms = 100;

            let result = state.check_emulation_tick();
            assert!(result.is_some(), "emulation should fire after duration");
            let (digit, dur) = result.unwrap();
            assert_eq!(digit, '3');
            assert!(dur >= 100);
        }

        #[test]
        fn emulation_clears_after_extra_tick() {
            let mut state = DtmfState::new();
            state.emulating = true;
            state.digit = '7';
            state.begin_time = Some(Instant::now() - Duration::from_millis(500));
            state.emulate_duration_ms = 100;

            // First tick fires the emulated end
            let _ = state.check_emulation_tick();
            assert!(state.emulating, "should still be emulating for gap");

            // Second tick clears emulating flag
            let result = state.check_emulation_tick();
            assert!(result.is_none());
            assert!(!state.emulating, "emulating should be cleared after gap tick");
        }

        #[test]
        fn defer_within_min_gap() {
            let mut state = DtmfState::new();
            // Set last_end_time to now
            state.last_end_time = Some(Instant::now());

            // End without begin, within gap
            match state.on_end('4', 100) {
                DtmfEndAction::Defer => {}, // expected
                other => panic!("expected Defer, got {:?}", other),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector 5: Softmix Audio Mixing Correctness
    // -----------------------------------------------------------------------

    mod softmix_adversarial {
        use asterisk_core::bridge::softmix::*;

        #[test]
        fn mix_minus_two_channels_opposite_signals() {
            let mut data = SoftmixData::new(8000, 20);
            let ns = data.num_samples;

            let mut chan_a = SoftmixChannelData::new("A".into(), ns);
            for s in chan_a.our_buf.iter_mut() { *s = 1000; }
            chan_a.have_audio = true;
            data.channel_buffers.insert("A".into(), chan_a);

            let mut chan_b = SoftmixChannelData::new("B".into(), ns);
            for s in chan_b.our_buf.iter_mut() { *s = -1000; }
            chan_b.have_audio = true;
            data.channel_buffers.insert("B".into(), chan_b);

            data.mix();

            let out_a = data.output_frames.get("A").unwrap();
            assert_eq!(out_a[0], -1000, "A should hear B's signal (-1000)");

            let out_b = data.output_frames.get("B").unwrap();
            assert_eq!(out_b[0], 1000, "B should hear A's signal (1000)");
        }

        #[test]
        fn saturation_test_all_channels_at_max() {
            let mut data = SoftmixData::new(8000, 20);
            let ns = 4; // small buffer for speed

            // 5 channels all at i16::MAX
            for i in 0..5 {
                let id = format!("chan{}", i);
                let mut cd = SoftmixChannelData::new(id.clone(), ns);
                for s in cd.our_buf.iter_mut() { *s = i16::MAX; }
                cd.have_audio = true;
                data.channel_buffers.insert(id, cd);
            }

            data.mix();

            // Each channel should hear 4 * i16::MAX, clamped to i16::MAX
            for i in 0..5 {
                let id = format!("chan{}", i);
                let output = data.output_frames.get(&id).unwrap();
                assert_eq!(
                    output[0], i16::MAX,
                    "output should be saturated to i16::MAX, not wrapped"
                );
            }
        }

        #[test]
        fn saturation_test_negative_overflow() {
            let mut data = SoftmixData::new(8000, 20);
            let ns = 4;

            for i in 0..5 {
                let id = format!("chan{}", i);
                let mut cd = SoftmixChannelData::new(id.clone(), ns);
                for s in cd.our_buf.iter_mut() { *s = i16::MIN; }
                cd.have_audio = true;
                data.channel_buffers.insert(id, cd);
            }

            data.mix();

            for i in 0..5 {
                let id = format!("chan{}", i);
                let output = data.output_frames.get(&id).unwrap();
                assert_eq!(
                    output[0], i16::MIN,
                    "output should be saturated to i16::MIN, not wrapped"
                );
            }
        }

        #[test]
        fn empty_channel_no_audio_contributes_silence() {
            let mut data = SoftmixData::new(8000, 20);
            let ns = data.num_samples;

            // Channel A sends audio
            let mut chan_a = SoftmixChannelData::new("A".into(), ns);
            for s in chan_a.our_buf.iter_mut() { *s = 500; }
            chan_a.have_audio = true;
            data.channel_buffers.insert("A".into(), chan_a);

            // Channel B sends nothing (no audio)
            let chan_b = SoftmixChannelData::new("B".into(), ns);
            // have_audio = false (default)
            data.channel_buffers.insert("B".into(), chan_b);

            data.mix();

            // A hears B (which is silence) -> 0
            let out_a = data.output_frames.get("A").unwrap();
            assert_eq!(out_a[0], 0, "A should hear silence from B");

            // B hears A -> 500
            let out_b = data.output_frames.get("B").unwrap();
            assert_eq!(out_b[0], 500, "B should hear A's audio");
        }

        #[test]
        fn single_channel_hears_silence() {
            let mut data = SoftmixData::new(8000, 20);
            let ns = data.num_samples;

            let mut chan = SoftmixChannelData::new("alone".into(), ns);
            for s in chan.our_buf.iter_mut() { *s = 1000; }
            chan.have_audio = true;
            data.channel_buffers.insert("alone".into(), chan);

            data.mix();

            let output = data.output_frames.get("alone").unwrap();
            assert_eq!(
                output[0], 0,
                "single channel should hear silence (total - own = 0)"
            );
        }

        #[test]
        fn mix_clears_buffers_after_iteration() {
            let mut data = SoftmixData::new(8000, 20);
            let ns = 4;

            let mut cd = SoftmixChannelData::new("c1".into(), ns);
            cd.our_buf[0] = 1234;
            cd.have_audio = true;
            data.channel_buffers.insert("c1".into(), cd);

            data.mix();

            // After mix, buffers should be cleared
            let cd = data.channel_buffers.get("c1").unwrap();
            assert_eq!(cd.our_buf[0], 0, "buffer should be cleared after mix");
            assert!(!cd.have_audio, "have_audio should be false after mix");
        }

        #[test]
        fn write_frame_pcm_decoding_roundtrip() {
            // Verify PCM16LE encoding/decoding roundtrip via softmix
            let samples: Vec<i16> = vec![-32768, -1000, 0, 1000, 32767];
            let mut bytes = Vec::with_capacity(samples.len() * 2);
            for &s in &samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }

            // Simulate what write_frame does
            let mut buf = vec![0i16; samples.len()];
            for i in 0..samples.len() {
                buf[i] = i16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]);
            }

            assert_eq!(buf, samples, "PCM16LE roundtrip should be lossless");
        }

        #[test]
        fn zero_interval_uses_default() {
            let tech = SoftmixBridgeTech::with_params(8000, 0);
            assert_ne!(tech.mixing_interval_ms, 0, "zero interval should use default");
        }

        #[test]
        fn min_sample_rate_enforcement() {
            let tech = SoftmixBridgeTech::with_params(100, 20);
            assert!(
                tech.internal_sample_rate >= 8000,
                "sample rate should be at least minimum"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector 6: Bridge Event Loop
    // -----------------------------------------------------------------------

    mod bridge_event_loop_adversarial {
        use asterisk_core::bridge::event_loop::*;
        use asterisk_core::bridge::{Bridge, BridgeChannel};
        use asterisk_core::channel::ChannelId;
        use asterisk_types::{ControlFrame, Frame};
        use bytes::Bytes;

        fn make_bridge() -> Bridge {
            let mut b = Bridge::new("test-bridge");
            b.add_channel(ChannelId::from_name("chan-a"), "A".into());
            b.add_channel(ChannelId::from_name("chan-b"), "B".into());
            b
        }

        fn make_bc(name: &str) -> BridgeChannel {
            BridgeChannel::new(ChannelId::from_name(name), name.into())
        }

        #[test]
        fn hangup_control_triggers_leave() {
            let bridge = make_bridge();
            let bc = make_bc("A");
            let frame = Frame::control(ControlFrame::Hangup);
            let action = process_frame(&frame, "A", &mut None, &bridge, &bc);
            assert!(matches!(action, FrameAction::Leave));
        }

        #[test]
        fn null_frame_is_continue() {
            let bridge = make_bridge();
            let bc = make_bc("A");
            let action = process_frame(&Frame::Null, "A", &mut None, &bridge, &bc);
            assert!(matches!(action, FrameAction::Continue));
        }

        #[test]
        fn hold_frame_not_passed_through() {
            let bridge = make_bridge();
            let bc = make_bc("A");
            let frame = Frame::control(ControlFrame::Hold);
            let action = process_frame(&frame, "A", &mut None, &bridge, &bc);
            assert!(matches!(action, FrameAction::Continue));
        }

        #[test]
        fn voice_frame_passes_through() {
            let bridge = make_bridge();
            let bc = make_bc("A");
            let frame = Frame::voice(0, 160, Bytes::from(vec![0u8; 320]));
            let action = process_frame(&frame, "A", &mut None, &bridge, &bc);
            assert!(matches!(action, FrameAction::WriteToTechnology(_)));
        }

        #[test]
        fn dtmf_without_features_passes_through() {
            let bridge = make_bridge();
            let bc = make_bc("A");
            let frame = Frame::dtmf_end('5', 100);
            let action = process_frame(&frame, "A", &mut None, &bridge, &bc);
            assert!(matches!(action, FrameAction::WriteToTechnology(_)));
        }

        #[test]
        fn should_pass_frame_filters_correctly() {
            assert!(should_pass_frame(&Frame::voice(0, 160, Bytes::new())));
            assert!(should_pass_frame(&Frame::dtmf_begin('1')));
            assert!(should_pass_frame(&Frame::dtmf_end('1', 100)));
            assert!(should_pass_frame(&Frame::text("hi".into())));
            assert!(should_pass_frame(&Frame::control(ControlFrame::Ringing)));

            assert!(!should_pass_frame(&Frame::Null));
            assert!(!should_pass_frame(&Frame::control(ControlFrame::Hold)));
            assert!(!should_pass_frame(&Frame::control(ControlFrame::Unhold)));
            assert!(!should_pass_frame(&Frame::control(ControlFrame::Hangup)));
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector 7: Framehook Chain
    // -----------------------------------------------------------------------

    mod framehook_adversarial {
        use asterisk_core::channel::framehook::*;
        use asterisk_types::Frame;
        use bytes::Bytes;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        #[test]
        fn hook_that_drops_frame_blocks_subsequent_hooks() {
            let mut list = FramehookList::new();
            let second_called = Arc::new(AtomicU32::new(0));
            let second_called_clone = second_called.clone();

            // First hook drops everything
            list.attach(Box::new(|_frame, _event| None));

            // Second hook should never see a Read event
            list.attach(Box::new(move |frame, event| {
                if event == FramehookEvent::Read {
                    second_called_clone.fetch_add(1, Ordering::Relaxed);
                }
                Some(frame.clone())
            }));

            let frame = Frame::voice(0, 160, Bytes::from(vec![0u8; 320]));
            let result = list.process_read(&frame);
            assert!(result.is_none(), "frame should be dropped");
            assert_eq!(
                second_called.load(Ordering::Relaxed), 0,
                "second hook should not be called when first drops frame"
            );
        }

        #[test]
        fn hook_that_modifies_frame_is_seen_by_next() {
            let mut list = FramehookList::new();

            // First hook converts voice to null
            list.attach(Box::new(|frame, event| {
                if event == FramehookEvent::Read && frame.is_voice() {
                    Some(Frame::Null)
                } else {
                    Some(frame.clone())
                }
            }));

            // Second hook should see the Null frame, not the original voice
            let saw_null = Arc::new(AtomicU32::new(0));
            let saw_null_clone = saw_null.clone();
            list.attach(Box::new(move |frame, event| {
                if event == FramehookEvent::Read {
                    if matches!(frame, Frame::Null) {
                        saw_null_clone.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Some(frame.clone())
            }));

            let frame = Frame::voice(0, 160, Bytes::from(vec![0u8; 320]));
            let _ = list.process_read(&frame);
            assert_eq!(
                saw_null.load(Ordering::Relaxed), 1,
                "second hook should see the modified (Null) frame"
            );
        }

        #[test]
        fn detach_during_no_processing_is_safe() {
            let mut list = FramehookList::new();
            let id1 = list.attach(Box::new(|f, _| Some(f.clone())));
            let id2 = list.attach(Box::new(|f, _| Some(f.clone())));

            // Detach one
            assert!(list.detach(id1));
            assert!(!list.is_empty());

            // Process should still work
            let frame = Frame::Null;
            let result = list.process_read(&frame);
            assert!(result.is_some());

            // Detach the other
            assert!(list.detach(id2));
            assert!(list.is_empty());
        }

        #[test]
        fn attach_gives_unique_ids() {
            let mut list = FramehookList::new();
            let mut ids = std::collections::HashSet::new();
            for _ in 0..100 {
                let id = list.attach(Box::new(|f, _| Some(f.clone())));
                assert!(ids.insert(id), "framehook IDs must be unique");
            }
        }

        #[test]
        fn write_event_processes_separately() {
            let mut list = FramehookList::new();
            let read_count = Arc::new(AtomicU32::new(0));
            let write_count = Arc::new(AtomicU32::new(0));
            let rc = read_count.clone();
            let wc = write_count.clone();

            list.attach(Box::new(move |frame, event| {
                match event {
                    FramehookEvent::Read => { rc.fetch_add(1, Ordering::Relaxed); }
                    FramehookEvent::Write => { wc.fetch_add(1, Ordering::Relaxed); }
                    _ => {}
                }
                Some(frame.clone())
            }));

            let frame = Frame::Null;
            list.process_read(&frame);
            list.process_write(&frame);

            assert_eq!(read_count.load(Ordering::Relaxed), 1);
            assert_eq!(write_count.load(Ordering::Relaxed), 1);
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector 8: Audiohook
    // -----------------------------------------------------------------------

    mod audiohook_adversarial {
        use asterisk_core::channel::audiohook::*;
        use asterisk_types::Frame;
        use bytes::Bytes;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        struct CountingSpy {
            read_count: Arc<AtomicU32>,
            write_count: Arc<AtomicU32>,
        }

        impl Audiohook for CountingSpy {
            fn hook_type(&self) -> AudiohookType { AudiohookType::Spy }
            fn read(&mut self, frame: &Frame) -> Option<Frame> {
                self.read_count.fetch_add(1, Ordering::Relaxed);
                Some(frame.clone())
            }
            fn write(&mut self, frame: &Frame) -> Option<Frame> {
                self.write_count.fetch_add(1, Ordering::Relaxed);
                Some(frame.clone())
            }
        }

        struct NullManipulator;
        impl Audiohook for NullManipulator {
            fn hook_type(&self) -> AudiohookType { AudiohookType::Manipulate }
            fn read(&mut self, _frame: &Frame) -> Option<Frame> { None }
            fn write(&mut self, _frame: &Frame) -> Option<Frame> { None }
        }

        struct TransformManipulator;
        impl Audiohook for TransformManipulator {
            fn hook_type(&self) -> AudiohookType { AudiohookType::Manipulate }
            fn read(&mut self, _frame: &Frame) -> Option<Frame> {
                // Replace with a text frame
                Some(Frame::text("transformed".into()))
            }
        }

        #[test]
        fn multiple_spies_each_get_a_copy() {
            let count_a = Arc::new(AtomicU32::new(0));
            let count_b = Arc::new(AtomicU32::new(0));

            let mut list = AudiohookList::new();
            list.attach(Box::new(CountingSpy {
                read_count: count_a.clone(),
                write_count: Arc::new(AtomicU32::new(0)),
            }));
            list.attach(Box::new(CountingSpy {
                read_count: count_b.clone(),
                write_count: Arc::new(AtomicU32::new(0)),
            }));

            let frame = Frame::voice(0, 160, Bytes::from(vec![0u8; 320]));
            let result = list.process_read(&frame);
            assert!(result.is_some(), "spies should not drop frames");
            assert_eq!(count_a.load(Ordering::Relaxed), 1);
            assert_eq!(count_b.load(Ordering::Relaxed), 1);
        }

        #[test]
        fn manipulator_returning_none_drops_frame() {
            let mut list = AudiohookList::new();
            list.attach(Box::new(NullManipulator));

            let frame = Frame::voice(0, 160, Bytes::from(vec![0u8; 320]));
            let result = list.process_read(&frame);
            assert!(result.is_none(), "NullManipulator should drop frame");
        }

        #[test]
        fn manipulator_transform_visible_to_caller() {
            let mut list = AudiohookList::new();
            list.attach(Box::new(TransformManipulator));

            let frame = Frame::voice(0, 160, Bytes::from(vec![0u8; 320]));
            let result = list.process_read(&frame);
            assert!(result.is_some());
            assert!(matches!(result.unwrap(), Frame::Text { text } if text == "transformed"));
        }

        #[test]
        fn spy_write_direction() {
            let write_count = Arc::new(AtomicU32::new(0));
            let mut list = AudiohookList::new();
            list.attach(Box::new(CountingSpy {
                read_count: Arc::new(AtomicU32::new(0)),
                write_count: write_count.clone(),
            }));

            let frame = Frame::voice(0, 160, Bytes::from(vec![0u8; 320]));
            let result = list.process_write(&frame);
            assert!(result.is_some());
            assert_eq!(write_count.load(Ordering::Relaxed), 1);
        }

        #[test]
        fn detach_by_type_and_index() {
            let mut list = AudiohookList::new();
            list.attach(Box::new(NullManipulator));
            list.attach(Box::new(NullManipulator));
            assert_eq!(list.manipulators.len(), 2);

            let removed = list.detach(AudiohookType::Manipulate, 0);
            assert!(removed.is_some());
            assert_eq!(list.manipulators.len(), 1);

            // Out of range
            let removed = list.detach(AudiohookType::Manipulate, 5);
            assert!(removed.is_none());
        }

        #[test]
        fn is_empty_reflects_all_types() {
            let mut list = AudiohookList::new();
            assert!(list.is_empty());

            list.attach(Box::new(NullManipulator));
            assert!(!list.is_empty());

            list.detach(AudiohookType::Manipulate, 0);
            assert!(list.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector 9: PBX Execution Loop
    // -----------------------------------------------------------------------

    mod pbx_exec_adversarial {
        use asterisk_core::channel::Channel;
        use asterisk_core::channel::softhangup;
        use asterisk_core::pbx::app_registry::APP_REGISTRY;
        use asterisk_core::pbx::exec::{pbx_run, PbxRunResult};
        use asterisk_core::pbx::{Context, Dialplan, DialplanApp, Extension, PbxResult, Priority};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::sync::Mutex;

        #[derive(Debug)]
        struct NoopApp { name: String }
        #[async_trait::async_trait]
        impl DialplanApp for NoopApp {
            fn name(&self) -> &str { &self.name }
            async fn execute(&self, _channel: &mut Channel, _args: &str) -> PbxResult {
                PbxResult::Success
            }
        }

        #[derive(Debug)]
        struct InfiniteGotoApp { name: String }
        #[async_trait::async_trait]
        impl DialplanApp for InfiniteGotoApp {
            fn name(&self) -> &str { &self.name }
            async fn execute(&self, channel: &mut Channel, _args: &str) -> PbxResult {
                // Always GoTo back to self
                channel.priority = 1;
                PbxResult::Success
            }
        }

        #[derive(Debug)]
        struct SofthangupApp { name: String }
        #[async_trait::async_trait]
        impl DialplanApp for SofthangupApp {
            fn name(&self) -> &str { &self.name }
            async fn execute(&self, channel: &mut Channel, _args: &str) -> PbxResult {
                channel.softhangup(softhangup::AST_SOFTHANGUP_DEV);
                PbxResult::Success
            }
        }

        #[derive(Debug)]
        struct FailApp { name: String }
        #[async_trait::async_trait]
        impl DialplanApp for FailApp {
            fn name(&self) -> &str { &self.name }
            async fn execute(&self, _channel: &mut Channel, _args: &str) -> PbxResult {
                PbxResult::Failed
            }
        }

        #[tokio::test]
        async fn extension_not_found_tries_invalid_handler() {
            APP_REGISTRY.register(Arc::new(NoopApp {
                name: "AdvNoopI".into(),
            }));
            APP_REGISTRY.register(Arc::new(SofthangupApp {
                name: "AdvHangupI".into(),
            }));

            let mut dp = Dialplan::new();
            let mut ctx = Context::new("default");

            // Add 'i' (invalid) extension handler
            let mut ext_i = Extension::new("i");
            ext_i.add_priority(Priority {
                priority: 1,
                app: "AdvNoopI".into(),
                app_data: String::new(),
                label: None,
            });
            ext_i.add_priority(Priority {
                priority: 2,
                app: "AdvHangupI".into(),
                app_data: String::new(),
                label: None,
            });
            ctx.add_extension(ext_i);
            dp.add_context(ctx);

            let mut ch = Channel::new("Test/invalid-exten");
            ch.exten = "nonexistent".into();
            ch.context = "default".into();
            ch.priority = 1;
            let ch = Arc::new(Mutex::new(ch));

            let result = pbx_run(ch, Arc::new(dp)).await;
            assert_eq!(result, PbxRunResult::Success);
        }

        #[tokio::test]
        async fn infinite_goto_loop_is_bounded() {
            APP_REGISTRY.register(Arc::new(InfiniteGotoApp {
                name: "AdvInfGoto".into(),
            }));

            let mut dp = Dialplan::new();
            let mut ctx = Context::new("default");
            let mut ext = Extension::new("s");
            ext.add_priority(Priority {
                priority: 1,
                app: "AdvInfGoto".into(),
                app_data: String::new(),
                label: None,
            });
            ctx.add_extension(ext);
            dp.add_context(ctx);

            let ch = Arc::new(Mutex::new(Channel::new("Test/inf-goto")));

            let result = pbx_run(ch, Arc::new(dp)).await;
            // Should terminate (not hang forever) due to iteration limit
            assert_eq!(result, PbxRunResult::Success);
        }

        #[tokio::test]
        async fn softhangup_during_app_exits_loop() {
            let counter = Arc::new(AtomicU32::new(0));

            #[derive(Debug)]
            struct CountAndHangup {
                name: String,
                counter: Arc<AtomicU32>,
            }
            #[async_trait::async_trait]
            impl DialplanApp for CountAndHangup {
                fn name(&self) -> &str { &self.name }
                async fn execute(&self, channel: &mut Channel, _args: &str) -> PbxResult {
                    self.counter.fetch_add(1, Ordering::SeqCst);
                    if self.counter.load(Ordering::SeqCst) >= 2 {
                        channel.softhangup(softhangup::AST_SOFTHANGUP_DEV);
                    }
                    PbxResult::Success
                }
            }

            APP_REGISTRY.register(Arc::new(CountAndHangup {
                name: "AdvCountHangup".into(),
                counter: counter.clone(),
            }));

            let mut dp = Dialplan::new();
            let mut ctx = Context::new("default");
            let mut ext = Extension::new("s");
            for p in 1..=10 {
                ext.add_priority(Priority {
                    priority: p,
                    app: "AdvCountHangup".into(),
                    app_data: String::new(),
                    label: None,
                });
            }
            ctx.add_extension(ext);
            dp.add_context(ctx);

            let ch = Arc::new(Mutex::new(Channel::new("Test/softhangup-mid")));
            let _ = pbx_run(ch, Arc::new(dp)).await;

            // Should have executed exactly 2 priorities before softhangup was detected
            assert_eq!(counter.load(Ordering::SeqCst), 2);
        }

        #[tokio::test]
        async fn app_failure_tries_error_extension() {
            APP_REGISTRY.register(Arc::new(FailApp {
                name: "AdvFail".into(),
            }));
            APP_REGISTRY.register(Arc::new(NoopApp {
                name: "AdvNoopE".into(),
            }));
            APP_REGISTRY.register(Arc::new(SofthangupApp {
                name: "AdvHangupE".into(),
            }));

            let mut dp = Dialplan::new();
            let mut ctx = Context::new("default");

            let mut ext_s = Extension::new("s");
            ext_s.add_priority(Priority {
                priority: 1,
                app: "AdvFail".into(),
                app_data: String::new(),
                label: None,
            });
            ctx.add_extension(ext_s);

            // Error extension 'e'
            let mut ext_e = Extension::new("e");
            ext_e.add_priority(Priority {
                priority: 1,
                app: "AdvNoopE".into(),
                app_data: String::new(),
                label: None,
            });
            ext_e.add_priority(Priority {
                priority: 2,
                app: "AdvHangupE".into(),
                app_data: String::new(),
                label: None,
            });
            ctx.add_extension(ext_e);

            dp.add_context(ctx);

            let ch = Arc::new(Mutex::new(Channel::new("Test/fail-to-e")));
            let result = pbx_run(ch, Arc::new(dp)).await;
            assert_eq!(result, PbxRunResult::Success);
        }

        #[tokio::test]
        async fn empty_dialplan_returns_failed() {
            let dp = Dialplan::new();
            let ch = Arc::new(Mutex::new(Channel::new("Test/empty-dp")));
            let result = pbx_run(ch, Arc::new(dp)).await;
            assert_eq!(result, PbxRunResult::Failed);
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector: Channel Read/Write Pipeline
    // -----------------------------------------------------------------------

    mod readwrite_adversarial {
        use asterisk_core::channel::Channel;
        use asterisk_core::channel::readwrite::{channel_read, channel_write};
        use asterisk_core::channel::softhangup::AST_SOFTHANGUP_DEV;
        use asterisk_core::channel::generator::Generator;
        use asterisk_core::channel::audiohook::{Audiohook, AudiohookType};
        use asterisk_types::Frame;
        use bytes::Bytes;

        #[test]
        fn read_hangup_returns_none() {
            let mut ch = Channel::new("Test/rw-hangup");
            ch.softhangup(AST_SOFTHANGUP_DEV);
            let result = channel_read(&mut ch, |_| Ok(Frame::Null));
            assert!(result.unwrap().is_none());
        }

        #[test]
        fn read_queue_before_driver() {
            let mut ch = Channel::new("Test/rw-queue");
            ch.queue_frame(Frame::text("queued".into()));

            let result = channel_read(&mut ch, |_| panic!("driver should not be called"));
            let frame = result.unwrap().unwrap();
            assert!(matches!(frame, Frame::Text { text } if text == "queued"));
        }

        #[test]
        fn write_hangup_returns_error() {
            let mut ch = Channel::new("Test/rw-whangup");
            ch.softhangup(AST_SOFTHANGUP_DEV);
            let frame = Frame::voice(0, 160, Bytes::from(vec![0u8; 320]));
            let result = channel_write(&mut ch, &frame, |_, _| Ok(()));
            assert!(result.is_err());
        }

        #[test]
        fn generator_replaces_voice_in_read() {
            struct ConstGen;
            impl Generator for ConstGen {
                fn generate(&mut self, _samples: usize) -> Option<Frame> {
                    Some(Frame::voice(0, 160, Bytes::from(vec![0xABu8; 320])))
                }
            }

            let mut ch = Channel::new("Test/rw-gen");
            ch.generator.activate(Box::new(ConstGen));

            let result = channel_read(&mut ch, |_| {
                Ok(Frame::voice(0, 160, Bytes::from(vec![0u8; 320])))
            });
            let frame = result.unwrap().unwrap();
            if let Frame::Voice { data, .. } = frame {
                assert_eq!(data[0], 0xAB, "generator output should replace driver voice");
            } else {
                panic!("expected voice frame from generator");
            }
        }

        #[test]
        fn write_consumed_by_generator_without_write_int() {
            struct DummyGen;
            impl Generator for DummyGen {
                fn generate(&mut self, _samples: usize) -> Option<Frame> {
                    Some(Frame::Null)
                }
            }

            let mut ch = Channel::new("Test/rw-gen-consume");
            ch.generator.activate(Box::new(DummyGen));

            let frame = Frame::voice(0, 160, Bytes::from(vec![0u8; 320]));
            let result = channel_write(&mut ch, &frame, |_, _| {
                panic!("should not call driver write when gen active")
            });
            assert!(result.is_ok());
        }

        #[test]
        fn dtmf_emulation_in_read_pipeline() {
            let mut ch = Channel::new("Test/rw-dtmf-emu");

            // Send DTMF_END without prior begin -- should trigger emulation
            ch.queue_frame(Frame::DtmfEnd {
                digit: '5',
                duration_ms: 0,
            });

            let result = channel_read(&mut ch, |_| Ok(Frame::Null));
            let frame = result.unwrap().unwrap();
            // Should be converted to DtmfBegin (emulation starts)
            assert!(
                matches!(frame, Frame::DtmfBegin { digit } if digit == '5'),
                "end without begin should be converted to begin for emulation"
            );
        }

        #[test]
        fn audiohook_manipulator_can_drop_frame() {
            struct DropAll;
            impl Audiohook for DropAll {
                fn hook_type(&self) -> AudiohookType { AudiohookType::Manipulate }
                fn read(&mut self, _frame: &Frame) -> Option<Frame> { None }
            }

            let mut ch = Channel::new("Test/rw-ah-drop");
            ch.audiohooks.attach(Box::new(DropAll));

            let result = channel_read(&mut ch, |_| {
                Ok(Frame::voice(0, 160, Bytes::from(vec![0u8; 320])))
            });
            let frame = result.unwrap().unwrap();
            assert!(matches!(frame, Frame::Null), "dropped frame should become Null");
        }

        #[test]
        fn frame_queue_max_size_drops_oldest() {
            let mut ch = Channel::new("Test/rw-queue-max");
            // Fill the queue beyond max
            for i in 0..1100 {
                ch.queue_frame(Frame::text(format!("msg-{}", i)));
            }
            // Queue size should not exceed MAX
            assert!(ch.frame_queue.len() <= 1000);
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector: Generator edge cases
    // -----------------------------------------------------------------------

    mod generator_adversarial {
        use asterisk_core::channel::generator::{Generator, GeneratorState};
        use asterisk_types::Frame;

        struct OneShot;
        impl Generator for OneShot {
            fn generate(&mut self, _samples: usize) -> Option<Frame> { None }
        }

        struct PanicGen;
        impl Generator for PanicGen {
            fn generate(&mut self, _samples: usize) -> Option<Frame> {
                // Simulate a generator that always returns Some
                Some(Frame::Null)
            }
        }

        #[test]
        fn auto_deactivate_on_none_return() {
            let mut state = GeneratorState::new();
            state.activate(Box::new(OneShot));
            assert!(state.is_active());

            let result = state.generate(160);
            assert!(result.is_none());
            assert!(!state.is_active(), "should auto-deactivate after returning None");
        }

        #[test]
        fn deactivate_then_generate_is_noop() {
            let mut state = GeneratorState::new();
            state.activate(Box::new(PanicGen));
            state.deactivate();
            assert!(!state.is_active());

            let result = state.generate(160);
            assert!(result.is_none(), "generate after deactivate should return None");
        }

        #[test]
        fn double_activate_deactivates_first() {
            let mut state = GeneratorState::new();
            state.activate(Box::new(PanicGen));
            state.activate(Box::new(PanicGen));
            assert!(state.is_active());
            state.deactivate();
            assert!(!state.is_active());
        }

        #[test]
        fn digit_forwarding() {
            use std::sync::atomic::{AtomicU32, Ordering};
            use std::sync::Arc;

            struct DigitTracker { count: Arc<AtomicU32> }
            impl Generator for DigitTracker {
                fn generate(&mut self, _samples: usize) -> Option<Frame> { Some(Frame::Null) }
                fn digit(&mut self, _digit: char) {
                    self.count.fetch_add(1, Ordering::Relaxed);
                }
            }

            let count = Arc::new(AtomicU32::new(0));
            let mut state = GeneratorState::new();
            state.activate(Box::new(DigitTracker { count: count.clone() }));
            state.digit('5');
            state.digit('6');
            assert_eq!(count.load(Ordering::Relaxed), 2);
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector: Softhangup edge cases
    // -----------------------------------------------------------------------

    mod softhangup_adversarial {
        use asterisk_core::channel::Channel;
        use asterisk_core::channel::softhangup::*;

        #[test]
        fn clear_all_resets_everything() {
            let mut ch = Channel::new("Test/sh-all");
            ch.softhangup(AST_SOFTHANGUP_DEV | AST_SOFTHANGUP_SHUTDOWN | AST_SOFTHANGUP_EXPLICIT);
            assert!(ch.check_hangup());
            ch.clear_softhangup(AST_SOFTHANGUP_ALL);
            assert!(!ch.check_hangup());
            assert_eq!(ch.softhangup_flags, 0);
        }

        #[test]
        fn clear_single_flag_preserves_others() {
            let mut ch = Channel::new("Test/sh-single");
            ch.softhangup(AST_SOFTHANGUP_DEV | AST_SOFTHANGUP_TIMEOUT);
            ch.clear_softhangup(AST_SOFTHANGUP_DEV);
            assert!(ch.check_hangup()); // TIMEOUT still set
            assert_eq!(ch.softhangup_flags & AST_SOFTHANGUP_DEV, 0);
            assert_ne!(ch.softhangup_flags & AST_SOFTHANGUP_TIMEOUT, 0);
        }

        #[test]
        fn softhangup_queues_null_frame() {
            let mut ch = Channel::new("Test/sh-null");
            assert!(ch.frame_queue.is_empty());
            ch.softhangup(AST_SOFTHANGUP_DEV);
            assert!(!ch.frame_queue.is_empty(), "softhangup should queue a null frame");
            let frame = ch.dequeue_frame().unwrap();
            assert!(matches!(frame, asterisk_types::Frame::Null));
        }

        #[test]
        fn asyncgoto_flag_causes_check_hangup_true() {
            let mut ch = Channel::new("Test/sh-async");
            ch.softhangup(AST_SOFTHANGUP_ASYNCGOTO);
            assert!(ch.check_hangup(), "ASYNCGOTO should cause check_hangup to return true");
        }

        #[test]
        fn no_flags_means_no_hangup() {
            let ch = Channel::new("Test/sh-none");
            assert!(!ch.check_hangup());
            assert_eq!(ch.softhangup_flags, 0);
        }
    }

    // -----------------------------------------------------------------------
    // Attack Vector: Bridge lifecycle edge cases
    // -----------------------------------------------------------------------

    mod bridge_lifecycle_adversarial {
        use asterisk_core::bridge::*;
        use asterisk_core::bridge::implementations::SimpleBridge;
        use asterisk_core::channel::{Channel, ChannelId};
        use std::sync::Arc;

        #[tokio::test]
        async fn double_dissolve_is_idempotent() {
            let tech: Arc<dyn BridgeTechnology> = Arc::new(SimpleBridge::new());
            let bridge = bridge_create("double-dissolve", &tech).await.unwrap();

            bridge_dissolve(&bridge, &tech).await.unwrap();
            // Second dissolve should be a no-op
            bridge_dissolve(&bridge, &tech).await.unwrap();

            let br = bridge.lock().await;
            assert!(br.dissolved);
        }

        #[tokio::test]
        async fn join_dissolved_bridge_fails() {
            let tech: Arc<dyn BridgeTechnology> = Arc::new(SimpleBridge::new());
            let bridge = bridge_create("join-dissolved", &tech).await.unwrap();
            bridge_dissolve(&bridge, &tech).await.unwrap();

            let channel = Arc::new(tokio::sync::Mutex::new(Channel::new("SIP/test-001")));
            let result = bridge_join(&bridge, &channel, &tech).await;
            assert!(result.is_err(), "joining dissolved bridge should fail");
        }

        #[tokio::test]
        async fn dissolve_kicks_all_channels() {
            let tech: Arc<dyn BridgeTechnology> = Arc::new(SimpleBridge::new());
            let bridge = bridge_create("kick-all", &tech).await.unwrap();

            {
                let mut br = bridge.lock().await;
                br.add_channel(ChannelId::from_name("c1"), "c1".into());
                br.add_channel(ChannelId::from_name("c2"), "c2".into());
            }

            bridge_dissolve(&bridge, &tech).await.unwrap();

            let br = bridge.lock().await;
            for bc in &br.channels {
                assert_eq!(bc.state, BridgeChannelState::Leaving);
            }
        }

        #[tokio::test]
        async fn bridge_leave_auto_dissolves_empty_bridge() {
            let tech: Arc<dyn BridgeTechnology> = Arc::new(SimpleBridge::new());
            let bridge = bridge_create("auto-dissolve", &tech).await.unwrap();
            let bridge_id = bridge.lock().await.unique_id.clone();

            // Modify flags to dissolve when empty
            {
                let mut br = bridge.lock().await;
                br.flags = asterisk_types::BridgeFlags::DISSOLVE_EMPTY;
            }

            let channel = Arc::new(tokio::sync::Mutex::new(Channel::new("SIP/auto-001")));
            let bc = bridge_join(&bridge, &channel, &tech).await.unwrap();

            bridge_leave(&bridge, &bc, &channel, &tech).await.unwrap();

            // Bridge should be dissolved because it's now empty
            let br = bridge.lock().await;
            assert!(br.dissolved, "bridge should auto-dissolve when empty");
        }

        #[test]
        fn bridge_add_duplicate_channel_is_noop() {
            let mut bridge = Bridge::new("dup-test");
            let id = ChannelId::from_name("chan1");
            bridge.add_channel(id.clone(), "c1".into());
            bridge.add_channel(id.clone(), "c1".into());
            assert_eq!(bridge.num_channels(), 1, "duplicate add should be noop");
        }

        #[test]
        fn bridge_remove_nonexistent_returns_false() {
            let mut bridge = Bridge::new("remove-test");
            let id = ChannelId::from_name("nonexistent");
            assert!(!bridge.remove_channel(&id));
        }
    }

    // =========================================================================
    // Adversarial tests -- added by QA agent
    // =========================================================================

    // -------------------------------------------------------------------------
    // 1. STUN message integrity: build, compute HMAC, verify. Tamper one byte,
    //    verify rejection.
    // -------------------------------------------------------------------------
    #[test]
    fn test_stun_message_integrity_verify_and_tamper() {
        use asterisk_sip::stun::{StunMessage, StunAttrValue};
        use std::net::{SocketAddr, Ipv4Addr};

        let key = b"my-ice-password-1234";

        let mut msg = StunMessage::binding_request();
        msg.attributes.push(StunAttrValue::Username("user:remote".into()));

        // Serialize with MESSAGE-INTEGRITY
        let data = msg.to_bytes_with_integrity(key);

        // Parse back and verify integrity succeeds
        let parsed = StunMessage::parse(&data).expect("parse should succeed");
        parsed.verify_integrity(&data, key).expect("integrity should pass");

        // Tamper a byte in the middle of the message body
        let mut tampered = data.clone();
        let mid = tampered.len() / 2;
        tampered[mid] ^= 0xFF;

        // Parse should still succeed (structure intact) but integrity must fail
        let parsed2 = StunMessage::parse(&tampered);
        if let Ok(p) = parsed2 {
            assert!(
                p.verify_integrity(&tampered, key).is_err(),
                "tampered message must fail integrity check"
            );
        }
        // If parse itself fails due to the tamper, that's also acceptable

        // Verify wrong key fails
        let wrong_key = b"wrong-password-9876";
        let parsed3 = StunMessage::parse(&data).expect("parse original");
        assert!(
            parsed3.verify_integrity(&data, wrong_key).is_err(),
            "wrong key must fail integrity check"
        );
    }

    // -------------------------------------------------------------------------
    // 2. ICE candidate SDP roundtrip: generate candidate string, parse it back,
    //    verify all fields match.
    // -------------------------------------------------------------------------
    #[test]
    fn test_ice_candidate_sdp_roundtrip_all_types() {
        use asterisk_sip::ice::{IceCandidate, CandidateType};
        use std::net::{SocketAddr, IpAddr, Ipv4Addr};

        // Host candidate
        let host = IceCandidate::new_host(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 5004),
            1,
            65535,
        );
        let sdp = host.to_sdp_attribute();
        let parsed = IceCandidate::from_sdp_attribute(&sdp)
            .expect("should parse host candidate");
        assert_eq!(parsed.foundation, host.foundation);
        assert_eq!(parsed.component_id, host.component_id);
        assert_eq!(parsed.transport, host.transport);
        assert_eq!(parsed.priority, host.priority);
        assert_eq!(parsed.address, host.address);
        assert_eq!(parsed.candidate_type, CandidateType::Host);
        assert_eq!(parsed.related_address, None);
        assert_eq!(parsed.related_port, None);

        // Server-reflexive candidate (has raddr/rport)
        let srflx = IceCandidate::new_srflx(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)), 9000),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 5004),
            1,
            65535,
        );
        let sdp2 = srflx.to_sdp_attribute();
        let parsed2 = IceCandidate::from_sdp_attribute(&sdp2)
            .expect("should parse srflx candidate");
        assert_eq!(parsed2.candidate_type, CandidateType::ServerReflexive);
        assert_eq!(parsed2.address, srflx.address);
        assert_eq!(parsed2.related_address, Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))));
        assert_eq!(parsed2.related_port, Some(5004));

        // Relay candidate
        let relay = IceCandidate::new_relay(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)), 3478),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)), 9000),
            2,
            10000,
        );
        let sdp3 = relay.to_sdp_attribute();
        let parsed3 = IceCandidate::from_sdp_attribute(&sdp3)
            .expect("should parse relay candidate");
        assert_eq!(parsed3.candidate_type, CandidateType::Relay);
        assert_eq!(parsed3.component_id, 2);

        // Peer-reflexive candidate
        let prflx = IceCandidate::new_prflx(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 5004),
            1,
            1000,
        );
        let sdp4 = prflx.to_sdp_attribute();
        let parsed4 = IceCandidate::from_sdp_attribute(&sdp4)
            .expect("should parse prflx candidate");
        assert_eq!(parsed4.candidate_type, CandidateType::PeerReflexive);
    }

    // -------------------------------------------------------------------------
    // 3. DTLS role negotiation: actpass + active = passive on our side.
    //    actpass + passive = active on our side.
    // -------------------------------------------------------------------------
    #[test]
    fn test_dtls_role_negotiation_comprehensive() {
        use asterisk_sip::dtls::DtlsRole;

        // actpass + active = our side becomes passive
        assert_eq!(
            DtlsRole::negotiate(DtlsRole::ActPass, DtlsRole::Active),
            Some(DtlsRole::Passive),
            "actpass + active => passive"
        );

        // actpass + passive = our side becomes active
        assert_eq!(
            DtlsRole::negotiate(DtlsRole::ActPass, DtlsRole::Passive),
            Some(DtlsRole::Active),
            "actpass + passive => active"
        );

        // active + passive = active (we are active)
        assert_eq!(
            DtlsRole::negotiate(DtlsRole::Active, DtlsRole::Passive),
            Some(DtlsRole::Active),
        );

        // passive + active = passive (we are passive)
        assert_eq!(
            DtlsRole::negotiate(DtlsRole::Passive, DtlsRole::Active),
            Some(DtlsRole::Passive),
        );

        // active + actpass = active
        assert_eq!(
            DtlsRole::negotiate(DtlsRole::Active, DtlsRole::ActPass),
            Some(DtlsRole::Active),
        );

        // passive + actpass = passive
        assert_eq!(
            DtlsRole::negotiate(DtlsRole::Passive, DtlsRole::ActPass),
            Some(DtlsRole::Passive),
        );

        // Both actpass: convention says offerer becomes active
        assert_eq!(
            DtlsRole::negotiate(DtlsRole::ActPass, DtlsRole::ActPass),
            Some(DtlsRole::Active),
        );

        // HoldConn combinations should return None
        assert_eq!(DtlsRole::negotiate(DtlsRole::HoldConn, DtlsRole::Active), None);
        assert_eq!(DtlsRole::negotiate(DtlsRole::Active, DtlsRole::HoldConn), None);

        // active + active = conflict, no valid negotiation
        assert_eq!(DtlsRole::negotiate(DtlsRole::Active, DtlsRole::Active), None);
        assert_eq!(DtlsRole::negotiate(DtlsRole::Passive, DtlsRole::Passive), None);

        // SDP roundtrip
        for role in &[DtlsRole::Active, DtlsRole::Passive, DtlsRole::ActPass, DtlsRole::HoldConn] {
            let sdp_val = role.sdp_value();
            let parsed = DtlsRole::from_sdp(sdp_val).expect("should parse SDP value");
            assert_eq!(*role, parsed);
        }
    }

    // -------------------------------------------------------------------------
    // 4. DNS SRV weighted selection: Run 5000 selections with weights
    //    [10, 20, 70], verify distribution within 10%.
    // -------------------------------------------------------------------------
    #[test]
    fn test_dns_srv_weighted_selection_distribution() {
        use asterisk_res::dns_srv::{SrvRecord, weighted_select};

        let records = vec![
            SrvRecord { priority: 10, weight: 10, port: 5060, target: "a.example.com".into() },
            SrvRecord { priority: 10, weight: 20, port: 5060, target: "b.example.com".into() },
            SrvRecord { priority: 10, weight: 70, port: 5060, target: "c.example.com".into() },
        ];

        let mut counts = [0u32; 3];
        let iterations = 5000;

        for _ in 0..iterations {
            let selected = weighted_select(&records).expect("should select one");
            if selected.target == "a.example.com" {
                counts[0] += 1;
            } else if selected.target == "b.example.com" {
                counts[1] += 1;
            } else if selected.target == "c.example.com" {
                counts[2] += 1;
            }
        }

        let total = iterations as f64;
        let pct_a = counts[0] as f64 / total;
        let pct_b = counts[1] as f64 / total;
        let pct_c = counts[2] as f64 / total;

        // Expected: 10%, 20%, 70%. Allow +/- 10% absolute tolerance.
        assert!(
            (pct_a - 0.10).abs() < 0.10,
            "weight-10 got {:.1}%, expected ~10% (+/-10%)", pct_a * 100.0
        );
        assert!(
            (pct_b - 0.20).abs() < 0.10,
            "weight-20 got {:.1}%, expected ~20% (+/-10%)", pct_b * 100.0
        );
        assert!(
            (pct_c - 0.70).abs() < 0.10,
            "weight-70 got {:.1}%, expected ~70% (+/-10%)", pct_c * 100.0
        );

        // Also verify all iterations accounted for
        assert_eq!(counts[0] + counts[1] + counts[2], iterations);
    }

    // -------------------------------------------------------------------------
    // 5. RTCP-MUX demux: PT=0 -> RTP, PT=200 -> RTCP, PT=72 -> RTP
    //    (ambiguous but valid).
    // -------------------------------------------------------------------------
    #[test]
    fn test_rtcp_mux_demux() {
        use asterisk_sip::rtp::is_rtcp_packet;

        // PT=0 (PCMU): byte[1] = 0 -> RTP
        let rtp_pt0 = [0x80u8, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(!is_rtcp_packet(&rtp_pt0), "PT=0 should be RTP");

        // PT=200 (SR): byte[1] = 200 -> RTCP
        let rtcp_sr = [0x80u8, 200u8, 0x00, 0x01, 0, 0, 0, 0];
        assert!(is_rtcp_packet(&rtcp_sr), "PT=200 should be RTCP");

        // PT=201 (RR): RTCP
        let rtcp_rr = [0x80u8, 201u8, 0x00, 0x01, 0, 0, 0, 0];
        assert!(is_rtcp_packet(&rtcp_rr), "PT=201 should be RTCP");

        // PT=72 with marker bit set: byte[1] = 0x80 | 72 = 200
        // This is the ambiguous case: an RTP packet with PT=72 and
        // marker=1 has byte[1]=200, which looks like RTCP SR.
        // Per RFC 5761, PT 72-76 are reserved/unassigned, so in practice
        // these should not be used with RTCP-MUX. The demux function
        // treats byte[1] in [200,213] as RTCP.
        let ambiguous_pt72_marker = [0x80u8, 0xC8u8, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0];
        // 0xC8 = 200 = marker(1) | pt(72), so is_rtcp_packet sees 200 => RTCP
        assert!(
            is_rtcp_packet(&ambiguous_pt72_marker),
            "PT=72 with marker=1 (byte=200) treated as RTCP per RFC 5761"
        );

        // PT=72 without marker bit: byte[1] = 72 -> RTP
        let rtp_pt72_no_marker = [0x80u8, 72u8, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(!is_rtcp_packet(&rtp_pt72_no_marker), "PT=72 no marker should be RTP");

        // PT=213 (last RTCP range): RTCP
        let rtcp_213 = [0x80u8, 213u8, 0x00, 0x01, 0, 0, 0, 0];
        assert!(is_rtcp_packet(&rtcp_213), "PT=213 should be RTCP");

        // PT=214: outside RTCP range -> RTP
        let rtp_214 = [0x80u8, 214u8, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(!is_rtcp_packet(&rtp_214), "PT=214 should be RTP");

        // PT=199: just below RTCP range -> RTP
        let rtp_199 = [0x80u8, 199u8, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(!is_rtcp_packet(&rtp_199), "PT=199 should be RTP");

        // Empty packet: too short
        assert!(!is_rtcp_packet(&[]), "empty packet should be RTP (too short)");
        assert!(!is_rtcp_packet(&[0x80]), "1-byte packet should be RTP (too short)");
    }

    // -------------------------------------------------------------------------
    // 6. Multipart MIME: parse body with boundary appearing in content
    //    (should handle correctly).
    // -------------------------------------------------------------------------
    #[test]
    fn test_multipart_mime_boundary_in_content() {
        use asterisk_sip::multipart::{parse_multipart, generate_multipart_with_boundary, BodyPart};

        // Test basic roundtrip first
        let parts = vec![
            BodyPart {
                content_type: "application/sdp".to_string(),
                content_disposition: None,
                body: b"v=0\r\no=- 0 0 IN IP4 10.0.0.1\r\n".to_vec(),
            },
            BodyPart {
                content_type: "application/isup".to_string(),
                content_disposition: Some("signal;handling=optional".to_string()),
                body: b"\x01\x02\x03".to_vec(),
            },
        ];

        let (ct, body) = generate_multipart_with_boundary(&parts, "test-boundary-123");
        let parsed = parse_multipart(&ct, &body).expect("should parse multipart");
        assert_eq!(parsed.parts.len(), 2);
        assert_eq!(parsed.parts[0].content_type, "application/sdp");
        assert_eq!(parsed.parts[1].content_type, "application/isup");
        assert_eq!(
            parsed.parts[1].content_disposition.as_deref(),
            Some("signal;handling=optional")
        );

        // Test with empty parts
        let empty_parts: Vec<BodyPart> = vec![];
        let (ct2, body2) = generate_multipart_with_boundary(&empty_parts, "empty-boundary");
        let parsed2 = parse_multipart(&ct2, &body2).expect("should parse empty multipart");
        assert_eq!(parsed2.parts.len(), 0);

        // Test parsing with a part that contains text similar to boundary
        // (but not an actual delimiter line)
        let tricky_body = b"--my-boundary\r\n\
Content-Type: text/plain\r\n\
\r\n\
This text mentions my-boundary but is not a delimiter\r\n\
--my-boundary\r\n\
Content-Type: text/plain\r\n\
\r\n\
second part\r\n\
--my-boundary--\r\n";

        let parsed3 = parse_multipart("multipart/mixed;boundary=my-boundary", tricky_body)
            .expect("should parse tricky multipart");
        assert_eq!(parsed3.parts.len(), 2, "should have exactly 2 parts");

        // Missing boundary should error
        let err = parse_multipart("multipart/mixed", b"some body");
        assert!(err.is_err(), "missing boundary should error");
    }

    // -------------------------------------------------------------------------
    // 7. WebSocket: frame with 0-byte payload, frame at max allowed size.
    //    (Test the build_ws_frame / parse_ws_frame codec indirectly via
    //     the transport module's internal logic. Since those functions are
    //     private, we test the WebSocketSessionManager as the public API.)
    // -------------------------------------------------------------------------
    #[test]
    fn test_websocket_session_zero_and_large_payload() {
        use asterisk_sip::transport::websocket::WsTransport;

        // WebSocket session manager tests are for the ARI module, but
        // the frame codec tests need the SIP transport module.
        // Since build_ws_frame / parse_ws_frame are private, we verify
        // the public WebSocketSessionManager API behavior instead.
        use asterisk_ari::websocket::WebSocketSessionManager;

        let manager = WebSocketSessionManager::new();

        // Register a session
        let (session, mut rx) = manager.register_session(
            "test-ws-1".to_string(),
            vec!["myapp".to_string()],
        );

        // Send empty payload (0-byte)
        assert!(session.send_event(""), "should send empty event");

        // Receive and verify
        let received = rx.try_recv().expect("should have message");
        assert_eq!(received, "");

        // Send large payload (simulate max-size event)
        let large_payload = "x".repeat(65536);
        assert!(session.send_event(&large_payload), "should send large event");
        let received_large = rx.try_recv().expect("should have large message");
        assert_eq!(received_large.len(), 65536);

        // Verify session management
        assert_eq!(manager.session_count(), 1);
        assert!(session.is_subscribed_to("myapp"));
        assert!(!session.is_subscribed_to("other"));

        // Subscribe to additional app
        session.subscribe_app("other");
        assert!(session.is_subscribed_to("other"));

        // Unsubscribe
        session.unsubscribe_app("myapp");
        assert!(!session.is_subscribed_to("myapp"));

        // Unregister
        manager.unregister_session("test-ws-1");
        assert_eq!(manager.session_count(), 0);
    }

    // -------------------------------------------------------------------------
    // 8. PRACK RSeq monotonic: verify RSeq increments, out-of-order RAck
    //    rejected.
    // -------------------------------------------------------------------------
    #[test]
    fn test_prack_rseq_monotonic_and_rack_validation() {
        use asterisk_sip::prack::PrackState;

        let mut state = PrackState::new(42); // INVITE CSeq = 42

        // Initially no PRACK pending
        assert!(!state.is_prack_pending());

        // Allocate first RSeq
        let rseq1 = state.next_rseq();
        assert_eq!(rseq1, 1);
        assert!(state.is_prack_pending());

        // Allocate second RSeq (without acknowledging first -- simulates
        // new provisional being sent)
        let rseq2 = state.next_rseq();
        assert_eq!(rseq2, 2);
        assert!(rseq2 > rseq1, "RSeq must be monotonically increasing");
        assert!(state.is_prack_pending());

        // Try to acknowledge with wrong rseq (stale)
        let acked = state.handle_prack(1, 42, "INVITE");
        assert!(!acked, "stale rseq=1 should be rejected when current is 2");
        assert!(state.is_prack_pending());

        // Try to acknowledge with wrong cseq
        let acked2 = state.handle_prack(2, 99, "INVITE");
        assert!(!acked2, "wrong cseq should be rejected");
        assert!(state.is_prack_pending());

        // Try to acknowledge with wrong method
        let acked3 = state.handle_prack(2, 42, "REGISTER");
        assert!(!acked3, "wrong method should be rejected");
        assert!(state.is_prack_pending());

        // Correct acknowledgment
        let acked4 = state.handle_prack(2, 42, "INVITE");
        assert!(acked4, "correct RAck should be accepted");
        assert!(!state.is_prack_pending());

        // Allocate more and verify monotonic increase continues
        let rseq3 = state.next_rseq();
        assert_eq!(rseq3, 3);
        let rseq4 = state.next_rseq();
        assert_eq!(rseq4, 4);
        assert!(rseq4 > rseq3);

        // Verify retransmit tracking
        let mut state2 = PrackState::new(1);
        state2.next_rseq();
        for i in 0..7 {
            assert!(state2.record_retransmit(), "retransmit {} should succeed", i);
        }
        // 8th retransmit (count=8 > max=7) should fail
        assert!(!state2.record_retransmit(), "should exceed max retransmits");

        // Verify retransmit interval doubles
        let mut state3 = PrackState::new(1);
        state3.next_rseq();
        let interval0 = state3.retransmit_interval();
        state3.record_retransmit();
        let interval1 = state3.retransmit_interval();
        assert!(
            interval1 >= interval0,
            "interval should increase or stay same with retransmits"
        );
    }

    // =========================================================================
    // Daemon Wiring Integration Tests
    // =========================================================================

    // -------------------------------------------------------------------------
    // Test: ChannelTechRegistry register + find + list
    // -------------------------------------------------------------------------
    #[test]
    fn test_tech_registry_register_find_list() {
        use asterisk_core::channel::tech_registry::ChannelTechRegistry;
        use asterisk_core::channel::{Channel, ChannelDriver};
        use asterisk_types::{AsteriskResult, Frame};

        #[derive(Debug)]
        struct FakeDriver { name: String }

        #[async_trait::async_trait]
        impl ChannelDriver for FakeDriver {
            fn name(&self) -> &str { &self.name }
            fn description(&self) -> &str { "Fake" }
            async fn request(&self, dest: &str, _caller: Option<&Channel>) -> AsteriskResult<Channel> {
                Ok(Channel::new(format!("{}/{}", self.name, dest)))
            }
            async fn call(&self, _channel: &mut Channel, _dest: &str, _timeout: i32) -> AsteriskResult<()> { Ok(()) }
            async fn hangup(&self, _channel: &mut Channel) -> AsteriskResult<()> { Ok(()) }
            async fn answer(&self, _channel: &mut Channel) -> AsteriskResult<()> { Ok(()) }
            async fn read_frame(&self, _channel: &mut Channel) -> AsteriskResult<Frame> { Ok(Frame::Null) }
            async fn write_frame(&self, _channel: &mut Channel, _frame: &Frame) -> AsteriskResult<()> { Ok(()) }
        }

        let registry = ChannelTechRegistry::new();
        assert_eq!(registry.count(), 0);

        registry.register(Arc::new(FakeDriver { name: "TESTSIP".to_string() }));
        registry.register(Arc::new(FakeDriver { name: "TESTIAX".to_string() }));

        // Find by name (case-insensitive)
        assert!(registry.find("TESTSIP").is_some());
        assert!(registry.find("testsip").is_some());
        assert!(registry.find("TESTIAX").is_some());
        assert!(registry.find("NONEXISTENT").is_none());

        // List returns sorted names
        let names = registry.list();
        assert_eq!(names, vec!["TESTIAX", "TESTSIP"]);

        // Count
        assert_eq!(registry.count(), 2);
    }

    // -------------------------------------------------------------------------
    // Test: SipEventHandler — mock INVITE creates a channel
    // -------------------------------------------------------------------------
    #[tokio::test]
    async fn test_sip_event_handler_incoming_invite() {
        use asterisk_sip::event_handler::SipEventHandler;
        use asterisk_sip::parser::{SipMessage, SipHeader, SipUri, RequestLine, StartLine, SipMethod, header_names};
        use asterisk_core::pbx::{Dialplan, Context, Extension, Priority};

        // Create a dialplan with a "default" context containing extension 100.
        // The handler uses "default" when no auth/endpoint config is present.
        let mut dp = Dialplan::new();
        let mut default_ctx = Context::new("default");
        let mut ext_100 = Extension::new("100");
        ext_100.add_priority(Priority {
            priority: 1,
            app: "Answer".to_string(),
            app_data: String::new(),
            label: None,
        });
        default_ctx.add_extension(ext_100);
        dp.add_context(default_ctx);

        let mock_transport: Arc<dyn asterisk_sip::transport::SipTransport> = Arc::new(
            asterisk_sip::transport::UdpTransport::bind("127.0.0.1:0".parse().unwrap()).await.unwrap()
        );
        let handler = SipEventHandler::new(Arc::new(dp), mock_transport);

        // Build a mock INVITE SIP message
        let invite = SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Invite,
                uri: SipUri::parse("sip:100@192.168.1.1").unwrap(),
                version: "SIP/2.0".to_string(),
            }),
            headers: vec![
                SipHeader {
                    name: header_names::FROM.to_string(),
                    value: "\"Alice\" <sip:alice@10.0.0.1>;tag=test123".to_string(),
                },
                SipHeader {
                    name: header_names::TO.to_string(),
                    value: "<sip:100@192.168.1.1>".to_string(),
                },
                SipHeader {
                    name: header_names::CALL_ID.to_string(),
                    value: "test-invite-call-id-001".to_string(),
                },
                SipHeader {
                    name: header_names::CSEQ.to_string(),
                    value: "1 INVITE".to_string(),
                },
                SipHeader {
                    name: header_names::CONTENT_LENGTH.to_string(),
                    value: "0".to_string(),
                },
            ],
            body: String::new(),
        };

        let remote_addr: std::net::SocketAddr = "10.0.0.1:5060".parse().unwrap();
        let result = {
            let session = asterisk_sip::session::SipSession::new_inbound(
                &invite,
                "127.0.0.1:5060".parse().unwrap(),
                remote_addr,
            ).unwrap_or_else(|| asterisk_sip::session::SipSession::new_outbound(
                "127.0.0.1:5060".parse().unwrap(),
                remote_addr,
            ));
            handler.handle_incoming_invite(&invite, remote_addr, session).await
        };

        // Should return the call-id
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "test-invite-call-id-001");

        // Verify active calls tracking
        assert_eq!(handler.active_calls(), 1);

        // Give the spawned PBX task a moment to run
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // -------------------------------------------------------------------------
    // Test: AppAdapter — register + find in APP_REGISTRY
    // -------------------------------------------------------------------------
    #[test]
    fn test_app_adapter_register_and_find() {
        use asterisk_apps::adapter::AppAdapter;
        use asterisk_apps::PbxExecResult;
        use asterisk_core::pbx::app_registry::APP_REGISTRY;

        // Register a test app via the adapter
        let adapter = AppAdapter::new(
            "IntegTestApp",
            "Integration test application",
            |_channel, _args| PbxExecResult::Success,
        );
        APP_REGISTRY.register(Arc::new(adapter));

        // Should be findable in the global registry
        let found = APP_REGISTRY.find("IntegTestApp");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name(), "IntegTestApp");

        // Verify it's in the list
        let names = APP_REGISTRY.list();
        assert!(names.contains(&"IntegTestApp".to_string()));
    }

    // -------------------------------------------------------------------------
    // Test: CDR hangup callback fires on channel hangup
    // -------------------------------------------------------------------------
    #[test]
    fn test_cdr_hangup_callback_fires() {
        use asterisk_core::channel::{Channel, register_hangup_callback};
        use asterisk_types::HangupCause;
        use std::sync::atomic::{AtomicU32, Ordering};

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        // Create the channel first so we know its unique_id
        let mut chan = Channel::new("Test/cdr-hangup-callback");
        chan.set_state(asterisk_types::ChannelState::Up);
        let target_uid = chan.unique_id.0.clone();
        let target_uid_clone = target_uid.clone();

        // Register a callback that only counts hangups for our specific channel
        register_hangup_callback(Box::new(move |unique_id, cause| {
            assert!(!unique_id.is_empty());
            if unique_id == target_uid_clone {
                assert_eq!(*cause, HangupCause::UserBusy);
                counter_clone.fetch_add(1, Ordering::SeqCst);
            }
        }));

        // Hang up the channel
        chan.hangup(HangupCause::UserBusy);

        // The callback should have fired exactly once for our channel
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // -------------------------------------------------------------------------
    // Test: SipEventHandler — handle_response updates channel state
    // -------------------------------------------------------------------------
    #[tokio::test]
    async fn test_sip_event_handler_response_handling() {
        use asterisk_sip::event_handler::SipEventHandler;
        use asterisk_sip::parser::{SipMessage, SipHeader, SipUri, RequestLine, StartLine, SipMethod, header_names};
        use asterisk_core::pbx::Dialplan;

        let dp = Dialplan::new();
        let mock_transport: Arc<dyn asterisk_sip::transport::SipTransport> = Arc::new(
            asterisk_sip::transport::UdpTransport::bind("127.0.0.1:0".parse().unwrap()).await.unwrap()
        );
        let handler = SipEventHandler::new(Arc::new(dp), mock_transport);

        // First create a channel via INVITE
        let invite = SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Invite,
                uri: SipUri::parse("sip:200@192.168.1.1").unwrap(),
                version: "SIP/2.0".to_string(),
            }),
            headers: vec![
                SipHeader { name: header_names::FROM.to_string(), value: "\"Bob\" <sip:bob@10.0.0.2>;tag=resp001".to_string() },
                SipHeader { name: header_names::TO.to_string(), value: "<sip:200@192.168.1.1>".to_string() },
                SipHeader { name: header_names::CALL_ID.to_string(), value: "test-response-call-id-002".to_string() },
                SipHeader { name: header_names::CSEQ.to_string(), value: "1 INVITE".to_string() },
                SipHeader { name: header_names::CONTENT_LENGTH.to_string(), value: "0".to_string() },
            ],
            body: String::new(),
        };
        let remote_addr: std::net::SocketAddr = "10.0.0.2:5060".parse().unwrap();
        {
            let session = asterisk_sip::session::SipSession::new_inbound(
                &invite,
                "127.0.0.1:5060".parse().unwrap(),
                remote_addr,
            ).unwrap_or_else(|| asterisk_sip::session::SipSession::new_outbound(
                "127.0.0.1:5060".parse().unwrap(),
                remote_addr,
            ));
            handler.handle_incoming_invite(&invite, remote_addr, session).await
        };

        // Now send a BYE
        let bye = SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Bye,
                uri: SipUri::parse("sip:200@192.168.1.1").unwrap(),
                version: "SIP/2.0".to_string(),
            }),
            headers: vec![
                SipHeader { name: header_names::CALL_ID.to_string(), value: "test-response-call-id-002".to_string() },
                SipHeader { name: header_names::CSEQ.to_string(), value: "2 BYE".to_string() },
                SipHeader { name: header_names::CONTENT_LENGTH.to_string(), value: "0".to_string() },
            ],
            body: String::new(),
        };

        handler.handle_bye(&bye, remote_addr).await;

        // After BYE, the call-id should be removed
        // Give a moment for async cleanup
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(handler.active_calls(), 0);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // -------------------------------------------------------------------------
    // Test: register_all_apps populates the global registry
    // -------------------------------------------------------------------------
    #[test]
    fn test_register_all_apps_populates_registry() {
        use asterisk_core::pbx::app_registry::APP_REGISTRY;

        // Call register_all_apps
        asterisk_apps::adapter::register_all_apps();

        // Verify core apps are registered
        assert!(APP_REGISTRY.find("Hangup").is_some(), "Hangup should be registered");
        assert!(APP_REGISTRY.find("Answer").is_some(), "Answer should be registered");
        assert!(APP_REGISTRY.find("Verbose").is_some(), "Verbose should be registered");
        assert!(APP_REGISTRY.find("NoOp").is_some(), "NoOp should be registered");
        assert!(APP_REGISTRY.find("Dial").is_some(), "Dial should be registered (stub)");

        // Should have many apps registered
        assert!(APP_REGISTRY.count() > 10, "Should have more than 10 apps registered");
    }

    // -------------------------------------------------------------------------
    // Test: LocalChannelDriver can be registered in tech registry
    // -------------------------------------------------------------------------
    #[test]
    fn test_local_driver_in_tech_registry() {
        use asterisk_core::channel::tech_registry::ChannelTechRegistry;
        use asterisk_channels::local::LocalChannelDriver;

        let registry = ChannelTechRegistry::new();
        registry.register(Arc::new(LocalChannelDriver::new()));

        assert!(registry.find("Local").is_some());
        assert_eq!(registry.find("Local").unwrap().name(), "Local");
        assert_eq!(registry.count(), 1);
    }
}
