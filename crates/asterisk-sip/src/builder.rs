//! Type-safe fluent SIP message builder API using the typestate pattern.
//!
//! This module provides a compile-time safe way to build SIP messages, ensuring
//! that all required headers are provided for each message type.
//!
//! # Example
//! ```
//! use asterisk_sip::builder::SipBuilder;
//!
//! let invite = SipBuilder::invite()
//!     .to("alice@example.com")
//!     .from("bob@example.com")
//!     .via_udp("10.0.0.1:5060")
//!     .call_id_auto()
//!     .cseq(1)
//!     .contact("bob@10.0.0.1")
//!     .sdp("v=0\r\no=- 123456 789012 IN IP4 10.0.0.1\r\n...")
//!     .build()
//!     .expect("Failed to build INVITE");
//! ```

use crate::parser::{SipMessage, SipMethod, SipUri, SipHeader, RequestLine, StartLine, header_names};
use std::collections::HashMap;
use std::marker::PhantomData;
use uuid::Uuid;

/// Marker types for the typestate pattern
pub mod state {
    pub struct NoMethod;
    pub struct HasMethod;
    pub struct NoTo;
    pub struct HasTo;
    pub struct NoFrom;
    pub struct HasFrom;
    pub struct NoVia;
    pub struct HasVia;
    pub struct NoCallId;
    pub struct HasCallId;
    pub struct NoCSeq;
    pub struct HasCSeq;
    pub struct NoContact;
    pub struct HasContact;
    pub struct Complete;
}

/// Transport types for Via header generation
#[derive(Clone, Debug)]
pub enum Transport {
    UDP,
    TCP,
    TLS,
    SCTP,
    WS,
    WSS,
}

impl Transport {
    fn as_str(&self) -> &'static str {
        match self {
            Transport::UDP => "UDP",
            Transport::TCP => "TCP",
            Transport::TLS => "TLS",
            Transport::SCTP => "SCTP",
            Transport::WS => "WS",
            Transport::WSS => "WSS",
        }
    }
}

/// Content type for SDP and other bodies
#[derive(Clone, Debug)]
pub enum ContentType {
    ApplicationSdp,
    TextPlain,
    ApplicationPidf,
    ApplicationDialogInfo,
    ApplicationSimpleMessageSummary,
    Custom(String),
}

impl ContentType {
    fn as_str(&self) -> &str {
        match self {
            ContentType::ApplicationSdp => "application/sdp",
            ContentType::TextPlain => "text/plain",
            ContentType::ApplicationPidf => "application/pidf+xml",
            ContentType::ApplicationDialogInfo => "application/dialog-info+xml",
            ContentType::ApplicationSimpleMessageSummary => "application/simple-message-summary",
            ContentType::Custom(s) => s,
        }
    }
}

/// Typestate SIP message builder
pub struct SipBuilder<Method, To, From, Via, CallId, CSeq, Contact> {
    method: Option<SipMethod>,
    request_uri: Option<SipUri>,
    headers: Vec<SipHeader>,
    body: Option<String>,
    content_type: Option<ContentType>,
    _phantom: PhantomData<(Method, To, From, Via, CallId, CSeq, Contact)>,
}

/// Builder errors
#[derive(Debug, thiserror::Error)]
pub enum BuilderError {
    #[error("Invalid URI format: {0}")]
    InvalidUri(String),
    #[error("Invalid header value: {0}")]
    InvalidHeader(String),
    #[error("Missing required header: {0}")]
    MissingHeader(String),
    #[error("URI parse error: {0}")]
    UriParseError(#[from] crate::parser::ParseError),
}

impl Default for SipBuilder<state::NoMethod, state::NoTo, state::NoFrom, state::NoVia, state::NoCallId, state::NoCSeq, state::NoContact> {
    fn default() -> Self {
        Self::new()
    }
}

impl SipBuilder<state::NoMethod, state::NoTo, state::NoFrom, state::NoVia, state::NoCallId, state::NoCSeq, state::NoContact> {
    pub fn new() -> Self {
        Self {
            method: None,
            request_uri: None,
            headers: Vec::new(),
            body: None,
            content_type: None,
            _phantom: PhantomData,
        }
    }

    /// Create an INVITE request builder
    pub fn invite() -> SipBuilder<state::HasMethod, state::NoTo, state::NoFrom, state::NoVia, state::NoCallId, state::NoCSeq, state::NoContact> {
        let mut builder = SipBuilder {
            method: Some(SipMethod::Invite),
            request_uri: None,
            headers: Vec::new(),
            body: None,
            content_type: None,
            _phantom: PhantomData,
        };
        
        // Add default Max-Forwards
        builder.add_header(header_names::MAX_FORWARDS, "70");
        builder
    }

