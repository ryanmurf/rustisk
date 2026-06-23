//! Extended session handling (port of res_pjsip_session.c extensions).
//!
//! Adds session supplements (pre/post request processing hooks),
//! re-INVITE handling for mid-call media changes, session timers
//! (RFC 4028), and connected-line updates via re-INVITE/UPDATE.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::RwLock;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::parser::{
    header_names, RequestLine, SipHeader, SipMessage, SipMethod, SipUri, StartLine,
};
use crate::sdp::SessionDescription;
use crate::session::{SessionState, SipSession};

// ---------------------------------------------------------------------------
// Session supplement (pre/post processing hooks)
// ---------------------------------------------------------------------------

/// Priority levels for session supplements (lower = earlier).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SupplementPriority {
    /// Runs first (e.g. security checks).
    First = 0,
    /// Normal priority.
    Normal = 500,
    /// Runs last (e.g. logging).
    Last = 1000,
}

/// A session supplement that provides hooks into SIP session processing.
///
/// Supplements are called in priority order for both inbound and outbound
/// SIP request/response processing.
#[async_trait]
pub trait SessionSupplement: Send + Sync {
    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// SIP method this supplement applies to (None = all methods).
    fn method(&self) -> Option<SipMethod> {
        None
    }

    /// Priority for ordering among supplements.
    fn priority(&self) -> SupplementPriority {
        SupplementPriority::Normal
    }

    /// Called before processing an incoming request.
    async fn on_incoming_request(
        &self,
        _session: &mut SipSession,
        _request: &SipMessage,
    ) -> SupplementResult {
        SupplementResult::Continue
    }

    /// Called after processing an incoming request.
    async fn on_incoming_request_post(
        &self,
        _session: &mut SipSession,
        _request: &SipMessage,
    ) {
    }

    /// Called before processing an incoming response.
    async fn on_incoming_response(
        &self,
        _session: &mut SipSession,
        _response: &SipMessage,
    ) {
    }

    /// Called before sending an outgoing request.
    async fn on_outgoing_request(
        &self,
        _session: &mut SipSession,
        _request: &mut SipMessage,
    ) {
    }

    /// Called before sending an outgoing response.
    async fn on_outgoing_response(
        &self,
        _session: &mut SipSession,
        _response: &mut SipMessage,
    ) {
    }
}

/// Result of a supplement's incoming request handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupplementResult {
    /// Continue processing normally.
    Continue,
    /// Stop processing and reject the request with the given status.
    Reject(u16),
}

/// Registry of session supplements.
#[derive(Default)]
pub struct SupplementRegistry {
    supplements: RwLock<Vec<Arc<dyn SessionSupplement>>>,
}

impl SupplementRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a session supplement.
    pub fn register(&self, supplement: Arc<dyn SessionSupplement>) {
        let mut sups = self.supplements.write();
        sups.push(supplement);
        sups.sort_by_key(|s| s.priority());
    }

    /// Unregister a supplement by name.
    pub fn unregister(&self, name: &str) {
        self.supplements.write().retain(|s| s.name() != name);
    }

    /// Get all supplements (sorted by priority).
    pub fn get_all(&self) -> Vec<Arc<dyn SessionSupplement>> {
        self.supplements.read().clone()
    }

    /// Get supplements for a specific method.
    pub fn get_for_method(&self, method: SipMethod) -> Vec<Arc<dyn SessionSupplement>> {
        self.supplements
            .read()
            .iter()
            .filter(|s| s.method().is_none() || s.method() == Some(method))
            .cloned()
            .collect()
    }

    /// Run all incoming-request supplements.
    pub async fn run_incoming_request(
        &self,
        session: &mut SipSession,
        request: &SipMessage,
    ) -> SupplementResult {
        let method = request.method().unwrap_or(SipMethod::Invite);
        let supplements = self.get_for_method(method);

        for sup in &supplements {
            let result = sup.on_incoming_request(session, request).await;
            if result != SupplementResult::Continue {
                return result;
            }
        }

        SupplementResult::Continue
    }

    /// Run all outgoing-request supplements.
    pub async fn run_outgoing_request(
        &self,
        session: &mut SipSession,
        request: &mut SipMessage,
    ) {
        let method = request.method().unwrap_or(SipMethod::Invite);
        let supplements = self.get_for_method(method);

        for sup in &supplements {
            sup.on_outgoing_request(session, request).await;
        }
    }
}

impl std::fmt::Debug for SupplementRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.supplements.read().len();
        f.debug_struct("SupplementRegistry")
            .field("count", &count)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Re-INVITE handling
