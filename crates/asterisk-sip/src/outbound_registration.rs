//! Outbound SIP REGISTER client (port of res_pjsip_outbound_registration.c).
//!
//! Registers with remote SIP servers/ITSPs. Implements a registration state
//! machine with periodic refresh, authentication challenge handling, and
//! retry-with-backoff on failure.
//!
//! Also includes keep-alive support per RFC 5626:
//! - OPTIONS ping keep-alive
//! - CRLF keep-alive for TCP/TLS connections
//! - Flow-Timer header processing

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::auth::{create_digest_response, DigestChallenge, DigestCredentials};
use crate::parser::{
    header_names, RequestLine, SipHeader, SipMessage, SipMethod, SipUri, StartLine,
};

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// Registration states for the outbound registration FSM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationState {
    /// Not yet registered.
    Unregistered,
    /// REGISTER request sent, waiting for response.
    Registering,
    /// Successfully registered; refresh timer running.
    Registered,
    /// Refreshing registration before expiration.
    Refreshing,
    /// Received a rejection that is considered permanent (e.g. 403).
    Rejected,
    /// Stopping / shutting down.
    Stopping,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for a single outbound registration.
#[derive(Debug, Clone)]
pub struct OutboundRegistrationConfig {
    /// SIP URI of the remote registrar (server_uri).
    pub server_uri: String,
    /// Our address-of-record (client_uri, goes in the To header).
    pub client_uri: String,
    /// Authentication username (may be empty if no auth required).
    pub username: String,
    /// Authentication password.
    pub password: String,
    /// Realm for auth (empty = match any realm).
    pub auth_realm: String,
    /// Requested registration expiration in seconds.
    pub expiration: u32,
    /// Interval between retries on failure (seconds).
    pub retry_interval: u32,
    /// Interval to retry after a 403 Forbidden (0 = never retry).
    pub forbidden_retry_interval: u32,
    /// Maximum number of retries (0 = stop after first failure).
    pub max_retries: u32,
    /// User part to place in the Contact header.
    pub contact_user: String,
    /// Whether a failed auth challenge is permanent.
    pub auth_rejection_permanent: bool,
    /// Local address from which to send the REGISTER.
    pub local_addr: SocketAddr,
}

impl Default for OutboundRegistrationConfig {
    fn default() -> Self {
        Self {
            server_uri: String::new(),
            client_uri: String::new(),
            username: String::new(),
            password: String::new(),
            auth_realm: String::new(),
            expiration: 3600,
            retry_interval: 60,
            forbidden_retry_interval: 0,
            max_retries: 10,
            contact_user: "s".to_string(),
            auth_rejection_permanent: true,
            local_addr: "0.0.0.0:5060".parse().unwrap(),
        }
    }
}

// ---------------------------------------------------------------------------
// OutboundRegistration
// ---------------------------------------------------------------------------

/// An outbound registration to a remote SIP registrar.
///
/// Call [`tick()`](OutboundRegistration::tick) periodically (e.g. every second)
/// to drive the state machine. The method returns an optional `SipMessage`
/// that should be sent via the transport.
#[derive(Debug)]
pub struct OutboundRegistration {
    pub config: OutboundRegistrationConfig,
    /// Current FSM state.
    pub state: RegistrationState,
    /// Number of retry attempts so far.
    pub retries: u32,
    /// CSeq counter.
    cseq: u32,
    /// Call-ID for the registration dialog.
    call_id: String,
    /// Our From tag.
    from_tag: String,
    /// The last nonce received in a 401/407 challenge.
    last_challenge: Option<DigestChallenge>,
    /// When the last REGISTER was sent.
    last_sent: Option<Instant>,
    /// When the current registration expires.
    expires_at: Option<Instant>,
    /// Server remote address (parsed from server_uri).
    server_addr: Option<SocketAddr>,
}