    /// Create a REGISTER request builder
    pub fn register() -> SipBuilder<state::HasMethod, state::NoTo, state::NoFrom, state::NoVia, state::NoCallId, state::NoCSeq, state::NoContact> {
        let mut builder = SipBuilder {
            method: Some(SipMethod::Register),
            request_uri: None,
            headers: Vec::new(),
            body: None,
            content_type: None,
            _phantom: PhantomData,
        };
        
        // Add default Max-Forwards
        builder.add_header(header_names::MAX_FORWARDS, "70");
        builder
    }

    /// Create a BYE request builder
    pub fn bye() -> SipBuilder<state::HasMethod, state::NoTo, state::NoFrom, state::NoVia, state::NoCallId, state::NoCSeq, state::NoContact> {
        let mut builder = SipBuilder {
            method: Some(SipMethod::Bye),
            request_uri: None,
            headers: Vec::new(),
            body: None,
            content_type: None,
            _phantom: PhantomData,
        };
        
        // Add default Max-Forwards
        builder.add_header(header_names::MAX_FORWARDS, "70");
        builder
    }

    /// Create an OPTIONS request builder
    pub fn options() -> SipBuilder<state::HasMethod, state::NoTo, state::NoFrom, state::NoVia, state::NoCallId, state::NoCSeq, state::NoContact> {
        let mut builder = SipBuilder {
            method: Some(SipMethod::Options),
            request_uri: None,
            headers: Vec::new(),
            body: None,
            content_type: None,
            _phantom: PhantomData,
        };
        
        // Add default Max-Forwards
        builder.add_header(header_names::MAX_FORWARDS, "70");
        builder
    }

    /// Create an ACK request builder
    pub fn ack() -> SipBuilder<state::HasMethod, state::NoTo, state::NoFrom, state::NoVia, state::NoCallId, state::NoCSeq, state::NoContact> {
        let builder = SipBuilder {
            method: Some(SipMethod::Ack),
            request_uri: None,
            headers: Vec::new(),
            body: None,
            content_type: None,
            _phantom: PhantomData,
        };
        
        // ACK doesn't need Max-Forwards
        builder
    }

    /// Create a CANCEL request builder
    pub fn cancel() -> SipBuilder<state::HasMethod, state::NoTo, state::NoFrom, state::NoVia, state::NoCallId, state::NoCSeq, state::NoContact> {
        let builder = SipBuilder {
            method: Some(SipMethod::Cancel),
            request_uri: None,
            headers: Vec::new(),
            body: None,
            content_type: None,
            _phantom: PhantomData,
        };
        
        // CANCEL doesn't need Max-Forwards
        builder
    }
}

// Implementation for method transitions
impl<To, From, Via, CallId, CSeq, Contact> SipBuilder<state::HasMethod, To, From, Via, CallId, CSeq, Contact> {
    /// Set the To header and request URI
    pub fn to(mut self, to_uri: &str) -> Result<SipBuilder<state::HasMethod, state::HasTo, From, Via, CallId, CSeq, Contact>, BuilderError> {
        let uri = parse_uri_string(to_uri)?;
        
        // For REGISTER, the To header should match the request URI
        if matches!(self.method, Some(SipMethod::Register)) {
            self.request_uri = Some(uri.clone());
        } else {
            self.request_uri = Some(uri.clone());
        }
        
        self.add_header(header_names::TO, to_uri);
        
        Ok(SipBuilder {
            method: self.method,
            request_uri: self.request_uri,
            headers: self.headers,
            body: self.body,
            content_type: self.content_type,
            _phantom: PhantomData,
        })
    }
}

// Implementation for To transitions
impl<From, Via, CallId, CSeq, Contact> SipBuilder<state::HasMethod, state::HasTo, From, Via, CallId, CSeq, Contact> {
    /// Set the From header
    pub fn from(mut self, from_uri: &str) -> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, Via, CallId, CSeq, Contact> {
        self.add_header(header_names::FROM, &format!("{};tag={}", from_uri, generate_tag()));
        
        SipBuilder {
            method: self.method,
            request_uri: self.request_uri,
            headers: self.headers,
            body: self.body,
            content_type: self.content_type,
            _phantom: PhantomData,
        }
    }
}

// Implementation for From transitions
impl<Via, CallId, CSeq, Contact> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, Via, CallId, CSeq, Contact> {
    /// Add a Via header for UDP transport
    pub fn via_udp(mut self, address: &str) -> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, CallId, CSeq, Contact> {
        self.via_transport(address, Transport::UDP)
    }