// ---------------------------------------------------------------------------

/// Reason for a re-INVITE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReinviteReason {
    /// Media change (codec renegotiation, hold/unhold).
    MediaChange,
    /// Connected line update (caller-ID update mid-call).
    ConnectedLineUpdate,
    /// Session timer refresh.
    SessionTimerRefresh,
    /// T.38 fax switchover.
    FaxSwitchover,
    /// Direct media negotiation.
    DirectMedia,
}

/// Build a re-INVITE request for an established session.
pub fn build_reinvite(
    session: &mut SipSession,
    new_sdp: Option<SessionDescription>,
    reason: ReinviteReason,
) -> Option<SipMessage> {
    let dialog = session.dialog.as_mut()?;

    if session.state != SessionState::Established {
        warn!("Cannot send re-INVITE: session not established");
        return None;
    }

    let cseq = dialog.next_cseq();
    let branch = format!(
        "z9hG4bK{}",
        &Uuid::new_v4().to_string().replace('-', "")[..16]
    );

    let target_uri = SipUri::parse(&dialog.remote_target).ok().unwrap_or(SipUri {
        scheme: "sip".to_string(),
        user: None,
        password: None,
        host: session.remote_addr.ip().to_string(),
        port: Some(session.remote_addr.port()),
        parameters: Default::default(),
        headers: Default::default(),
    });

    let sdp_body = new_sdp
        .as_ref()
        .map(|s| s.to_string())
        .unwrap_or_default();

    if new_sdp.is_some() {
        session.local_sdp = new_sdp;
    }

    let from_value = format!(
        "<sip:asterisk@{}>;tag={}",
        session.local_addr, dialog.local_tag
    );
    let to_value = format!(
        "<{}>;tag={}",
        dialog.remote_uri, dialog.remote_tag
    );

    let mut headers = vec![
        SipHeader {
            name: header_names::VIA.to_string(),
            value: format!("SIP/2.0/UDP {};branch={}", session.local_addr, branch),
        },
        SipHeader {
            name: header_names::MAX_FORWARDS.to_string(),
            value: "70".to_string(),
        },
        SipHeader {
            name: header_names::FROM.to_string(),
            value: from_value,
        },
        SipHeader {
            name: header_names::TO.to_string(),
            value: to_value,
        },
        SipHeader {
            name: header_names::CALL_ID.to_string(),
            value: session.call_id.clone(),
        },
        SipHeader {
            name: header_names::CSEQ.to_string(),
            value: format!("{} INVITE", cseq),
        },
        SipHeader {
            name: header_names::CONTACT.to_string(),
            value: format!("<sip:asterisk@{}>", session.local_addr),
        },
        SipHeader {
            name: header_names::USER_AGENT.to_string(),
            value: "Rustisk/0.1.0".to_string(),
        },
        SipHeader {
            name: header_names::ALLOW.to_string(),
            value: "INVITE, ACK, CANCEL, BYE, OPTIONS, REFER, NOTIFY, UPDATE".to_string(),
        },
    ];

    if !sdp_body.is_empty() {
        headers.push(SipHeader {
            name: header_names::CONTENT_TYPE.to_string(),
            value: "application/sdp".to_string(),
        });
    }
    headers.push(SipHeader {
        name: header_names::CONTENT_LENGTH.to_string(),
        value: sdp_body.len().to_string(),
    });

    debug!(
        call_id = %session.call_id,
        reason = ?reason,
        "Building re-INVITE"
    );

    Some(SipMessage {
        start_line: StartLine::Request(RequestLine {
            method: SipMethod::Invite,
            uri: target_uri,
            version: "SIP/2.0".to_string(),
        }),
        headers,
        body: sdp_body,
    })
}

// ---------------------------------------------------------------------------
// Session timers (RFC 4028)
// ---------------------------------------------------------------------------

/// Session timer configuration.
#[derive(Debug, Clone)]
pub struct SessionTimerConfig {
    /// Session-Expires interval in seconds.
    pub session_expires: u32,
    /// Minimum session-expires we will accept.
    pub min_se: u32,
    /// Our preferred refresher role.
    pub refresher: SessionTimerRefresher,
    /// Whether session timers are required.
    pub required: bool,
}

impl Default for SessionTimerConfig {
    fn default() -> Self {
        Self {
            session_expires: 1800,
            min_se: 90,
            refresher: SessionTimerRefresher::Uac,
            required: false,
        }
    }
}

/// Who is responsible for refreshing the session timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTimerRefresher {
    Uac,
    Uas,
}

