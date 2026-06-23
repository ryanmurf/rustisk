//! SIP MESSAGE support (port of res_pjsip_messaging.c).
//!
//! Handles incoming SIP MESSAGE requests and provides an API for sending
//! outbound SIP MESSAGE requests. Supports text/plain, text/html, and
//! application/* content types.

use std::net::SocketAddr;

use tracing::info;
use uuid::Uuid;

use crate::parser::{
    extract_uri, header_names, RequestLine, SipHeader, SipMessage, SipMethod, SipUri, StartLine,
    StatusLine,
};

// ---------------------------------------------------------------------------
// SipMessage (the IM payload, not the SIP framing)
// ---------------------------------------------------------------------------

/// Content types supported for SIP MESSAGE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageContentType {
    TextPlain,
    TextHtml,
    /// Generic `application/*` (e.g. `application/json`).
    Application(String),
    /// Any other MIME type as a raw string.
    Other(String),
}

impl MessageContentType {
    /// Parse a Content-Type header value.
    pub fn parse(ct: &str) -> Self {
        let ct = ct.trim().to_lowercase();
        if ct == "text/plain" {
            Self::TextPlain
        } else if ct == "text/html" {
            Self::TextHtml
        } else if ct.starts_with("application/") {
            Self::Application(ct)
        } else {
            Self::Other(ct)
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::TextPlain => "text/plain",
            Self::TextHtml => "text/html",
            Self::Application(s) => s,
            Self::Other(s) => s,
        }
    }
}

/// An instant message carried via SIP MESSAGE.
#[derive(Debug, Clone)]
pub struct InstantMessage {
    /// From URI.
    pub from: String,
    /// To URI.
    pub to: String,
    /// Content-Type of the body.
    pub content_type: MessageContentType,
    /// Message body.
    pub body: String,
    /// Call-ID of the MESSAGE request.
    pub call_id: String,
}

// ---------------------------------------------------------------------------
// Incoming MESSAGE handling
// ---------------------------------------------------------------------------

/// Result of processing an incoming MESSAGE.
#[derive(Debug)]
pub enum MessageResult {
    /// Message accepted; send the included 200 OK response.
    Accepted {
        message: InstantMessage,
        response: SipMessage,
    },
    /// Message rejected with the included error response.
    Rejected { response: SipMessage },
}

/// Check whether a content type is acceptable for an out-of-dialog MESSAGE.
fn is_acceptable_content_type_ood(ct: &str) -> bool {
    let ct = ct.trim().to_lowercase();
    ct == "text/plain"
}

/// Check whether a content type is acceptable for an in-dialog MESSAGE.
fn is_acceptable_content_type_dialog(ct: &str) -> bool {
    let ct = ct.trim().to_lowercase();
    ct.starts_with("text/") || ct.starts_with("application/")
}

/// Process an incoming SIP MESSAGE request (out-of-dialog).
///
/// Returns a `MessageResult` that includes the parsed instant message
/// and the SIP response to send.
pub fn handle_incoming_message(request: &SipMessage) -> MessageResult {
    if request.method() != Some(SipMethod::Message) {
        return MessageResult::Rejected {
            response: make_error(request, 405, "Method Not Allowed"),
        };
    }

    let content_type_str = request
        .get_header(header_names::CONTENT_TYPE)
        .unwrap_or("text/plain");

    if !is_acceptable_content_type_ood(content_type_str) && !is_acceptable_content_type_dialog(content_type_str) {
        return MessageResult::Rejected {
            response: make_error(request, 415, "Unsupported Media Type"),
        };
    }

    if request.body.is_empty() {
        // Per RFC 3428, an empty MESSAGE body is acceptable but there is nothing to deliver.
        let response = request
            .create_response(200, "OK")
            .unwrap_or_else(|_| make_error(request, 500, "Internal Server Error"));
        return MessageResult::Accepted {
            message: InstantMessage {
                from: extract_from(request),
                to: extract_to(request),
                content_type: MessageContentType::parse(content_type_str),
                body: String::new(),
                call_id: request.call_id().unwrap_or("").to_string(),
            },
            response,
        };
    }

    let from = extract_from(request);
    let to = extract_to(request);
    let call_id = request.call_id().unwrap_or("").to_string();

    let message = InstantMessage {
        from,
        to,
        content_type: MessageContentType::parse(content_type_str),
        body: request.body.clone(),
        call_id,
    };

    let response = request
        .create_response(200, "OK")
        .unwrap_or_else(|_| make_error(request, 500, "Internal Server Error"));

    info!(
        from = %message.from,
        to = %message.to,
        content_type = %content_type_str,
        body_len = message.body.len(),
        "Received SIP MESSAGE"
    );

    MessageResult::Accepted { message, response }
}