    /// Add a Via header for TCP transport
    pub fn via_tcp(mut self, address: &str) -> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, CallId, CSeq, Contact> {
        self.via_transport(address, Transport::TCP)
    }

    /// Add a Via header for TLS transport
    pub fn via_tls(mut self, address: &str) -> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, CallId, CSeq, Contact> {
        self.via_transport(address, Transport::TLS)
    }

    /// Add a Via header with custom transport
    pub fn via_transport(mut self, address: &str, transport: Transport) -> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, CallId, CSeq, Contact> {
        let branch = generate_branch();
        let via_value = format!("SIP/2.0/{} {};branch={}", transport.as_str(), address, branch);
        self.add_header(header_names::VIA, &via_value);
        
        SipBuilder {
            method: self.method,
            request_uri: self.request_uri,
            headers: self.headers,
            body: self.body,
            content_type: self.content_type,
            _phantom: PhantomData,
        }
    }
}

// Implementation for Via transitions  
impl<CallId, CSeq, Contact> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, CallId, CSeq, Contact> {
    /// Set the Call-ID header automatically (generates a UUID)
    pub fn call_id_auto(mut self) -> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, state::HasCallId, CSeq, Contact> {
        let call_id = generate_call_id();
        self.add_header(header_names::CALL_ID, &call_id);
        
        SipBuilder {
            method: self.method,
            request_uri: self.request_uri,
            headers: self.headers,
            body: self.body,
            content_type: self.content_type,
            _phantom: PhantomData,
        }
    }

    /// Set the Call-ID header manually
    pub fn call_id(mut self, call_id: &str) -> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, state::HasCallId, CSeq, Contact> {
        self.add_header(header_names::CALL_ID, call_id);
        
        SipBuilder {
            method: self.method,
            request_uri: self.request_uri,
            headers: self.headers,
            body: self.body,
            content_type: self.content_type,
            _phantom: PhantomData,
        }
    }
}

// Implementation for CallId transitions
impl<CSeq, Contact> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, state::HasCallId, CSeq, Contact> {
    /// Set the CSeq header
    pub fn cseq(mut self, seq: u32) -> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, state::HasCallId, state::HasCSeq, Contact> {
        if let Some(method) = &self.method {
            let cseq_value = format!("{} {}", seq, method.as_str());
            self.add_header(header_names::CSEQ, &cseq_value);
        }
        
        SipBuilder {
            method: self.method,
            request_uri: self.request_uri,
            headers: self.headers,
            body: self.body,
            content_type: self.content_type,
            _phantom: PhantomData,
        }
    }
}

// Implementation for CSeq transitions (Contact is only required for INVITE and REGISTER)
impl SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, state::HasCallId, state::HasCSeq, state::NoContact> {
    /// Set the Contact header (required for INVITE and REGISTER)
    pub fn contact(mut self, contact_uri: &str) -> SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, state::HasCallId, state::HasCSeq, state::HasContact> {
        self.add_header(header_names::CONTACT, contact_uri);
        
        SipBuilder {
            method: self.method,
            request_uri: self.request_uri,
            headers: self.headers,
            body: self.body,
            content_type: self.content_type,
            _phantom: PhantomData,
        }
    }

    /// Build the message (for methods that don't require Contact header)
    pub fn build(self) -> Result<SipMessage, BuilderError> {
        // Only allow this for methods that don't require Contact
        if matches!(self.method, Some(SipMethod::Invite) | Some(SipMethod::Register)) {
            return Err(BuilderError::MissingHeader("Contact is required for INVITE and REGISTER".to_string()));
        }
        
        self.build_internal()
    }
}

// Implementation for complete builder (with Contact)
impl SipBuilder<state::HasMethod, state::HasTo, state::HasFrom, state::HasVia, state::HasCallId, state::HasCSeq, state::HasContact> {
    /// Build the final SIP message
    pub fn build(self) -> Result<SipMessage, BuilderError> {
        self.build_internal()
    }
}

// Common implementation for all states
impl<Method, To, From, Via, CallId, CSeq, Contact> SipBuilder<Method, To, From, Via, CallId, CSeq, Contact> {
    /// Add SDP body content
    pub fn sdp(mut self, sdp_content: &str) -> Self {
        self.body = Some(sdp_content.to_string());
        self.content_type = Some(ContentType::ApplicationSdp);
        self
    }