impl SessionTimerRefresher {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Uac => "uac",
            Self::Uas => "uas",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "uas" => Self::Uas,
            _ => Self::Uac,
        }
    }
}

/// Active session timer state.
#[derive(Debug, Clone)]
pub struct SessionTimer {
    /// Negotiated session-expires interval.
    pub session_expires: u32,
    /// Who is the refresher.
    pub refresher: SessionTimerRefresher,
    /// When the last refresh occurred.
    pub last_refresh: Instant,
    /// Whether we are the refresher.
    pub we_are_refresher: bool,
}

impl SessionTimer {
    pub fn new(config: &SessionTimerConfig, is_uac: bool) -> Self {
        let we_are_refresher = match config.refresher {
            SessionTimerRefresher::Uac => is_uac,
            SessionTimerRefresher::Uas => !is_uac,
        };

        Self {
            session_expires: config.session_expires,
            refresher: config.refresher,
            last_refresh: Instant::now(),
            we_are_refresher,
        }
    }

    /// Check if a refresh is needed.
    ///
    /// We send a re-INVITE at 50% of the session-expires interval
    /// (half the timer, like Asterisk does).
    pub fn needs_refresh(&self) -> bool {
        if !self.we_are_refresher {
            return false;
        }
        let half_interval = Duration::from_secs((self.session_expires / 2) as u64);
        self.last_refresh.elapsed() >= half_interval
    }

    /// Check if the session has expired (no refresh received).
    pub fn is_expired(&self) -> bool {
        self.last_refresh.elapsed() >= Duration::from_secs(self.session_expires as u64)
    }

    /// Mark that a refresh was received/sent.
    pub fn refresh(&mut self) {
        self.last_refresh = Instant::now();
    }

    /// Build Session-Expires and Min-SE headers.
    pub fn build_headers(&self, config: &SessionTimerConfig) -> Vec<SipHeader> {
        vec![
            SipHeader {
                name: "Session-Expires".to_string(),
                value: format!(
                    "{};refresher={}",
                    self.session_expires,
                    self.refresher.as_str()
                ),
            },
            SipHeader {
                name: "Min-SE".to_string(),
                value: config.min_se.to_string(),
            },
        ]
    }
}

/// Parse Session-Expires header from a SIP message.
pub fn parse_session_expires(msg: &SipMessage) -> Option<(u32, SessionTimerRefresher)> {
    let value = msg.get_header("Session-Expires")?;

    let mut parts = value.split(';');
    let seconds: u32 = parts.next()?.trim().parse().ok()?;

    let refresher = parts
        .find_map(|p| {
            let p = p.trim();
            p.strip_prefix("refresher=")
                .map(SessionTimerRefresher::from_str)
        })
        .unwrap_or(SessionTimerRefresher::Uac);

    Some((seconds, refresher))
}

/// Parse Min-SE header from a SIP message.
pub fn parse_min_se(msg: &SipMessage) -> Option<u32> {
    msg.get_header("Min-SE")
        .and_then(|v| v.trim().parse::<u32>().ok())
}

// ---------------------------------------------------------------------------
// RFC 4028 full compliance: Min-SE enforcement & 422 response
// ---------------------------------------------------------------------------

/// Default minimum session-expires value (RFC 4028 Section 4).
pub const MIN_SE_DEFAULT: u32 = 90;

/// Validate Session-Expires against Min-SE.
///
/// Returns `Ok(())` if the Session-Expires is acceptable, or
/// `Err(min_se)` if it is too small (caller should send 422).
pub fn validate_session_interval(
    session_expires: u32,
    min_se: u32,
) -> Result<(), u32> {
    if session_expires < min_se {
        Err(min_se)
    } else {
        Ok(())
    }
}

/// Build a 422 Session Interval Too Small response.
///
/// Per RFC 4028 Section 7, when a UAS receives a Session-Expires value
/// below its minimum, it responds with 422 and includes a Min-SE header.
pub fn build_422_response(
    request: &SipMessage,
    min_se: u32,
) -> Option<SipMessage> {
    let mut response = request.create_response(422, "Session Interval Too Small").ok()?;
    response.headers.push(SipHeader {
        name: "Min-SE".to_string(),
        value: min_se.to_string(),
    });
    Some(response)
}