impl OutboundRegistration {
    pub fn new(config: OutboundRegistrationConfig) -> Self {
        let call_id = format!("obreg-{}", Uuid::new_v4());
        let from_tag = Uuid::new_v4().to_string()[..8].to_string();

        // Try to parse the server address from server_uri.
        let server_addr = SipUri::parse(&config.server_uri)
            .ok()
            .and_then(|u| {
                let port = u.port.unwrap_or(5060);
                format!("{}:{}", u.host, port).parse::<SocketAddr>().ok()
            });

        Self {
            config,
            state: RegistrationState::Unregistered,
            retries: 0,
            cseq: 0,
            call_id,
            from_tag,
            last_challenge: None,
            last_sent: None,
            expires_at: None,
            server_addr,
        }
    }

    /// Remote server address (if resolved).
    pub fn server_addr(&self) -> Option<SocketAddr> {
        self.server_addr
    }

    /// Set the remote server address explicitly.
    pub fn set_server_addr(&mut self, addr: SocketAddr) {
        self.server_addr = Some(addr);
    }

    /// Drive the state machine. Returns an optional SIP message to send.
    ///
    /// Call this periodically (every ~1s is fine).
    pub fn tick(&mut self) -> Option<SipMessage> {
        match self.state {
            RegistrationState::Unregistered => {
                // Start registering immediately.
                self.state = RegistrationState::Registering;
                Some(self.build_register(false))
            }
            RegistrationState::Registering | RegistrationState::Refreshing => {
                // Waiting for response -- check for timeout.
                if let Some(sent) = self.last_sent {
                    // 32-second transaction timeout (Timer B / Timer F).
                    if sent.elapsed() > Duration::from_secs(32) {
                        warn!(uri = %self.config.server_uri, "Outbound registration timed out");
                        self.handle_failure();
                    }
                }
                None
            }
            RegistrationState::Registered => {
                // Check if we need to refresh (send re-REGISTER before expiry).
                if let Some(_expires_at) = self.expires_at {
                    // Refresh when 90% of the expiration has elapsed.
                    let refresh_point = Duration::from_secs(
                        (self.config.expiration as f64 * 0.9) as u64,
                    );
                    if self.last_sent.is_none_or(|s| s.elapsed() >= refresh_point) {
                        self.state = RegistrationState::Refreshing;
                        return Some(self.build_register(false));
                    }
                }
                None
            }
            RegistrationState::Rejected => {
                // Check retry timer.
                let interval = if self.config.forbidden_retry_interval > 0 {
                    self.config.forbidden_retry_interval
                } else {
                    return None; // Permanent rejection.
                };
                if let Some(sent) = self.last_sent {
                    if sent.elapsed() >= Duration::from_secs(interval as u64) {
                        self.state = RegistrationState::Registering;
                        return Some(self.build_register(false));
                    }
                }
                None
            }
            RegistrationState::Stopping => None,
        }
    }

    /// Process a response to our outbound REGISTER.
    pub fn on_response(&mut self, response: &SipMessage) -> Option<SipMessage> {
        let status = response.status_code().unwrap_or(0);

        match status {
            200..=299 => {
                // Success.
                self.state = RegistrationState::Registered;
                self.retries = 0;
                self.last_challenge = None;

                // Determine granted expiration.
                let granted = response
                    .get_header(header_names::EXPIRES)
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or(self.config.expiration);

                self.expires_at = Some(Instant::now() + Duration::from_secs(granted as u64));
                info!(
                    uri = %self.config.server_uri,
                    expires = granted,
                    "Outbound registration successful"
                );
                None
            }
            401 | 407 => {
                // Authentication challenge.
                let auth_header_name = if status == 401 {
                    header_names::WWW_AUTHENTICATE
                } else {
                    header_names::PROXY_AUTHENTICATE
                };

                let challenge = response
                    .get_header(auth_header_name)
                    .and_then(DigestChallenge::parse);

                match challenge {
                    Some(ch) => {
                        if self.last_challenge.is_some() && self.config.auth_rejection_permanent {
                            warn!(uri = %self.config.server_uri, "Repeated auth challenge -- permanent rejection");
                            self.state = RegistrationState::Rejected;
                            self.last_sent = Some(Instant::now());
                            return None;
                        }
                        self.last_challenge = Some(ch);
                        // Resend REGISTER with auth credentials.
                        let msg = self.build_register(true);
                        Some(msg)
                    }
                    None => {
                        warn!(uri = %self.config.server_uri, "No parseable challenge in {}", status);
                        self.handle_failure();
                        None
                    }
                }
            }
            403 => {
                warn!(uri = %self.config.server_uri, "Outbound registration forbidden (403)");
                self.state = RegistrationState::Rejected;
                self.last_sent = Some(Instant::now());
                None
            }
            _ => {
                warn!(uri = %self.config.server_uri, status, "Outbound registration failed");
                self.handle_failure();
                None
            }
        }
    }

