//! SIP session management.
//!
//! A SipSession represents an INVITE dialog/media session. It manages
//! the full lifecycle: INVITE -> 1xx -> 200 OK -> ACK -> BYE.

use std::net::SocketAddr;

use tracing::{debug, info};
use uuid::Uuid;

use crate::dialog::Dialog;
use crate::parser::{extract_tag, extract_uri, SipMessage, SipMethod, SipUri, StartLine, RequestLine, SipHeader, header_names};
use crate::rtp::RtpSession;
use crate::sdp::SessionDescription;

/// Session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// INVITE sent/received, waiting for response.
    Initiated,
    /// 1xx received, ringing.
    Early,
    /// 200 OK received/sent, media flowing.
    Established,
    /// BYE sent/received.
    Terminating,
    /// Session ended.
    Terminated,
}

/// Configuration for early media fork handling.
#[derive(Debug, Clone)]
pub struct EarlyMediaConfig {
    /// Whether to accept early media from any fork.
    pub follow_early_media_fork: bool,
    /// Whether to accept multiple SDP answers from different forks.
    pub accept_multiple_sdp_answers: bool,
}

impl Default for EarlyMediaConfig {
    fn default() -> Self {
        Self {
            follow_early_media_fork: true,
            accept_multiple_sdp_answers: false,
        }
    }
}

/// State tracking for early media from forked INVITEs.
#[derive(Debug, Clone)]
#[derive(Default)]
pub struct EarlyMediaState {
    /// Whether the INVITE was forked (1xx from multiple UASs).
    pub forked: bool,
    /// URIs of UASs that have sent provisional responses.
    pub forked_from: Vec<SipUri>,
    /// Index of the currently selected fork for media.
    pub selected_fork: Option<usize>,
    /// To-tags from different forks (for dialog disambiguation).
    pub fork_tags: Vec<String>,
}


impl EarlyMediaState {
    /// Record a provisional response from a fork.
    ///
    /// Returns `true` if this is a new fork (not seen before).
    pub fn on_provisional(&mut self, to_tag: &str, contact_uri: Option<&SipUri>) -> bool {
        if self.fork_tags.contains(&to_tag.to_string()) {
            return false;
        }

        self.fork_tags.push(to_tag.to_string());
        if let Some(uri) = contact_uri {
            self.forked_from.push(uri.clone());
        }

        if self.fork_tags.len() > 1 {
            self.forked = true;
        }

        // Select the first fork by default.
        if self.selected_fork.is_none() {
            self.selected_fork = Some(0);
        }

        true
    }

    /// Select a specific fork for early media.
    pub fn select_fork(&mut self, index: usize) {
        if index < self.fork_tags.len() {
            self.selected_fork = Some(index);
        }
    }

    /// Check if a given To-tag is the currently selected fork.
    pub fn is_selected_fork(&self, to_tag: &str) -> bool {
        match self.selected_fork {
            Some(idx) => self.fork_tags.get(idx).map(|t| t.as_str()) == Some(to_tag),
            None => false,
        }
    }

    /// Get the number of forks detected.
    pub fn fork_count(&self) -> usize {
        self.fork_tags.len()
    }
}

/// A SIP media session.
#[derive(Debug)]
pub struct SipSession {
    /// Unique session identifier.
    pub id: String,
    /// Current session state.
    pub state: SessionState,
    /// The underlying SIP dialog.
    pub dialog: Option<Dialog>,
    /// Local SDP description.
    pub local_sdp: Option<SessionDescription>,
    /// The initial local SDP answer (before any re-INVITEs), used by SFU.
    pub initial_local_sdp: Option<SessionDescription>,
    /// Remote SDP description.
    pub remote_sdp: Option<SessionDescription>,
    /// RTP session for media.
    pub rtp: Option<RtpSession>,
    /// Local SIP address.
    pub local_addr: SocketAddr,
    /// Remote SIP address.
    pub remote_addr: SocketAddr,
    /// The original INVITE request (for reference).
    pub invite: Option<SipMessage>,
    /// Whether we are the caller (UAC) or callee (UAS).
    pub is_outbound: bool,
    /// Call-ID for this session.
    pub call_id: String,
    /// Our From tag.
    pub local_tag: String,
    /// Early media fork state.
    pub early_media: EarlyMediaState,
    /// Early media configuration.
    pub early_media_config: EarlyMediaConfig,
}

