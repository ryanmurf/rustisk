use crate::builder::SipBuilder;
use crate::builder::{Transport, ContentType};

/// Example demonstrating the fluent SIP message builder API
/// 
/// This example shows how to build various SIP messages using the type-safe
/// builder pattern with compile-time validation.

pub fn example_invite_with_sdp() -> Result<String, Box<dyn std::error::Error>> {
    let invite = SipBuilder::invite()
        .to("sip:alice@example.com")?
        .from("sip:bob@example.com")
        .via_udp("192.168.1.10:5060")
        .call_id_auto()
        .cseq(1)
        .contact("sip:bob@192.168.1.10:5060")
        .user_agent("Rustisk/0.1.0")
        .allow(&["INVITE", "ACK", "BYE", "CANCEL", "OPTIONS"])
        .sdp(
            "v=0\r\n\
             o=bob 123456 789012 IN IP4 192.168.1.10\r\n\
             s=Voice Call\r\n\
             c=IN IP4 192.168.1.10\r\n\
             t=0 0\r\n\
             m=audio 5004 RTP/AVP 0 8\r\n\
             a=rtpmap:0 PCMU/8000\r\n\
             a=rtpmap:8 PCMA/8000\r\n"
        )
        .build()?;

    Ok(invite.to_string())
}

pub fn example_register_with_authentication() -> Result<String, Box<dyn std::error::Error>> {
    let register = SipBuilder::register()
        .to("sip:bob@proxy.example.com")?
        .from("sip:bob@proxy.example.com")
        .via_tcp("10.0.0.5:5060")
        .call_id("reg-12345@rustisk")
        .cseq(1)
        .contact("sip:bob@10.0.0.5:5060")
        .expires(3600)
        .user_agent("Rustisk SIP UA")
        .header("Authorization", "Digest username=\"bob\", realm=\"example.com\", nonce=\"abc123\", uri=\"sip:proxy.example.com\", response=\"def456\"")
        .build()?;

    Ok(register.to_string())
}

pub fn example_bye_with_reason() -> Result<String, Box<dyn std::error::Error>> {
    let bye = SipBuilder::bye()
        .to("sip:alice@example.com;tag=alice-tag")?
        .from("sip:bob@example.com;tag=bob-tag")
        .via_udp("10.0.0.1:5060")
        .call_id("call-12345@asterisk")
        .cseq(2)
        .header("Reason", "SIP;cause=200;text=\"Call completed\"")
        .build()?;

    Ok(bye.to_string())
}

pub fn example_options_ping() -> Result<String, Box<dyn std::error::Error>> {
    let options = SipBuilder::options()
        .to("sip:*")?
        .from("sip:keepalive@monitor.example.com")
        .via_udp("10.0.0.1:5060")
        .call_id_auto()
        .cseq(1)
        .allow(&["INVITE", "ACK", "BYE", "CANCEL", "OPTIONS", "REGISTER", "INFO"])
        .user_agent("Rustisk/0.1.0")
        .build()?;

    Ok(options.to_string())
}

pub fn example_secure_invite() -> Result<String, Box<dyn std::error::Error>> {
    let invite = SipBuilder::invite()
        .to("sips:alice@secure.example.com")?
        .from("sips:bob@secure.example.com")
        .via_tls("secure.example.com:5061")
        .call_id_auto()
        .cseq(1)
        .contact("sips:bob@secure.example.com:5061")
        .user_agent("Rustisk/0.1.0")
        .header("Supported", "100rel,timer,replaces,norefersub")
        .header("Session-Expires", "1800")
        .sdp(
            "v=0\r\n\
             o=bob 123456 789012 IN IP4 203.0.113.10\r\n\
             s=Secure Voice Call\r\n\
             c=IN IP4 203.0.113.10\r\n\
             t=0 0\r\n\
             m=audio 5004 RTP/SAVP 0\r\n\
             a=rtpmap:0 PCMU/8000\r\n\
             a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:WnD7c1ksDGs+dIefCEo8omPg4uO8DYIinNGL5yxQ\r\n"
        )
        .build()?;

    Ok(invite.to_string())
}