/// Negotiate session timer parameters from an incoming request.
///
/// Applies RFC 4028 rules:
/// 1. Check if Session-Expires is present
/// 2. Validate against our Min-SE
/// 3. Determine refresher role
///
/// Returns `Ok(Some(SessionTimer))` if negotiation succeeded,
/// `Ok(None)` if no session timer was requested,
/// `Err(min_se)` if the interval is too small (caller should send 422).
pub fn negotiate_session_timer(
    msg: &SipMessage,
    config: &SessionTimerConfig,
    is_uac: bool,
) -> Result<Option<SessionTimer>, u32> {
    let (session_expires, remote_refresher) = match parse_session_expires(msg) {
        Some(v) => v,
        None => {
            if config.required {
                // We require session timers but peer didn't offer; use defaults.
                return Ok(Some(SessionTimer::new(config, is_uac)));
            }
            return Ok(None);
        }
    };

    // Validate against our min-SE.
    validate_session_interval(session_expires, config.min_se)?;

    // Determine refresher: honor remote preference if possible.
    let refresher = remote_refresher;
    let we_are_refresher = match refresher {
        SessionTimerRefresher::Uac => is_uac,
        SessionTimerRefresher::Uas => !is_uac,
    };

    Ok(Some(SessionTimer {
        session_expires,
        refresher,
        last_refresh: Instant::now(),
        we_are_refresher,
    }))
}

/// Build a BYE request due to session timer expiry.
///
/// Per RFC 4028 Section 10, if the session timer expires without a
/// refresh, the UA that is not the refresher should send BYE.
pub fn build_session_timeout_bye(session: &mut SipSession) -> Option<SipMessage> {
    warn!(
        call_id = %session.call_id,
        "Session timer expired -- sending BYE"
    );
    session.build_bye()
}

// ---------------------------------------------------------------------------
// Connected line updates
// ---------------------------------------------------------------------------

/// Connected line information for display updates.
#[derive(Debug, Clone)]
pub struct ConnectedLineInfo {
    /// Display name.
    pub name: Option<String>,
    /// SIP URI.
    pub uri: String,
    /// Privacy flag.
    pub privacy: bool,
}