impl SipSession {
    /// Create a new outbound session.
    pub fn new_outbound(local_addr: SocketAddr, remote_addr: SocketAddr) -> Self {
        let id = Uuid::new_v4().to_string();
        let call_id = format!("{}@{}", Uuid::new_v4(), local_addr.ip());
        let local_tag = Uuid::new_v4().to_string()[..8].to_string();

        Self {
            id,
            state: SessionState::Initiated,
            dialog: None,
            local_sdp: None,
            initial_local_sdp: None,
            remote_sdp: None,
            rtp: None,
            local_addr,
            remote_addr,
            invite: None,
            is_outbound: true,
            call_id,
            local_tag,
            early_media: EarlyMediaState::default(),
            early_media_config: EarlyMediaConfig::default(),
        }
    }

    /// Create a new inbound session from a received INVITE.
    pub fn new_inbound(
        invite: &SipMessage,
        local_addr: SocketAddr,
        remote_addr: SocketAddr,
    ) -> Option<Self> {
        let call_id = invite.call_id()?.to_string();
        let local_tag = Uuid::new_v4().to_string()[..8].to_string();

        let dialog = Dialog::from_uas_request(invite, &local_tag);

        // Parse SDP from body if present.
        let remote_sdp = if !invite.body.is_empty() {
            SessionDescription::parse(&invite.body).ok()
        } else {
            None
        };

        Some(Self {
            id: Uuid::new_v4().to_string(),
            state: SessionState::Initiated,
            dialog,
            local_sdp: None,
            initial_local_sdp: None,
            remote_sdp,
            rtp: None,
            local_addr,
            remote_addr,
            invite: Some(invite.clone()),
            is_outbound: false,
            call_id,
            local_tag,
            early_media: EarlyMediaState::default(),
            early_media_config: EarlyMediaConfig::default(),
        })
    }

    /// Build an INVITE request for an outbound session.
    pub fn build_invite(&mut self, to_uri: &str) -> SipMessage {
        self.build_invite_with_uri(to_uri, to_uri)
    }