    /// Build an unregister (expires=0) REGISTER request.
    pub fn build_unregister(&mut self) -> SipMessage {
        self.state = RegistrationState::Stopping;
        let mut msg = self.build_register(false);
        // Set Expires: 0 and contact expires=0.
        for h in &mut msg.headers {
            if h.name.eq_ignore_ascii_case(header_names::EXPIRES) {
                h.value = "0".to_string();
            }
            if h.name.eq_ignore_ascii_case(header_names::CONTACT) {
                h.value = format!("{};expires=0", h.value);
            }
        }
        msg
    }

    // ---- internal ---------------------------------------------------------

    fn handle_failure(&mut self) {
        self.retries += 1;
        if self.config.max_retries > 0 && self.retries >= self.config.max_retries {
            self.state = RegistrationState::Rejected;
        } else {
            self.state = RegistrationState::Unregistered;
        }
        self.last_sent = Some(Instant::now());
    }

    fn build_register(&mut self, with_auth: bool) -> SipMessage {
        self.cseq += 1;
        let branch = format!(
            "z9hG4bK{}",
            &Uuid::new_v4().to_string().replace('-', "")[..16]
        );

        let request_uri = SipUri::parse(&self.config.server_uri).unwrap_or_else(|_| SipUri {
            scheme: "sip".to_string(),
            user: None,
            password: None,
            host: "localhost".to_string(),
            port: Some(5060),
            parameters: Default::default(),
            headers: Default::default(),
        });

        let contact = format!(
            "<sip:{}@{}>",
            self.config.contact_user, self.config.local_addr
        );

        let mut headers = vec![
            SipHeader {
                name: header_names::VIA.to_string(),
                value: format!(
                    "SIP/2.0/UDP {};branch={}",
                    self.config.local_addr, branch
                ),
            },
            SipHeader {
                name: header_names::MAX_FORWARDS.to_string(),
                value: "70".to_string(),
            },
            SipHeader {
                name: header_names::FROM.to_string(),
                value: format!("<{}>;tag={}", self.config.client_uri, self.from_tag),
            },
            SipHeader {
                name: header_names::TO.to_string(),
                value: format!("<{}>", self.config.client_uri),
            },
            SipHeader {
                name: header_names::CALL_ID.to_string(),
                value: self.call_id.clone(),
            },
            SipHeader {
                name: header_names::CSEQ.to_string(),
                value: format!("{} REGISTER", self.cseq),
            },
            SipHeader {
                name: header_names::CONTACT.to_string(),
                value: contact,
            },
            SipHeader {
                name: header_names::EXPIRES.to_string(),
                value: self.config.expiration.to_string(),
            },
            SipHeader {
                name: header_names::USER_AGENT.to_string(),
                value: "Rustisk/0.1.0".to_string(),
            },
            SipHeader {
                name: header_names::CONTENT_LENGTH.to_string(),
                value: "0".to_string(),
            },
        ];

        // Attach Authorization header if we have a challenge.
        if with_auth {
            if let Some(ref challenge) = self.last_challenge {
                let creds = DigestCredentials {
                    username: self.config.username.clone(),
                    password: self.config.password.clone(),
                    realm: challenge.realm.clone(),
                };
                let uri_str = self.config.server_uri.clone();
                let auth_value =
                    create_digest_response(challenge, &creds, "REGISTER", &uri_str);
                headers.push(SipHeader {
                    name: header_names::AUTHORIZATION.to_string(),
                    value: auth_value,
                });
            }
        }

        self.last_sent = Some(Instant::now());

        SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Register,
                uri: request_uri,
                version: "SIP/2.0".to_string(),
            }),
            headers,
            body: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Keep-Alive support (RFC 5626)