/// Process an incoming SIP MESSAGE within an existing dialog.
pub fn handle_incoming_message_in_dialog(request: &SipMessage) -> MessageResult {
    if request.method() != Some(SipMethod::Message) {
        return MessageResult::Rejected {
            response: make_error(request, 405, "Method Not Allowed"),
        };
    }

    let content_type_str = request
        .get_header(header_names::CONTENT_TYPE)
        .unwrap_or("text/plain");

    if !is_acceptable_content_type_dialog(content_type_str) {
        return MessageResult::Rejected {
            response: make_error(request, 415, "Unsupported Media Type"),
        };
    }

    let message = InstantMessage {
        from: extract_from(request),
        to: extract_to(request),
        content_type: MessageContentType::parse(content_type_str),
        body: request.body.clone(),
        call_id: request.call_id().unwrap_or("").to_string(),
    };

    let response = request
        .create_response(200, "OK")
        .unwrap_or_else(|_| make_error(request, 500, "Internal Server Error"));

    MessageResult::Accepted { message, response }
}

// ---------------------------------------------------------------------------
// Outbound MESSAGE building
// ---------------------------------------------------------------------------

/// Build an outbound SIP MESSAGE request.
pub fn build_message(
    from_uri: &str,
    to_uri: &str,
    body: &str,
    content_type: &MessageContentType,
    local_addr: SocketAddr,
) -> SipMessage {
    let call_id = format!("msg-{}@{}", Uuid::new_v4(), local_addr.ip());
    let from_tag = Uuid::new_v4().to_string()[..8].to_string();
    let branch = format!(
        "z9hG4bK{}",
        &Uuid::new_v4().to_string().replace('-', "")[..16]
    );

    let request_uri = SipUri::parse(to_uri).unwrap_or_else(|_| SipUri {
        scheme: "sip".to_string(),
        user: None,
        password: None,
        host: "localhost".to_string(),
        port: Some(5060),
        parameters: Default::default(),
        headers: Default::default(),
    });

    let headers = vec![
        SipHeader {
            name: header_names::VIA.to_string(),
            value: format!("SIP/2.0/UDP {};branch={}", local_addr, branch),
        },
        SipHeader {
            name: header_names::MAX_FORWARDS.to_string(),
            value: "70".to_string(),
        },
        SipHeader {
            name: header_names::FROM.to_string(),
            value: format!("<{}>;tag={}", from_uri, from_tag),
        },
        SipHeader {
            name: header_names::TO.to_string(),
            value: format!("<{}>", to_uri),
        },
        SipHeader {
            name: header_names::CALL_ID.to_string(),
            value: call_id,
        },
        SipHeader {
            name: header_names::CSEQ.to_string(),
            value: "1 MESSAGE".to_string(),
        },
        SipHeader {
            name: header_names::CONTENT_TYPE.to_string(),
            value: content_type.as_str().to_string(),
        },
        SipHeader {
            name: header_names::CONTENT_LENGTH.to_string(),
            value: body.len().to_string(),
        },
        SipHeader {
            name: header_names::USER_AGENT.to_string(),
            value: "Rustisk/0.1.0".to_string(),
        },
    ];

    SipMessage {
        start_line: StartLine::Request(RequestLine {
            method: SipMethod::Message,
            uri: request_uri,
            version: "SIP/2.0".to_string(),
        }),
        headers,
        body: body.to_string(),
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn extract_from(msg: &SipMessage) -> String {
    msg.from_header()
        .and_then(extract_uri)
        .unwrap_or_default()
}

fn extract_to(msg: &SipMessage) -> String {
    msg.to_header()
        .and_then(extract_uri)
        .unwrap_or_default()
}

fn make_error(request: &SipMessage, code: u16, reason: &str) -> SipMessage {
    request
        .create_response(code, reason)
        .unwrap_or_else(|_| SipMessage {
            start_line: StartLine::Response(StatusLine {
                version: "SIP/2.0".to_string(),
                status_code: code,
                reason_phrase: reason.to_string(),
            }),
            headers: Vec::new(),
            body: String::new(),
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_incoming_message() {
        let msg = SipMessage::parse(
            b"MESSAGE sip:bob@example.com SIP/2.0\r\n\
              Via: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123\r\n\
              From: Alice <sip:alice@example.com>;tag=abc\r\n\
              To: Bob <sip:bob@example.com>\r\n\
              Call-ID: msg-test-123\r\n\
              CSeq: 1 MESSAGE\r\n\
              Content-Type: text/plain\r\n\
              Content-Length: 5\r\n\
              \r\n\
              Hello",
        )
        .unwrap();

        match handle_incoming_message(&msg) {
            MessageResult::Accepted { message, response } => {
                assert_eq!(message.body, "Hello");
                assert_eq!(message.content_type, MessageContentType::TextPlain);
                assert_eq!(response.status_code(), Some(200));
            }
            _ => panic!("Expected Accepted"),
        }
    }

    #[test]
    fn test_build_outbound_message() {
        let msg = build_message(
            "sip:alice@example.com",
            "sip:bob@example.com",
            "Hi there",
            &MessageContentType::TextPlain,
            "10.0.0.1:5060".parse().unwrap(),
        );

        assert_eq!(msg.method(), Some(SipMethod::Message));
        assert_eq!(msg.body, "Hi there");
        assert_eq!(
            msg.get_header(header_names::CONTENT_TYPE),
            Some("text/plain")
        );
    }
}