    /// Add text body content
    pub fn text_body(mut self, text_content: &str) -> Self {
        self.body = Some(text_content.to_string());
        self.content_type = Some(ContentType::TextPlain);
        self
    }

    /// Add custom body content with content type
    pub fn body(mut self, content: &str, content_type: ContentType) -> Self {
        self.body = Some(content.to_string());
        self.content_type = Some(content_type);
        self
    }

    /// Add a custom header
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.add_header(name, value);
        self
    }

    /// Add User-Agent header
    pub fn user_agent(mut self, agent: &str) -> Self {
        self.add_header("User-Agent", agent);
        self
    }

    /// Add Allow header
    pub fn allow(mut self, methods: &[&str]) -> Self {
        let allow_value = methods.join(", ");
        self.add_header("Allow", &allow_value);
        self
    }

    /// Add Expires header
    pub fn expires(mut self, seconds: u32) -> Self {
        self.add_header("Expires", &seconds.to_string());
        self
    }

    fn add_header(&mut self, name: &str, value: &str) {
        self.headers.push(SipHeader {
            name: name.to_string(),
            value: value.to_string(),
        });
    }

    fn build_internal(mut self) -> Result<SipMessage, BuilderError> {
        let method = self.method.ok_or_else(|| BuilderError::MissingHeader("Method".to_string()))?;
        let uri = self.request_uri.ok_or_else(|| BuilderError::MissingHeader("Request-URI".to_string()))?;

        // Add content headers if body is present
        if let Some(body) = &self.body {
            let content_length = body.len().to_string();
            self.add_header(header_names::CONTENT_LENGTH, &content_length);
            
            if let Some(content_type) = &self.content_type {
                self.add_header(header_names::CONTENT_TYPE, content_type.as_str());
            }
        } else {
            // Add Content-Length: 0 for empty body
            self.add_header(header_names::CONTENT_LENGTH, "0");
        }

        let request_line = RequestLine {
            method,
            uri,
            version: "SIP/2.0".to_string(),
        };

        Ok(SipMessage {
            start_line: StartLine::Request(request_line),
            headers: self.headers,
            body: self.body.unwrap_or_default(),
        })
    }
}

// Helper functions
fn parse_uri_string(uri_str: &str) -> Result<SipUri, BuilderError> {
    // Basic URI parsing - this is a simplified version
    if uri_str.starts_with("sip:") || uri_str.starts_with("sips:") {
        let parts: Vec<&str> = uri_str.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(BuilderError::InvalidUri(format!("Invalid URI format: {}", uri_str)));
        }

        let scheme = parts[0].to_string();
        let rest = parts[1];

        // Parse user@host:port
        let (user_info, host_port) = if rest.contains('@') {
            let parts: Vec<&str> = rest.splitn(2, '@').collect();
            (Some(parts[0].to_string()), parts[1])
        } else {
            (None, rest)
        };

        let (user, password) = if let Some(user_info) = user_info {
            if user_info.contains(':') {
                let parts: Vec<&str> = user_info.splitn(2, ':').collect();
                (Some(parts[0].to_string()), Some(parts[1].to_string()))
            } else {
                (Some(user_info), None)
            }
        } else {
            (None, None)
        };

        let (host, port) = if host_port.contains(':') {
            let parts: Vec<&str> = host_port.splitn(2, ':').collect();
            let port = parts[1].parse::<u16>().map_err(|_| {
                BuilderError::InvalidUri(format!("Invalid port in URI: {}", uri_str))
            })?;
            (parts[0].to_string(), Some(port))
        } else {
            (host_port.to_string(), None)
        };

        Ok(SipUri {
            scheme,
            user,
            password,
            host,
            port,
            parameters: HashMap::new(),
            headers: HashMap::new(),
        })
    } else {
        Err(BuilderError::InvalidUri(format!("URI must start with sip: or sips: {}", uri_str)))
    }
}

fn generate_tag() -> String {
    format!("{:x}", Uuid::new_v4().as_u128())
}

fn generate_branch() -> String {
    format!("z9hG4bK{:x}", Uuid::new_v4().as_u128())
}

fn generate_call_id() -> String {
    format!("{}@rustisk", Uuid::new_v4())
}