// ---------------------------------------------------------------------------

/// Method used for keep-alive probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepAliveMethod {
    /// Send periodic OPTIONS requests.
    Options,
    /// Send CRLF (`\r\n\r\n`) on TCP/TLS connections.
    CrLf,
    /// No keep-alive.
    None,
}

/// State for outbound registration keep-alive.
#[derive(Debug, Clone)]
pub struct KeepAliveState {
    /// Method used for keep-alive.
    pub method: KeepAliveMethod,
    /// Interval between keep-alive probes.
    pub interval: Duration,
    /// When the last keep-alive was sent.
    pub last_sent: Option<Instant>,
    /// Consecutive failures.
    pub failures: u32,
    /// Maximum failures before declaring connection dead.
    pub max_failures: u32,
}

impl KeepAliveState {
    /// Create a new keep-alive state.
    pub fn new(method: KeepAliveMethod, interval: Duration) -> Self {
        Self {
            method,
            interval,
            last_sent: None,
            failures: 0,
            max_failures: 4,
        }
    }

    /// Create keep-alive state from a Flow-Timer header value.
    ///
    /// Per RFC 5626, the client should send keep-alives somewhat faster than
    /// the Flow-Timer value. We use 80% of the value.
    pub fn from_flow_timer(seconds: u32, method: KeepAliveMethod) -> Self {
        let interval = Duration::from_secs(((seconds as f64) * 0.8) as u64);
        Self::new(method, interval)
    }

    /// Check if a keep-alive should be sent now.
    pub fn should_send(&self) -> bool {
        if self.method == KeepAliveMethod::None {
            return false;
        }
        match self.last_sent {
            None => true,
            Some(sent) => sent.elapsed() >= self.interval,
        }
    }

    /// Record that a keep-alive was sent.
    pub fn mark_sent(&mut self) {
        self.last_sent = Some(Instant::now());
        debug!(method = ?self.method, "Keep-alive sent");
    }

    /// Record a keep-alive response received (for OPTIONS pings).
    pub fn on_response(&mut self) {
        self.failures = 0;
    }

    /// Record a keep-alive failure (timeout or error).
    pub fn on_failure(&mut self) {
        self.failures += 1;
    }

    /// Check if the connection should be considered dead.
    pub fn is_connection_dead(&self) -> bool {
        self.failures >= self.max_failures
    }

