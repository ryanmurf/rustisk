# Type-Safe SIP Message Builder API

This document describes the type-safe fluent SIP message builder API for the asterisk-sip crate.

## Overview

The SIP builder uses the **typestate pattern** to ensure compile-time validation of required headers for different SIP message types. This prevents common mistakes like missing required headers or incorrect header combinations.

## Quick Start

```rust
use asterisk_sip::builder::SipBuilder;

// Build an INVITE request
let invite = SipBuilder::invite()
    .to("sip:alice@example.com")?
    .from("sip:bob@example.com")
    .via_udp("10.0.0.1:5060")
    .call_id_auto()
    .cseq(1)
    .contact("sip:bob@10.0.0.1:5060")
    .sdp("v=0\r\no=- 123456 789012 IN IP4 10.0.0.1\r\n...")
    .build()?;
```

## Supported SIP Methods

### INVITE
Creates a SIP INVITE request for establishing calls.

**Required headers:** To, From, Via, Call-ID, CSeq, Contact  
**Optional:** User-Agent, Allow, SDP body, custom headers

```rust
let invite = SipBuilder::invite()
    .to("sip:alice@example.com")?
    .from("sip:bob@example.com")
    .via_udp("192.168.1.10:5060")
    .call_id_auto()
    .cseq(1)
    .contact("sip:bob@192.168.1.10:5060")
    .user_agent("Rustisk/0.1.0")
    .allow(&["INVITE", "ACK", "BYE", "CANCEL", "OPTIONS"])
    .sdp("v=0\r\n...")  // SDP offer
    .build()?;
```

### REGISTER
Creates a SIP REGISTER request for registration.

**Required headers:** To, From, Via, Call-ID, CSeq, Contact  
**Optional:** Expires, Authorization, custom headers

```rust
let register = SipBuilder::register()
    .to("sip:bob@proxy.example.com")?
    .from("sip:bob@proxy.example.com")
    .via_tcp("10.0.0.5:5060")
    .call_id("reg-12345@rustisk")
    .cseq(1)
    .contact("sip:bob@10.0.0.5:5060")
    .expires(3600)
    .header("Authorization", "Digest ...")
    .build()?;
```

### BYE
Creates a SIP BYE request for terminating calls.

**Required headers:** To, From, Via, Call-ID, CSeq  
**Optional:** Reason, custom headers

```rust
let bye = SipBuilder::bye()
    .to("sip:alice@example.com;tag=alice-tag")?
    .from("sip:bob@example.com;tag=bob-tag")
    .via_udp("10.0.0.1:5060")
    .call_id("call-12345@asterisk")
    .cseq(2)
    .header("Reason", "SIP;cause=200;text=\"Call completed\"")
    .build()?;
```

### OPTIONS
Creates a SIP OPTIONS request for capability discovery.

**Required headers:** To, From, Via, Call-ID, CSeq  
**Optional:** Allow, User-Agent, custom headers

```rust
let options = SipBuilder::options()
    .to("sip:*")?
    .from("sip:keepalive@monitor.example.com")
    .via_udp("10.0.0.1:5060")
    .call_id_auto()
    .cseq(1)
    .allow(&["INVITE", "ACK", "BYE", "CANCEL", "OPTIONS"])
    .build()?;
```

### ACK
Creates a SIP ACK request for acknowledging responses.

**Required headers:** To, From, Via, Call-ID, CSeq  
**Note:** ACK doesn't include Max-Forwards by default

```rust
let ack = SipBuilder::ack()
    .to("sip:alice@example.com;tag=alice-tag")?
    .from("sip:bob@example.com;tag=bob-tag")
    .via_udp("10.0.0.1:5060")
    .call_id("invite-call@asterisk")
    .cseq(1)  // Same CSeq as the INVITE
    .build()?;
```

### CANCEL
Creates a SIP CANCEL request for canceling pending requests.

**Required headers:** To, From, Via, Call-ID, CSeq  
**Note:** CANCEL doesn't include Max-Forwards by default

```rust
let cancel = SipBuilder::cancel()
    .to("sip:alice@example.com")?  // No tag for CANCEL
    .from("sip:bob@example.com;tag=bob-tag")
    .via_udp("10.0.0.1:5060")
    .call_id("invite-call@asterisk")
    .cseq(1)  // Same CSeq number as original INVITE
    .header("Reason", "SIP;cause=487;text=\"Request Terminated\"")
    .build()?;
```

## Builder Methods

### Core Methods

- `.to(uri: &str)` - Set the To header and request URI
- `.from(uri: &str)` - Set the From header (automatically adds tag)
- `.cseq(seq: u32)` - Set the CSeq header
- `.contact(uri: &str)` - Set the Contact header (required for INVITE/REGISTER)

### Via Headers

- `.via_udp(address: &str)` - Add Via header for UDP transport
- `.via_tcp(address: &str)` - Add Via header for TCP transport  
- `.via_tls(address: &str)` - Add Via header for TLS transport
- `.via_transport(address: &str, transport: Transport)` - Custom transport