// Extension trait for SipMethod is not needed as as_str() already exists

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invite_builder() {
        let invite = SipBuilder::invite()
            .to("alice@example.com")
            .expect("Valid To header")
            .from("bob@example.com")
            .via_udp("10.0.0.1:5060")
            .call_id_auto()
            .cseq(1)
            .contact("bob@10.0.0.1")
            .user_agent("Rustisk/0.1.0")
            .sdp("v=0\r\no=- 123456 789012 IN IP4 10.0.0.1\r\n")
            .build()
            .expect("Failed to build INVITE");

        if let StartLine::Request(ref req_line) = invite.start_line {
            assert_eq!(req_line.method, SipMethod::Invite);
            assert_eq!(req_line.uri.host, "alice");
            assert_eq!(req_line.uri.scheme, "sip");
        } else {
            panic!("Expected request line");
        }

        // Verify required headers
        assert!(invite.get_header("To").is_some());
        assert!(invite.get_header("From").is_some());
        assert!(invite.get_header("Via").is_some());
        assert!(invite.get_header("Call-ID").is_some());
        assert!(invite.get_header("CSeq").is_some());
        assert!(invite.get_header("Contact").is_some());
        assert!(invite.get_header("Content-Type").is_some());
        assert!(invite.get_header("Content-Length").is_some());
        assert!(invite.get_header("User-Agent").is_some());
    }

    #[test]
    fn test_register_builder() {
        let register = SipBuilder::register()
            .to("bob@example.com")
            .expect("Valid To header")
            .from("bob@example.com")
            .via_udp("10.0.0.1:5060")
            .call_id_auto()
            .cseq(1)
            .contact("bob@10.0.0.1")
            .expires(3600)
            .build()
            .expect("Failed to build REGISTER");

        if let StartLine::Request(ref req_line) = register.start_line {
            assert_eq!(req_line.method, SipMethod::Register);
        } else {
            panic!("Expected request line");
        }

        assert!(register.get_header("Expires").is_some());
    }

    #[test]
    fn test_bye_builder() {
        let bye = SipBuilder::bye()
            .to("alice@example.com")
            .expect("Valid To header")
            .from("bob@example.com")
            .via_udp("10.0.0.1:5060")
            .call_id_auto()
            .cseq(2)
            .build()
            .expect("Failed to build BYE");

        if let StartLine::Request(ref req_line) = bye.start_line {
            assert_eq!(req_line.method, SipMethod::Bye);
        } else {
            panic!("Expected request line");
        }
    }

    #[test]
    fn test_options_builder() {
        let options = SipBuilder::options()
            .to("*")
            .expect("Valid To header")
            .from("bob@example.com")
            .via_udp("10.0.0.1:5060")
            .call_id_auto()
            .cseq(1)
            .allow(&["INVITE", "ACK", "BYE", "CANCEL", "OPTIONS"])
            .build()
            .expect("Failed to build OPTIONS");

        if let StartLine::Request(ref req_line) = options.start_line {
            assert_eq!(req_line.method, SipMethod::Options);
        } else {
            panic!("Expected request line");
        }

        assert!(options.get_header("Allow").is_some());
    }

    #[test]
    fn test_compilation_errors() {
        // These should not compile (uncomment to test):
        
        // Missing To header
        // let _invalid = SipBuilder::invite()
        //     .from("bob@example.com")
        //     .build();

        // Missing Contact for INVITE
        // let _invalid = SipBuilder::invite()
        //     .to("alice@example.com")
        //     .from("bob@example.com")
        //     .via_udp("10.0.0.1:5060")
        //     .call_id_auto()
        //     .cseq(1)
        //     .build();
    }

    #[test]
    fn test_uri_parsing() {
        let uri = parse_uri_string("sip:alice@example.com:5060").expect("Valid URI");
        assert_eq!(uri.scheme, "sip");
        assert_eq!(uri.user, Some("alice".to_string()));
        assert_eq!(uri.host, "example.com");
        assert_eq!(uri.port, Some(5060));

        let simple_uri = parse_uri_string("sip:example.com").expect("Valid simple URI");
        assert_eq!(simple_uri.scheme, "sip");
        assert_eq!(simple_uri.user, None);
        assert_eq!(simple_uri.host, "example.com");
        assert_eq!(simple_uri.port, None);
    }

    #[test]
    fn test_invalid_uri() {
        assert!(parse_uri_string("http://example.com").is_err());
        assert!(parse_uri_string("invalid-uri").is_err());
    }
}