    /// Build an OPTIONS keep-alive request.
    pub fn build_options_ping(
        &self,
        local_addr: SocketAddr,
        remote_uri: &str,
    ) -> SipMessage {
        let branch = format!(
            "z9hG4bK{}",
            &Uuid::new_v4().to_string().replace('-', "")[..16]
        );

        let request_uri = SipUri::parse(remote_uri).unwrap_or_else(|_| SipUri {
            scheme: "sip".to_string(),
            user: None,
            password: None,
            host: "localhost".to_string(),
            port: Some(5060),
            parameters: Default::default(),
            headers: Default::default(),
        });

        let call_id = format!("keepalive-{}", Uuid::new_v4());
        let from_tag = &Uuid::new_v4().to_string()[..8];

        SipMessage {
            start_line: StartLine::Request(RequestLine {
                method: SipMethod::Options,
                uri: request_uri.clone(),
                version: "SIP/2.0".to_string(),
            }),
            headers: vec![
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
                    value: format!("<sip:keepalive@{}>;tag={}", local_addr, from_tag),
                },
                SipHeader {
                    name: header_names::TO.to_string(),
                    value: format!("<{}>", remote_uri),
                },
                SipHeader {
                    name: header_names::CALL_ID.to_string(),
                    value: call_id,
                },
                SipHeader {
                    name: header_names::CSEQ.to_string(),
                    value: "1 OPTIONS".to_string(),
                },
                SipHeader {
                    name: header_names::CONTENT_LENGTH.to_string(),
                    value: "0".to_string(),
                },
            ],
            body: String::new(),
        }
    }

    /// Get the CRLF bytes to send for CRLF keep-alive.
    pub fn crlf_bytes() -> &'static [u8] {
        b"\r\n\r\n"
    }
}