/// Build an UPDATE request for a connected-line update.
pub fn build_update_connected_line(
    session: &mut SipSession,
    connected: &ConnectedLineInfo,
) -> Option<SipMessage> {
    let dialog = session.dialog.as_mut()?;

    if session.state != SessionState::Established {
        return None;
    }

    let cseq = dialog.next_cseq();
    let branch = format!(
        "z9hG4bK{}",
        &Uuid::new_v4().to_string().replace('-', "")[..16]
    );

    let target_uri = SipUri::parse(&dialog.remote_target).ok().unwrap_or(SipUri {
        scheme: "sip".to_string(),
        user: None,
        password: None,
        host: session.remote_addr.ip().to_string(),
        port: Some(session.remote_addr.port()),
        parameters: Default::default(),
        headers: Default::default(),
    });

    let from_display = connected
        .name
        .as_deref()
        .map(|n| format!("\"{}\" ", n))
        .unwrap_or_default();

    let from_value = format!(
        "{}<sip:asterisk@{}>;tag={}",
        from_display, session.local_addr, dialog.local_tag
    );

    let to_value = format!(
        "<{}>;tag={}",
        dialog.remote_uri, dialog.remote_tag
    );

    let mut headers = vec![
        SipHeader {
            name: header_names::VIA.to_string(),
            value: format!("SIP/2.0/UDP {};branch={}", session.local_addr, branch),
        },
        SipHeader {
            name: header_names::MAX_FORWARDS.to_string(),
            value: "70".to_string(),
        },
        SipHeader {
            name: header_names::FROM.to_string(),
            value: from_value,
        },
        SipHeader {
            name: header_names::TO.to_string(),
            value: to_value,
        },
        SipHeader {
            name: header_names::CALL_ID.to_string(),
            value: session.call_id.clone(),
        },
        SipHeader {
            name: header_names::CSEQ.to_string(),
            value: format!("{} UPDATE", cseq),
        },
        SipHeader {
            name: header_names::CONTACT.to_string(),
            value: format!("<{}>", connected.uri),
        },
        SipHeader {
            name: header_names::CONTENT_LENGTH.to_string(),
            value: "0".to_string(),
        },
    ];

    if connected.privacy {
        headers.push(SipHeader {
            name: "Privacy".to_string(),
            value: "id".to_string(),
        });
    }

    Some(SipMessage {
        start_line: StartLine::Request(RequestLine {
            method: SipMethod::Update,
            uri: target_uri,
            version: "SIP/2.0".to_string(),
        }),
        headers,
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
    fn test_session_timer_config() {
        let config = SessionTimerConfig::default();
        assert_eq!(config.session_expires, 1800);
        assert_eq!(config.min_se, 90);
    }

    #[test]
    fn test_session_timer_refresh() {
        let config = SessionTimerConfig {
            session_expires: 10,
            min_se: 5,
            refresher: SessionTimerRefresher::Uac,
            required: false,
        };

        let mut timer = SessionTimer::new(&config, true);
        assert!(timer.we_are_refresher);
        assert!(!timer.is_expired());

        // Simulate time passing via refresh.
        timer.refresh();
        assert!(!timer.is_expired());
    }

    #[test]
    fn test_parse_session_expires() {
        let msg = SipMessage::parse(
            b"INVITE sip:bob@example.com SIP/2.0\r\n\
              Via: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123\r\n\
              From: <sip:alice@example.com>;tag=abc\r\n\
              To: <sip:bob@example.com>\r\n\
              Call-ID: timer-test\r\n\
              CSeq: 1 INVITE\r\n\
              Session-Expires: 1800;refresher=uac\r\n\
              Min-SE: 90\r\n\
              Content-Length: 0\r\n\
              \r\n",
        )
        .unwrap();

        let (seconds, refresher) = parse_session_expires(&msg).unwrap();
        assert_eq!(seconds, 1800);
        assert_eq!(refresher, SessionTimerRefresher::Uac);
    }

    #[test]
    fn test_supplement_priority_ordering() {
        assert!(SupplementPriority::First < SupplementPriority::Normal);
        assert!(SupplementPriority::Normal < SupplementPriority::Last);
    }

    #[test]
    fn test_validate_session_interval() {
        assert!(validate_session_interval(1800, 90).is_ok());
        assert!(validate_session_interval(90, 90).is_ok());
        assert_eq!(validate_session_interval(60, 90), Err(90));
    }

    #[test]
    fn test_build_422_response() {
        let req = SipMessage::parse(
            b"INVITE sip:bob@example.com SIP/2.0\r\n\
              Via: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123\r\n\
              From: <sip:alice@example.com>;tag=abc\r\n\
              To: <sip:bob@example.com>\r\n\
              Call-ID: timer-422\r\n\
              CSeq: 1 INVITE\r\n\
              Session-Expires: 30;refresher=uac\r\n\
              Content-Length: 0\r\n\
              \r\n",
        )
        .unwrap();

        let response = build_422_response(&req, 90).unwrap();
        assert_eq!(response.status_code(), Some(422));
        assert_eq!(response.get_header("Min-SE"), Some("90"));
    }

    #[test]
    fn test_negotiate_session_timer_ok() {
        let msg = SipMessage::parse(
            b"INVITE sip:bob@example.com SIP/2.0\r\n\
              Via: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123\r\n\
              From: <sip:alice@example.com>;tag=abc\r\n\
              To: <sip:bob@example.com>\r\n\
              Call-ID: negotiate-ok\r\n\
              CSeq: 1 INVITE\r\n\
              Session-Expires: 1800;refresher=uac\r\n\
              Min-SE: 90\r\n\
              Content-Length: 0\r\n\
              \r\n",
        )
        .unwrap();

        let config = SessionTimerConfig::default();
        let result = negotiate_session_timer(&msg, &config, false);
        assert!(result.is_ok());
        let timer = result.unwrap().unwrap();
        assert_eq!(timer.session_expires, 1800);
        assert_eq!(timer.refresher, SessionTimerRefresher::Uac);
    }

    #[test]
    fn test_negotiate_session_timer_too_small() {
        let msg = SipMessage::parse(
            b"INVITE sip:bob@example.com SIP/2.0\r\n\
              Via: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123\r\n\
              From: <sip:alice@example.com>;tag=abc\r\n\
              To: <sip:bob@example.com>\r\n\
              Call-ID: negotiate-small\r\n\
              CSeq: 1 INVITE\r\n\
              Session-Expires: 30;refresher=uac\r\n\
              Content-Length: 0\r\n\
              \r\n",
        )
        .unwrap();

        let config = SessionTimerConfig::default();
        let result = negotiate_session_timer(&msg, &config, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 90);
    }

    #[test]
    fn test_parse_min_se() {
        let msg = SipMessage::parse(
            b"INVITE sip:bob@example.com SIP/2.0\r\n\
              Via: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123\r\n\
              From: <sip:alice@example.com>;tag=abc\r\n\
              To: <sip:bob@example.com>\r\n\
              Call-ID: min-se-test\r\n\
              CSeq: 1 INVITE\r\n\
              Min-SE: 120\r\n\
              Content-Length: 0\r\n\
              \r\n",
        )
        .unwrap();

        assert_eq!(parse_min_se(&msg), Some(120));
    }
}