    /// Build an INVITE with separate Request-URI and To header value.
    /// The request_uri is used as the actual SIP Request-URI (typically the
    /// contact address), while to_uri is used in the To header.
    pub fn build_invite_with_uri(&mut self, request_uri: &str, to_uri: &str) -> SipMessage {
        let from_uri = format!("sip:asterisk@{}", self.local_addr);
        let contact_uri = format!("sip:asterisk@{}", self.local_addr);
        let branch = format!("z9hG4bK{}", &Uuid::new_v4().to_string().replace('-', "")[..16]);

        let uri = SipUri::parse(request_uri).unwrap_or_else(|_| SipUri {
            scheme: "sip".to_string(),
            user: None,
            password: None,
            host: self.remote_addr.ip().to_string(),
            port: Some(self.remote_addr.port()),
            parameters: Default::default(),
            headers: Default::default(),
        });

        let sdp_body = self.local_sdp.as_ref().map(|s| s.to_string()).unwrap_or_default();
        let content_length = sdp_body.len();

        let mut headers = vec![
            SipHeader { name: header_names::VIA.to_string(), value: format!("SIP/2.0/UDP {};branch={}", self.local_addr, branch) },
            SipHeader { name: header_names::MAX_FORWARDS.to_string(), value: "70".to_string() },
            SipHeader { name: header_names::FROM.to_string(), value: format!("<{}>;tag={}", from_uri, self.local_tag) },
            SipHeader { name: header_names::TO.to_string(), value: format!("<{}>", to_uri) },
            SipHeader { name: header_names::CALL_ID.to_string(), value: self.call_id.clone() },
            SipHeader { name: header_names::CSEQ.to_string(), value: "1 INVITE".to_string() },
            SipHeader { name: header_names::CONTACT.to_string(), value: format!("<{}>", contact_uri) },
            SipHeader { name: header_names::USER_AGENT.to_string(), value: "Rustisk/0.1.0".to_string() },
            SipHeader { name: header_names::ALLOW.to_string(), value: "INVITE, ACK, CANCEL, BYE, OPTIONS, REFER, NOTIFY".to_string() },
        ];

        if !sdp_body.is_empty() {
            headers.push(SipHeader { name: header_names::CONTENT_TYPE.to_string(), value: "application/sdp".to_string() });
        }
        headers.push(SipHeader { name: header_names::CONTENT_LENGTH.to_string(), value: content_length.to_string() });

        let msg = SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Invite,
                uri,
                version: "SIP/2.0".to_string(),
            }),
            headers,
            body: sdp_body,
        };

        self.invite = Some(msg.clone());
        msg
    }

    /// Process a response to our INVITE.
    pub fn on_response(&mut self, response: &SipMessage) {
        let status = response.status_code().unwrap_or(0);

        match status {
            100..=199 => {
                self.state = SessionState::Early;

                // Track early media forks: detect 1xx from different UASs via To-tag
                if let Some(to_hdr) = response.to_header() {
                    if let Some(to_tag) = extract_tag(to_hdr) {
                        let contact_uri = response
                            .get_header(header_names::CONTACT)
                            .and_then(extract_uri)
                            .and_then(|u| SipUri::parse(&u).ok());

                        self.early_media.on_provisional(
                            &to_tag,
                            contact_uri.as_ref(),
                        );

                        // Only process SDP from the selected fork
                        if (self.early_media_config.follow_early_media_fork
                            || self.early_media.is_selected_fork(&to_tag))
                            && !response.body.is_empty()
                                && (self.early_media_config.accept_multiple_sdp_answers
                                    || self.remote_sdp.is_none())
                                {
                                    self.remote_sdp =
                                        SessionDescription::parse(&response.body).ok();
                                }
                    }
                }

                // Create early dialog if To tag is present
                if let (Some(invite), None) = (&self.invite, &self.dialog) {
                    self.dialog = Dialog::from_uac_response(invite, response);
                }
                debug!(call_id = %self.call_id, status, "Session early");
            }
            200..=299 => {
                self.state = SessionState::Established;
                // Create or confirm dialog
                if let Some(invite) = &self.invite {
                    if let Some(ref mut dialog) = self.dialog {
                        dialog.confirm();
                    } else {
                        self.dialog = Dialog::from_uac_response(invite, response);
                    }
                }
                // Parse SDP from body
                if !response.body.is_empty() {
                    self.remote_sdp = SessionDescription::parse(&response.body).ok();
                }
                info!(call_id = %self.call_id, "Session established");
            }
            300..=699 => {
                self.state = SessionState::Terminated;
                if let Some(ref mut dialog) = self.dialog {
                    dialog.terminate();
                }
                info!(call_id = %self.call_id, status, "Session failed");
            }
            _ => {}
        }
    }

    /// Build a 200 OK response (for UAS).
    pub fn build_200_ok(&self) -> Option<SipMessage> {
        let invite = self.invite.as_ref()?;
        let mut response = invite.create_response(200, "OK").ok()?;

        // Add Contact
        let contact = format!("<sip:asterisk@{}>", self.local_addr);
        response.headers.push(SipHeader {
            name: header_names::CONTACT.to_string(),
            value: contact,
        });

        // Add To tag
        for h in &mut response.headers {
            if h.name.eq_ignore_ascii_case(header_names::TO) && !h.value.contains("tag=") {
                h.value = format!("{};tag={}", h.value, self.local_tag);
            }
        }

        // Add SDP body
        if let Some(ref sdp) = self.local_sdp {
            let body = sdp.to_string();
            response.body = body.clone();
            // Update Content-Length and add Content-Type
            for h in &mut response.headers {
                if h.name.eq_ignore_ascii_case(header_names::CONTENT_LENGTH) {
                    h.value = body.len().to_string();
                }
            }
            response.headers.push(SipHeader {
                name: header_names::CONTENT_TYPE.to_string(),
                value: "application/sdp".to_string(),
            });
        }

        Some(response)
    }

    /// Build an ACK request.
    pub fn build_ack(&self) -> Option<SipMessage> {
        let invite = self.invite.as_ref()?;
        let dialog = self.dialog.as_ref()?;

        let uri = match &invite.start_line {
            StartLine::Request(r) => r.uri.clone(),
            _ => return None,
        };

        let branch = format!("z9hG4bK{}", &Uuid::new_v4().to_string().replace('-', "")[..16]);

        let headers = vec![
            SipHeader { name: header_names::VIA.to_string(), value: format!("SIP/2.0/UDP {};branch={}", self.local_addr, branch) },
            SipHeader { name: header_names::MAX_FORWARDS.to_string(), value: "70".to_string() },
            SipHeader { name: header_names::FROM.to_string(), value: invite.from_header()?.to_string() },
            SipHeader {
                name: header_names::TO.to_string(),
                value: format!("{};tag={}", invite.to_header()?.split(";tag=").next().unwrap_or(""), dialog.remote_tag),
            },
            SipHeader { name: header_names::CALL_ID.to_string(), value: self.call_id.clone() },
            SipHeader { name: header_names::CSEQ.to_string(), value: "1 ACK".to_string() },
            SipHeader { name: header_names::CONTENT_LENGTH.to_string(), value: "0".to_string() },
        ];

        Some(SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Ack,
                uri,
                version: "SIP/2.0".to_string(),
            }),
            headers,
            body: String::new(),
        })
    }

    /// Build a BYE request.
    pub fn build_bye(&mut self) -> Option<SipMessage> {
        let dialog = self.dialog.as_mut()?;
        let cseq = dialog.next_cseq();

        let uri = SipUri::parse(&dialog.remote_target).ok().unwrap_or_else(|| SipUri {
            scheme: "sip".to_string(),
            user: None,
            password: None,
            host: self.remote_addr.ip().to_string(),
            port: Some(self.remote_addr.port()),
            parameters: Default::default(),
            headers: Default::default(),
        });

        let branch = format!("z9hG4bK{}", &Uuid::new_v4().to_string().replace('-', "")[..16]);

        let from_value = format!("<sip:asterisk@{}>;tag={}", self.local_addr, dialog.local_tag);

        let to_value = format!("<{}>;tag={}", dialog.remote_uri, dialog.remote_tag);

        let headers = vec![
            SipHeader { name: header_names::VIA.to_string(), value: format!("SIP/2.0/UDP {};branch={}", self.local_addr, branch) },
            SipHeader { name: header_names::MAX_FORWARDS.to_string(), value: "70".to_string() },
            SipHeader { name: header_names::FROM.to_string(), value: from_value },
            SipHeader { name: header_names::TO.to_string(), value: to_value },
            SipHeader { name: header_names::CALL_ID.to_string(), value: self.call_id.clone() },
            SipHeader { name: header_names::CSEQ.to_string(), value: format!("{} BYE", cseq) },
            SipHeader { name: header_names::CONTENT_LENGTH.to_string(), value: "0".to_string() },
        ];

        self.state = SessionState::Terminating;

        Some(SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Bye,
                uri,
                version: "SIP/2.0".to_string(),
            }),
            headers,
            body: String::new(),
        })
    }

    /// Build a CANCEL request for the original INVITE.
    ///
    /// Used to cancel remaining forks after 200 OK is received from one UAS.
    pub fn build_cancel(&self) -> Option<SipMessage> {
        let invite = self.invite.as_ref()?;

        let uri = match &invite.start_line {
            StartLine::Request(r) => r.uri.clone(),
            _ => return None,
        };

        // CANCEL reuses the same branch as the INVITE (same transaction)
        let via = invite.get_header(header_names::VIA)?;

        let headers = vec![
            SipHeader {
                name: header_names::VIA.to_string(),
                value: via.to_string(),
            },
            SipHeader {
                name: header_names::MAX_FORWARDS.to_string(),
                value: "70".to_string(),
            },
            SipHeader {
                name: header_names::FROM.to_string(),
                value: invite.from_header()?.to_string(),
            },
            SipHeader {
                name: header_names::TO.to_string(),
                value: invite.to_header()?.to_string(),
            },
            SipHeader {
                name: header_names::CALL_ID.to_string(),
                value: self.call_id.clone(),
            },
            SipHeader {
                name: header_names::CSEQ.to_string(),
                value: "1 CANCEL".to_string(),
            },
            SipHeader {
                name: header_names::CONTENT_LENGTH.to_string(),
                value: "0".to_string(),
            },
        ];

        Some(SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Cancel,
                uri,
                version: "SIP/2.0".to_string(),
            }),
            headers,
            body: String::new(),
        })
    }

    /// Build an in-dialog re-INVITE with a new SDP offer.
    ///
    /// Used by the SFU ConfBridge to add/remove video streams for participants.
    pub fn build_reinvite(&mut self, sdp: &SessionDescription) -> Option<SipMessage> {
        let dialog = self.dialog.as_mut()?;
        let cseq = dialog.next_cseq();

        // Build the Request-URI from the remote target (Contact of the remote side).
        let uri = SipUri::parse(&dialog.remote_target).ok().unwrap_or_else(|| SipUri {
            scheme: "sip".to_string(),
            user: None,
            password: None,
            host: self.remote_addr.ip().to_string(),
            port: Some(self.remote_addr.port()),
            parameters: Default::default(),
            headers: Default::default(),
        });

        let branch = format!("z9hG4bK{}", &Uuid::new_v4().to_string().replace('-', "")[..16]);

        // For UAS (inbound call), From = our local tag, To = remote tag.
        let from_value = format!("<sip:asterisk@{}>;tag={}", self.local_addr, dialog.local_tag);
        let to_value = format!("<{}>;tag={}", dialog.remote_uri, dialog.remote_tag);

        let body = sdp.to_string();

        let headers = vec![
            SipHeader { name: header_names::VIA.to_string(), value: format!("SIP/2.0/UDP {};branch={}", self.local_addr, branch) },
            SipHeader { name: header_names::MAX_FORWARDS.to_string(), value: "70".to_string() },
            SipHeader { name: header_names::FROM.to_string(), value: from_value },
            SipHeader { name: header_names::TO.to_string(), value: to_value },
            SipHeader { name: header_names::CALL_ID.to_string(), value: self.call_id.clone() },
            SipHeader { name: header_names::CSEQ.to_string(), value: format!("{} INVITE", cseq) },
            SipHeader { name: header_names::CONTACT.to_string(), value: format!("<sip:asterisk@{}>", self.local_addr) },
            SipHeader { name: header_names::CONTENT_TYPE.to_string(), value: "application/sdp".to_string() },
            SipHeader { name: header_names::CONTENT_LENGTH.to_string(), value: body.len().to_string() },
        ];

        // Store the new local SDP.
        self.local_sdp = Some(sdp.clone());

        Some(SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Invite,
                uri,
                version: "SIP/2.0".to_string(),
            }),
            headers,
            body,
        })
    }

    /// Build an ACK for a received 200 OK to our re-INVITE.
    pub fn build_reinvite_ack(&self, response: &SipMessage) -> Option<SipMessage> {
        let dialog = self.dialog.as_ref()?;

        let uri = SipUri::parse(&dialog.remote_target).ok().unwrap_or_else(|| SipUri {
            scheme: "sip".to_string(),
            user: None,
            password: None,
            host: self.remote_addr.ip().to_string(),
            port: Some(self.remote_addr.port()),
            parameters: Default::default(),
            headers: Default::default(),
        });

        let branch = format!("z9hG4bK{}", &Uuid::new_v4().to_string().replace('-', "")[..16]);

        let from_value = format!("<sip:asterisk@{}>;tag={}", self.local_addr, dialog.local_tag);
        let to_value = format!("<{}>;tag={}", dialog.remote_uri, dialog.remote_tag);

        // CSeq from the response we're ACKing.
        let cseq_num = response.cseq()
            .and_then(|cs| cs.split_whitespace().next())
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(1);

        let headers = vec![
            SipHeader { name: header_names::VIA.to_string(), value: format!("SIP/2.0/UDP {};branch={}", self.local_addr, branch) },
            SipHeader { name: header_names::MAX_FORWARDS.to_string(), value: "70".to_string() },
            SipHeader { name: header_names::FROM.to_string(), value: from_value },
            SipHeader { name: header_names::TO.to_string(), value: to_value },
            SipHeader { name: header_names::CALL_ID.to_string(), value: self.call_id.clone() },
            SipHeader { name: header_names::CSEQ.to_string(), value: format!("{} ACK", cseq_num) },
            SipHeader { name: header_names::CONTENT_LENGTH.to_string(), value: "0".to_string() },
        ];

        Some(SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Ack,
                uri,
                version: "SIP/2.0".to_string(),
            }),
            headers,
            body: String::new(),
        })
    }

    /// Terminate the session.
    pub fn terminate(&mut self) {
        self.state = SessionState::Terminated;
        if let Some(ref mut dialog) = self.dialog {
            dialog.terminate();
        }
    }
}