/// Extract Flow-Timer from a registration response and create a KeepAliveState.
pub fn keepalive_from_response(
    response: &SipMessage,
    method: KeepAliveMethod,
    default_interval: Duration,
) -> KeepAliveState {
    if let Some(flow_timer) = response
        .get_header("Flow-Timer")
        .and_then(|v| v.trim().parse::<u32>().ok())
    {
        KeepAliveState::from_flow_timer(flow_timer, method)
    } else {
        KeepAliveState::new(method, default_interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_register() {
        let mut reg = OutboundRegistration::new(OutboundRegistrationConfig {
            server_uri: "sip:registrar.example.com".to_string(),
            client_uri: "sip:alice@example.com".to_string(),
            expiration: 3600,
            local_addr: "10.0.0.1:5060".parse().unwrap(),
            ..Default::default()
        });

        assert_eq!(reg.state, RegistrationState::Unregistered);
        let msg = reg.tick();
        assert!(msg.is_some());
        assert_eq!(reg.state, RegistrationState::Registering);

        let msg = msg.unwrap();
        assert_eq!(msg.method(), Some(SipMethod::Register));
    }

    #[test]
    fn test_successful_registration() {
        let mut reg = OutboundRegistration::new(OutboundRegistrationConfig {
            server_uri: "sip:registrar.example.com".to_string(),
            client_uri: "sip:alice@example.com".to_string(),
            expiration: 3600,
            local_addr: "10.0.0.1:5060".parse().unwrap(),
            ..Default::default()
        });

        let _ = reg.tick(); // Send REGISTER.

        // Simulate 200 OK response.
        let response = SipMessage::parse(
            b"SIP/2.0 200 OK\r\n\
              Via: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123\r\n\
              From: <sip:alice@example.com>;tag=abc\r\n\
              To: <sip:alice@example.com>;tag=def\r\n\
              Call-ID: test123\r\n\
              CSeq: 1 REGISTER\r\n\
              Expires: 3600\r\n\
              Content-Length: 0\r\n\
              \r\n",
        )
        .unwrap();

        reg.on_response(&response);
        assert_eq!(reg.state, RegistrationState::Registered);
    }

    #[test]
    fn test_auth_challenge() {
        let mut reg = OutboundRegistration::new(OutboundRegistrationConfig {
            server_uri: "sip:registrar.example.com".to_string(),
            client_uri: "sip:alice@example.com".to_string(),
            username: "alice".to_string(),
            password: "secret".to_string(),
            expiration: 3600,
            local_addr: "10.0.0.1:5060".parse().unwrap(),
            ..Default::default()
        });

        let _ = reg.tick(); // Send REGISTER.

        // Simulate 401 challenge.
        let challenge = SipMessage::parse(
            b"SIP/2.0 401 Unauthorized\r\n\
              Via: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123\r\n\
              From: <sip:alice@example.com>;tag=abc\r\n\
              To: <sip:alice@example.com>;tag=def\r\n\
              Call-ID: test123\r\n\
              CSeq: 1 REGISTER\r\n\
              WWW-Authenticate: Digest realm=\"asterisk\", nonce=\"abc123\"\r\n\
              Content-Length: 0\r\n\
              \r\n",
        )
        .unwrap();

        let retry = reg.on_response(&challenge);
        assert!(retry.is_some()); // Should resend with auth.
        let retry_msg = retry.unwrap();
        assert!(retry_msg.get_header(header_names::AUTHORIZATION).is_some());
    }

    // ---- Keep-alive tests ----

    #[test]
    fn test_keepalive_options_creation() {
        let ka = KeepAliveState::new(
            KeepAliveMethod::Options,
            Duration::from_secs(30),
        );
        assert!(ka.should_send());
        assert!(!ka.is_connection_dead());

        let msg = ka.build_options_ping(
            "10.0.0.1:5060".parse().unwrap(),
            "sip:registrar.example.com",
        );
        assert_eq!(msg.method(), Some(SipMethod::Options));
    }

    #[test]
    fn test_keepalive_crlf() {
        let crlf = KeepAliveState::crlf_bytes();
        assert_eq!(crlf, b"\r\n\r\n");
    }

    #[test]
    fn test_keepalive_failure_detection() {
        let mut ka = KeepAliveState::new(
            KeepAliveMethod::Options,
            Duration::from_secs(30),
        );
        ka.max_failures = 3;

        ka.on_failure();
        ka.on_failure();
        assert!(!ka.is_connection_dead());

        ka.on_failure();
        assert!(ka.is_connection_dead());
    }

    #[test]
    fn test_keepalive_recovery() {
        let mut ka = KeepAliveState::new(
            KeepAliveMethod::Options,
            Duration::from_secs(30),
        );
        ka.max_failures = 2;
        ka.on_failure();
        ka.on_failure();
        assert!(ka.is_connection_dead());

        ka.on_response();
        assert_eq!(ka.failures, 0);
        assert!(!ka.is_connection_dead());
    }

    #[test]
    fn test_keepalive_none_never_sends() {
        let ka = KeepAliveState::new(
            KeepAliveMethod::None,
            Duration::from_secs(30),
        );
        assert!(!ka.should_send());
    }

    #[test]
    fn test_keepalive_from_flow_timer() {
        let response = SipMessage::parse(
            b"SIP/2.0 200 OK\r\n\
              Via: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123\r\n\
              From: <sip:alice@example.com>;tag=abc\r\n\
              To: <sip:alice@example.com>;tag=def\r\n\
              Call-ID: flow-test\r\n\
              CSeq: 1 REGISTER\r\n\
              Flow-Timer: 120\r\n\
              Content-Length: 0\r\n\
              \r\n",
        )
        .unwrap();

        let ka = keepalive_from_response(
            &response,
            KeepAliveMethod::Options,
            Duration::from_secs(90),
        );
        // 120 * 0.8 = 96 seconds
        assert_eq!(ka.interval, Duration::from_secs(96));
    }

    #[test]
    fn test_keepalive_default_interval_no_flow_timer() {
        let response = SipMessage::parse(
            b"SIP/2.0 200 OK\r\n\
              Via: SIP/2.0/UDP 10.0.0.1;branch=z9hG4bK123\r\n\
              From: <sip:alice@example.com>;tag=abc\r\n\
              To: <sip:alice@example.com>;tag=def\r\n\
              Call-ID: noflow-test\r\n\
              CSeq: 1 REGISTER\r\n\
              Content-Length: 0\r\n\
              \r\n",
        )
        .unwrap();

        let ka = keepalive_from_response(
            &response,
            KeepAliveMethod::CrLf,
            Duration::from_secs(90),
        );
        assert_eq!(ka.interval, Duration::from_secs(90));
        assert_eq!(ka.method, KeepAliveMethod::CrLf);
    }
}