pub fn example_message_with_text() -> Result<String, Box<dyn std::error::Error>> {
    let message = SipBuilder::invite()  // Using INVITE for demonstration, you can add MESSAGE method
        .to("sip:alice@example.com")?
        .from("sip:bob@example.com")
        .via_udp("10.0.0.1:5060")
        .call_id_auto()
        .cseq(1)
        .contact("sip:bob@10.0.0.1:5060")
        .header("Subject", "Meeting Reminder")
        .text_body("Don't forget about our meeting at 3 PM today!")
        .build()?;

    Ok(message.to_string())
}

pub fn example_custom_transport_sctp() -> Result<String, Box<dyn std::error::Error>> {
    let invite = SipBuilder::invite()
        .to("sip:conference@example.com")?
        .from("sip:organizer@example.com")
        .via_transport("192.168.1.100:5060", Transport::SCTP)
        .call_id_auto()
        .cseq(1)
        .contact("sip:organizer@192.168.1.100:5060")
        .header("Subject", "Conference Call")
        .header("Priority", "urgent")
        .sdp(
            "v=0\r\n\
             o=organizer 123456 789012 IN IP4 192.168.1.100\r\n\
             s=Conference Call\r\n\
             c=IN IP4 192.168.1.100\r\n\
             t=0 0\r\n\
             m=audio 5004 RTP/AVP 0 8 18\r\n\
             a=rtpmap:0 PCMU/8000\r\n\
             a=rtpmap:8 PCMA/8000\r\n\
             a=rtpmap:18 G729/8000\r\n"
        )
        .build()?;

    Ok(invite.to_string())
}

pub fn example_ack_for_invite() -> Result<String, Box<dyn std::error::Error>> {
    let ack = SipBuilder::ack()
        .to("sip:alice@example.com;tag=alice-tag")?
        .from("sip:bob@example.com;tag=bob-tag")
        .via_udp("10.0.0.1:5060")
        .call_id("invite-call@asterisk")
        .cseq(1)  // Same CSeq as the INVITE
        .build()?;

    Ok(ack.to_string())
}

pub fn example_cancel_invite() -> Result<String, Box<dyn std::error::Error>> {
    let cancel = SipBuilder::cancel()
        .to("sip:alice@example.com")?  // Same as original INVITE (no tag)
        .from("sip:bob@example.com;tag=bob-tag")  // Same From tag as INVITE
        .via_udp("10.0.0.1:5060")  // Same Via as INVITE
        .call_id("invite-call@asterisk")  // Same Call-ID as INVITE
        .cseq(1)  // Same CSeq number as INVITE, but method is CANCEL
        .header("Reason", "SIP;cause=487;text=\"Request Terminated\"")
        .build()?;

    Ok(cancel.to_string())
}

// Example demonstrating compile-time type safety
/*
This code should NOT compile due to missing required headers:

pub fn example_invalid_invite() {
    let _invalid = SipBuilder::invite()
        .to("sip:alice@example.com").unwrap()
        // Missing: from, via, call_id, cseq, contact
        .build(); // Compilation error!
}

pub fn example_invalid_invite_missing_contact() {
    let _invalid = SipBuilder::invite()
        .to("sip:alice@example.com").unwrap()
        .from("sip:bob@example.com")
        .via_udp("10.0.0.1:5060")
        .call_id_auto()
        .cseq(1)
        // Missing contact - required for INVITE
        .build(); // Compilation error!
}
*/

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_examples_compile() {
        assert!(example_invite_with_sdp().is_ok());
        assert!(example_register_with_authentication().is_ok());
        assert!(example_bye_with_reason().is_ok());
        assert!(example_options_ping().is_ok());
        assert!(example_secure_invite().is_ok());
        assert!(example_message_with_text().is_ok());
        assert!(example_custom_transport_sctp().is_ok());
        assert!(example_ack_for_invite().is_ok());
        assert!(example_cancel_invite().is_ok());
    }

    #[test]
    fn test_invite_contains_sdp() {
        let result = example_invite_with_sdp().unwrap();
        assert!(result.contains("INVITE sip:alice@example.com SIP/2.0"));
        assert!(result.contains("Content-Type: application/sdp"));
        assert!(result.contains("m=audio"));
    }

    #[test]
    fn test_register_contains_auth() {
        let result = example_register_with_authentication().unwrap();
        assert!(result.contains("REGISTER sip:proxy.example.com SIP/2.0"));
        assert!(result.contains("Authorization: Digest"));
        assert!(result.contains("Expires: 3600"));
    }
}