### Call-ID

- `.call_id_auto()` - Generate automatic Call-ID with UUID
- `.call_id(call_id: &str)` - Set custom Call-ID

### Content

- `.sdp(content: &str)` - Add SDP body with application/sdp content type
- `.text_body(content: &str)` - Add text body with text/plain content type
- `.body(content: &str, content_type: ContentType)` - Custom content type

### Optional Headers

- `.user_agent(agent: &str)` - Set User-Agent header
- `.allow(methods: &[&str])` - Set Allow header with supported methods
- `.expires(seconds: u32)` - Set Expires header
- `.header(name: &str, value: &str)` - Add custom header

### Build

- `.build()` - Build the final SIP message

## Transport Types

```rust
use asterisk_sip::builder::Transport;

// Available transport types
Transport::UDP   // UDP transport
Transport::TCP   // TCP transport  
Transport::TLS   // TLS transport
Transport::SCTP  // SCTP transport
Transport::WS    // WebSocket
Transport::WSS   // Secure WebSocket
```

## Content Types

```rust
use asterisk_sip::builder::ContentType;

// Built-in content types
ContentType::ApplicationSdp                    // application/sdp
ContentType::TextPlain                         // text/plain
ContentType::ApplicationPidf                  // application/pidf+xml
ContentType::ApplicationDialogInfo            // application/dialog-info+xml
ContentType::ApplicationSimpleMessageSummary // application/simple-message-summary
ContentType::Custom("custom/type".to_string()) // Custom content type
```

## Error Handling

```rust
use asterisk_sip::builder::BuilderError;

match result {
    Ok(message) => println!("Built: {}", message),
    Err(BuilderError::InvalidUri(uri)) => eprintln!("Invalid URI: {}", uri),
    Err(BuilderError::InvalidHeader(header)) => eprintln!("Invalid header: {}", header),
    Err(BuilderError::MissingHeader(header)) => eprintln!("Missing required header: {}", header),
    Err(e) => eprintln!("Other error: {}", e),
}
```

## Compile-Time Safety

The typestate pattern ensures that you cannot build incomplete messages:

```rust
// This will NOT compile - missing required headers
let invalid = SipBuilder::invite()
    .to("sip:alice@example.com").unwrap()
    .build(); // Compilation error!

// This will NOT compile - missing Contact for INVITE
let invalid = SipBuilder::invite()
    .to("sip:alice@example.com").unwrap()
    .from("sip:bob@example.com")
    .via_udp("10.0.0.1:5060")
    .call_id_auto()
    .cseq(1)
    .build(); // Compilation error!
```

## Advanced Examples

### Secure TLS INVITE with SRTP

```rust
let secure_invite = SipBuilder::invite()
    .to("sips:alice@secure.example.com")?
    .from("sips:bob@secure.example.com")
    .via_tls("secure.example.com:5061")
    .call_id_auto()
    .cseq(1)
    .contact("sips:bob@secure.example.com:5061")
    .header("Supported", "100rel,timer,replaces,norefersub")
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
```

### REGISTER with Digest Authentication

```rust
let register = SipBuilder::register()
    .to("sip:bob@proxy.example.com")?
    .from("sip:bob@proxy.example.com")
    .via_tcp("10.0.0.5:5060")
    .call_id("reg-12345@rustisk")
    .cseq(1)
    .contact("sip:bob@10.0.0.5:5060")
    .expires(3600)
    .header("Authorization", 
            "Digest username=\"bob\", realm=\"example.com\", \
             nonce=\"abc123\", uri=\"sip:proxy.example.com\", \
             response=\"def456\"")
    .build()?;
```

### Custom Transport and Headers

```rust
let conference_invite = SipBuilder::invite()
    .to("sip:conference@example.com")?
    .from("sip:organizer@example.com")
    .via_transport("192.168.1.100:5060", Transport::SCTP)
    .call_id_auto()
    .cseq(1)
    .contact("sip:organizer@192.168.1.100:5060")
    .header("Subject", "Weekly Team Meeting")
    .header("Priority", "urgent")
    .header("P-Asserted-Identity", "\"Team Lead\" <sip:lead@example.com>")
    .sdp("v=0\r\n...")
    .build()?;
```

## Integration with Existing Code

The builder produces standard `SipMessage` objects that work with the rest of the asterisk-sip crate:

```rust
let message = SipBuilder::invite()
    .to("sip:alice@example.com")?
    .from("sip:bob@example.com")
    .via_udp("10.0.0.1:5060")
    .call_id_auto()
    .cseq(1)
    .contact("sip:bob@10.0.0.1:5060")
    .build()?;

// Use with existing SIP stack
println!("Call-ID: {}", message.call_id().unwrap_or("none"));
println!("Method: {}", message.method().unwrap_or_default());
println!("SIP Message:\n{}", message);
```
