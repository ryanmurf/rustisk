//! ICE (Interactive Connectivity Establishment) implementation — RFC 8445.
//!
//! Provides full ICE agent with candidate gathering (host, server-reflexive,
//! relay), connectivity checks, nomination, and state machine management.
//! Supports both Full and Lite ICE modes.
//!
//! Port of the ICE integration in `res/res_rtp_asterisk.c` which delegates
//! to pjproject's ICE session. This is a native Rust implementation that
//! follows RFC 8445 directly.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use rand::Rng;
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

use crate::stun::{self, MessageClass, StunAttrValue, StunMessage, TransactionId};
use crate::turn::TurnClient;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Type preference values per RFC 8445 Section 5.1.2.1.
pub const TYPE_PREFERENCE_HOST: u32 = 126;
pub const TYPE_PREFERENCE_PEER_REFLEXIVE: u32 = 110;
pub const TYPE_PREFERENCE_SERVER_REFLEXIVE: u32 = 100;
pub const TYPE_PREFERENCE_RELAY: u32 = 0;

/// Default STUN connectivity check interval (Ta) in ms per RFC 8445 Section 14.
pub const DEFAULT_TA_MS: u64 = 50;

/// Maximum number of connectivity check retransmissions.
pub const MAX_CHECK_RETRANSMISSIONS: u32 = 7;

/// Connectivity check transaction timeout in ms.
pub const CHECK_TIMEOUT_MS: u64 = 500;

// ---------------------------------------------------------------------------
// Candidate types
// ---------------------------------------------------------------------------

/// ICE candidate type (RFC 8445 Section 5.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateType {
    /// A candidate whose transport address is a host address.
    Host,
    /// A candidate whose transport address is the server-reflexive
    /// (STUN mapped) address.
    ServerReflexive,
    /// A candidate discovered during connectivity checks.
    PeerReflexive,
    /// A candidate whose transport address is allocated by a TURN relay.
    Relay,
}

impl CandidateType {
    /// Type preference for priority calculation.
    pub fn type_preference(&self) -> u32 {
        match self {
            Self::Host => TYPE_PREFERENCE_HOST,
            Self::ServerReflexive => TYPE_PREFERENCE_SERVER_REFLEXIVE,
            Self::PeerReflexive => TYPE_PREFERENCE_PEER_REFLEXIVE,
            Self::Relay => TYPE_PREFERENCE_RELAY,
        }
    }

    /// SDP string representation.
    pub fn sdp_str(&self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::ServerReflexive => "srflx",
            Self::PeerReflexive => "prflx",
            Self::Relay => "relay",
        }
    }

    /// Parse from SDP string.
    pub fn from_sdp_str(s: &str) -> Option<Self> {
        match s {
            "host" => Some(Self::Host),
            "srflx" => Some(Self::ServerReflexive),
            "prflx" => Some(Self::PeerReflexive),
            "relay" => Some(Self::Relay),
            _ => None,
        }
    }
}

impl fmt::Display for CandidateType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.sdp_str())
    }
}

// ---------------------------------------------------------------------------
// ICE candidate
// ---------------------------------------------------------------------------

/// ICE component IDs (RFC 8445 Section 13).
pub const COMPONENT_RTP: u16 = 1;
pub const COMPONENT_RTCP: u16 = 2;

/// An ICE candidate (RFC 8445 Section 5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceCandidate {
    /// Foundation string — used for frozen/unfreezing logic.
    pub foundation: String,
    /// Component ID (1 = RTP, 2 = RTCP).
    pub component_id: u16,
    /// Transport protocol (usually "UDP").
    pub transport: String,
    /// Priority (computed per RFC 8445 Section 5.1.2).
    pub priority: u32,
    /// Transport address.
    pub address: SocketAddr,
    /// Candidate type.
    pub candidate_type: CandidateType,
    /// Related address (base address for srflx/prflx, relay for relay).
    pub related_address: Option<IpAddr>,
    /// Related port.
    pub related_port: Option<u16>,
}

impl IceCandidate {
    /// Compute candidate priority per RFC 8445 Section 5.1.2.
    ///
    /// `priority = (2^24) * type_preference + (2^8) * local_preference + (256 - component_id)`
    pub fn compute_priority(
        candidate_type: CandidateType,
        local_preference: u32,
        component_id: u16,
    ) -> u32 {
        let type_pref = candidate_type.type_preference();
        (type_pref << 24) | ((local_preference & 0xFFFF) << 8) | (256 - component_id as u32)
    }

    /// Create a host candidate.
    pub fn new_host(address: SocketAddr, component_id: u16, local_preference: u32) -> Self {
        let priority = Self::compute_priority(CandidateType::Host, local_preference, component_id);
        Self {
            foundation: format!("H{}{}", address.ip(), component_id),
            component_id,
            transport: "UDP".to_string(),
            priority,
            address,
            candidate_type: CandidateType::Host,
            related_address: None,
            related_port: None,
        }
    }

    /// Create a server-reflexive candidate.
    pub fn new_srflx(
        mapped_address: SocketAddr,
        base_address: SocketAddr,
        component_id: u16,
        local_preference: u32,
    ) -> Self {
        let priority =
            Self::compute_priority(CandidateType::ServerReflexive, local_preference, component_id);
        Self {
            foundation: format!("S{}{}", mapped_address.ip(), component_id),
            component_id,
            transport: "UDP".to_string(),
            priority,
            address: mapped_address,
            candidate_type: CandidateType::ServerReflexive,
            related_address: Some(base_address.ip()),
            related_port: Some(base_address.port()),
        }
    }

    /// Create a peer-reflexive candidate.
    pub fn new_prflx(
        address: SocketAddr,
        base_address: SocketAddr,
        component_id: u16,
        priority: u32,
    ) -> Self {
        Self {
            foundation: format!("P{}{}", address.ip(), component_id),
            component_id,
            transport: "UDP".to_string(),
            priority,
            address,
            candidate_type: CandidateType::PeerReflexive,
            related_address: Some(base_address.ip()),
            related_port: Some(base_address.port()),
        }
    }

    /// Create a relay candidate.
    pub fn new_relay(
        relayed_address: SocketAddr,
        mapped_address: SocketAddr,
        component_id: u16,
        local_preference: u32,
    ) -> Self {
        let priority =
            Self::compute_priority(CandidateType::Relay, local_preference, component_id);
        Self {
            foundation: format!("R{}{}", relayed_address.ip(), component_id),
            component_id,
            transport: "UDP".to_string(),
            priority,
            address: relayed_address,
            candidate_type: CandidateType::Relay,
            related_address: Some(mapped_address.ip()),
            related_port: Some(mapped_address.port()),
        }
    }

    /// Format as an SDP `a=candidate:` line (RFC 8839 Section 5.1).
    pub fn to_sdp_attribute(&self) -> String {
        let mut s = format!(
            "{} {} {} {} {} {} typ {}",
            self.foundation,
            self.component_id,
            self.transport,
            self.priority,
            self.address.ip(),
            self.address.port(),
            self.candidate_type.sdp_str(),
        );
        if let (Some(raddr), Some(rport)) = (self.related_address, self.related_port) {
            s.push_str(&format!(" raddr {} rport {}", raddr, rport));
        }
        s
    }

    /// Parse from SDP `a=candidate:` value (the part after `a=candidate:`).
    pub fn from_sdp_attribute(value: &str) -> Option<Self> {
        let parts: Vec<&str> = value.split_whitespace().collect();
        if parts.len() < 8 {
            return None;
        }

        let foundation = parts[0].to_string();
        let component_id: u16 = parts[1].parse().ok()?;
        let transport = parts[2].to_uppercase();
        let priority: u32 = parts[3].parse().ok()?;
        let ip: IpAddr = parts[4].parse().ok()?;
        let port: u16 = parts[5].parse().ok()?;
        // parts[6] should be "typ"
        if parts[6] != "typ" {
            return None;
        }
        let candidate_type = CandidateType::from_sdp_str(parts[7])?;

        let mut related_address = None;
        let mut related_port = None;

        // Parse optional raddr/rport
        let mut i = 8;
        while i + 1 < parts.len() {
            match parts[i] {
                "raddr" => {
                    related_address = parts[i + 1].parse().ok();
                    i += 2;
                }
                "rport" => {
                    related_port = parts[i + 1].parse().ok();
                    i += 2;
                }
                _ => {
                    i += 1;
                }
            }
        }

        Some(Self {
            foundation,
            component_id,
            transport,
            priority,
            address: SocketAddr::new(ip, port),
            candidate_type,
            related_address,
            related_port,
        })
    }
}

// ---------------------------------------------------------------------------
// Candidate pair
// ---------------------------------------------------------------------------

/// State of a candidate pair in the check list (RFC 8445 Section 6.1.2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairState {
    /// Check has not been performed, waiting to be scheduled.
    Frozen,
    /// Check is waiting to be sent.
    Waiting,
    /// Check has been sent, awaiting response.
    InProgress,
    /// Check succeeded.
    Succeeded,
    /// Check failed.
    Failed,
}

/// A candidate pair for connectivity checking (RFC 8445 Section 6.1.2).
#[derive(Debug, Clone)]
pub struct CandidatePair {
    /// Local candidate.
    pub local: IceCandidate,
    /// Remote candidate.
    pub remote: IceCandidate,
    /// Pair priority.
    pub priority: u64,
    /// Current state.
    pub state: PairState,
    /// Whether this pair has been nominated.
    pub nominated: bool,
    /// Transaction ID of the in-progress check.
    pub transaction_id: Option<TransactionId>,
    /// Number of retransmissions for the current check.
    pub retransmissions: u32,
}

impl CandidatePair {
    /// Compute pair priority per RFC 8445 Section 6.1.2.3.
    ///
    /// `pair_priority = 2^32 * min(G,D) + 2 * max(G,D) + (G > D ? 1 : 0)`
    ///
    /// where G = controlling candidate priority, D = controlled candidate priority.
    pub fn compute_priority(
        controlling_priority: u32,
        controlled_priority: u32,
    ) -> u64 {
        let g = controlling_priority as u64;
        let d = controlled_priority as u64;
        let min_val = g.min(d);
        let max_val = g.max(d);
        let tie = if g > d { 1u64 } else { 0u64 };
        (1u64 << 32) * min_val + 2 * max_val + tie
    }

    /// Create a new candidate pair.
    pub fn new(local: IceCandidate, remote: IceCandidate, role: IceRole) -> Self {
        let (g, d) = match role {
            IceRole::Controlling => (local.priority, remote.priority),
            IceRole::Controlled => (remote.priority, local.priority),
        };
        let priority = Self::compute_priority(g, d);
        Self {
            local,
            remote,
            priority,
            state: PairState::Frozen,
            nominated: false,
            transaction_id: None,
            retransmissions: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// ICE role and mode
// ---------------------------------------------------------------------------

/// ICE role during negotiation (RFC 8445 Section 5.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceRole {
    Controlling,
    Controlled,
}

/// ICE mode: Full or Lite (RFC 8445 Section 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceMode {
    /// Full ICE: performs connectivity checks.
    Full,
    /// Lite ICE: only host candidates, always controlled.
    Lite,
}

// ---------------------------------------------------------------------------
// ICE state machine
// ---------------------------------------------------------------------------

/// ICE agent state (RFC 8445 Section 6.1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceState {
    /// Initial state, not yet started.
    Idle,
    /// Gathering local candidates.
    Gathering,
    /// Running connectivity checks.
    Checking,
    /// ICE has completed successfully — a nominated pair exists for each
    /// component.
    Completed,
    /// ICE has failed.
    Failed,
}

// ---------------------------------------------------------------------------
// ICE options
// ---------------------------------------------------------------------------

/// ICE options from SDP `a=ice-options:`.
#[derive(Debug, Clone, Default)]
pub struct IceOptions {
    /// Trickle ICE (RFC 8838).
    pub trickle: bool,
    /// ICE renomination extension.
    pub renomination: bool,
    /// Raw option tokens.
    pub tokens: Vec<String>,
}

impl IceOptions {
    pub fn parse(value: &str) -> Self {
        let tokens: Vec<String> = value.split_whitespace().map(|s| s.to_string()).collect();
        let trickle = tokens.iter().any(|t| t == "trickle");
        let renomination = tokens.iter().any(|t| t == "renomination");
        Self {
            trickle,
            renomination,
            tokens,
        }
    }

    pub fn to_sdp_value(&self) -> String {
        self.tokens.join(" ")
    }
}

// ---------------------------------------------------------------------------
// Nomination mode
// ---------------------------------------------------------------------------

/// Nomination strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NominationMode {
    /// Regular nomination: perform checks first, then nominate with
    /// USE-CANDIDATE.
    Regular,
    /// Aggressive nomination: include USE-CANDIDATE in every check
    /// (deprecated in RFC 8445 but widely used).
    Aggressive,
}

// ---------------------------------------------------------------------------
// ICE Agent
// ---------------------------------------------------------------------------

/// An ICE agent that manages candidate gathering, pairing, and
/// connectivity checking (RFC 8445).
pub struct IceAgent {
    /// Our role.
    pub role: IceRole,
    /// ICE mode.
    pub mode: IceMode,
    /// Nomination strategy.
    pub nomination: NominationMode,
    /// Local ICE credentials.
    pub local_ufrag: String,
    pub local_pwd: String,
    /// Remote ICE credentials.
    pub remote_ufrag: String,
    pub remote_pwd: String,
    /// Local candidates.
    pub local_candidates: Vec<IceCandidate>,
    /// Remote candidates.
    pub remote_candidates: Vec<IceCandidate>,
    /// Check list — ordered by priority.
    pub check_list: Vec<CandidatePair>,
    /// Current state.
    pub state: IceState,
    /// Tie-breaker for role conflict resolution (random 64-bit).
    pub tie_breaker: u64,
    /// Number of ICE components (1 = RTP only, 2 = RTP + RTCP).
    pub num_components: u16,
    /// Nominated pairs per component.
    pub nominated_pairs: HashMap<u16, CandidatePair>,
    /// Valid pairs — pairs that have passed connectivity checks.
    pub valid_pairs: Vec<CandidatePair>,
}

impl fmt::Debug for IceAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IceAgent")
            .field("role", &self.role)
            .field("mode", &self.mode)
            .field("state", &self.state)
            .field("local_ufrag", &self.local_ufrag)
            .field("local_candidates", &self.local_candidates.len())
            .field("remote_candidates", &self.remote_candidates.len())
            .field("check_list", &self.check_list.len())
            .finish()
    }
}

impl IceAgent {
    /// Create a new ICE agent.
    pub fn new(mode: IceMode, role: IceRole, num_components: u16) -> Self {
        let mut rng = rand::thread_rng();

        // Generate ICE credentials: ufrag must be >= 4 chars, pwd >= 22 chars.
        let local_ufrag = generate_ice_string(4);
        let local_pwd = generate_ice_string(22);

        Self {
            role,
            mode,
            nomination: NominationMode::Aggressive,
            local_ufrag,
            local_pwd,
            remote_ufrag: String::new(),
            remote_pwd: String::new(),
            local_candidates: Vec::new(),
            remote_candidates: Vec::new(),
            check_list: Vec::new(),
            state: IceState::Idle,
            tie_breaker: rng.gen(),
            num_components,
            nominated_pairs: HashMap::new(),
            valid_pairs: Vec::new(),
        }
    }

    /// Create an ICE-Lite agent (always controlled, host candidates only).
    pub fn new_lite(num_components: u16) -> Self {
        let mut agent = Self::new(IceMode::Lite, IceRole::Controlled, num_components);
        agent.nomination = NominationMode::Regular;
        agent
    }

    /// Set remote credentials.
    pub fn set_remote_credentials(&mut self, ufrag: &str, pwd: &str) {
        let changed = self.remote_ufrag != ufrag || self.remote_pwd != pwd;
        self.remote_ufrag = ufrag.to_string();
        self.remote_pwd = pwd.to_string();
        if changed && self.state != IceState::Idle {
            // ICE restart: clear state
            debug!("ICE credentials changed, restarting");
            self.restart();
        }
    }

    /// Add a remote candidate.
    pub fn add_remote_candidate(&mut self, candidate: IceCandidate) {
        // Avoid duplicates
        if self
            .remote_candidates
            .iter()
            .any(|c| c.address == candidate.address && c.component_id == candidate.component_id)
        {
            return;
        }
        debug!(
            addr = %candidate.address,
            typ = %candidate.candidate_type,
            component = candidate.component_id,
            "ICE adding remote candidate"
        );
        self.remote_candidates.push(candidate);
    }

    // ----- Candidate Gathering -----

    /// Gather host candidates from local network interfaces.
    ///
    /// Enumerates all non-loopback IPv4 addresses and creates host candidates
    /// for each component using the given base port. The actual transport
    /// addresses come from the provided sockets or the caller is expected to
    /// bind sockets.
    pub fn gather_host_candidates(&mut self, local_addrs: &[SocketAddr]) {
        self.state = IceState::Gathering;

        for (idx, addr) in local_addrs.iter().enumerate() {
            for component in 1..=self.num_components {
                let local_pref = 65535 - idx as u32;
                let candidate = IceCandidate::new_host(*addr, component, local_pref);
                debug!(
                    addr = %addr,
                    component = component,
                    priority = candidate.priority,
                    "ICE gathered host candidate"
                );
                self.local_candidates.push(candidate);
            }
        }
    }

    /// Gather server-reflexive candidates by sending STUN Binding Requests
    /// to the specified STUN server.
    pub async fn gather_srflx_candidates(
        &mut self,
        stun_server: SocketAddr,
        socket: &UdpSocket,
        component_id: u16,
    ) -> Result<(), stun::StunError> {
        let local_addr = socket.local_addr()?;

        let request = StunMessage::binding_request();
        let bytes = request.to_bytes();
        let tid = request.transaction_id.clone();

        socket.send_to(&bytes, stun_server).await?;

        let mut buf = vec![0u8; stun::MAX_MESSAGE_SIZE];
        let timeout = Duration::from_millis(3000);
        let (len, _) = tokio::time::timeout(timeout, socket.recv_from(&mut buf))
            .await
            .map_err(|_| stun::StunError::Timeout)??;

        let response = StunMessage::parse(&buf[..len])?;
        if response.transaction_id != tid {
            return Err(stun::StunError::Parse("transaction ID mismatch".into()));
        }

        if response.class != MessageClass::SuccessResponse {
            return Err(stun::StunError::Parse("expected success response".into()));
        }

        if let Some(mapped) = response.get_mapped_address() {
            let candidate = IceCandidate::new_srflx(mapped, local_addr, component_id, 65535);
            debug!(
                mapped = %mapped,
                base = %local_addr,
                "ICE gathered srflx candidate"
            );
            self.local_candidates.push(candidate);
        }

        Ok(())
    }

    /// Gather relay candidates via TURN.
    pub async fn gather_relay_candidates(
        &mut self,
        turn_client: &mut TurnClient,
        component_id: u16,
    ) -> Result<(), crate::turn::TurnError> {
        let alloc = turn_client.allocate().await?;

        let candidate = IceCandidate::new_relay(
            alloc.relayed_addr,
            alloc.mapped_addr,
            component_id,
            65535,
        );
        debug!(
            relayed = %alloc.relayed_addr,
            mapped = %alloc.mapped_addr,
            "ICE gathered relay candidate"
        );
        self.local_candidates.push(candidate);
        Ok(())
    }

    // ----- Check list formation -----

    /// Form the check list by pairing local and remote candidates
    /// (RFC 8445 Section 6.1.2).
    pub fn form_check_list(&mut self) {
        self.check_list.clear();

        for local in &self.local_candidates {
            for remote in &self.remote_candidates {
                // Only pair candidates with same component ID and transport
                if local.component_id != remote.component_id {
                    continue;
                }
                if !local.transport.eq_ignore_ascii_case(&remote.transport) {
                    continue;
                }
                // Only pair same address family
                if local.address.is_ipv4() != remote.address.is_ipv4() {
                    continue;
                }

                let pair = CandidatePair::new(local.clone(), remote.clone(), self.role);
                self.check_list.push(pair);
            }
        }

        // Sort by priority (highest first)
        self.check_list
            .sort_by_key(|pair| std::cmp::Reverse(pair.priority));

        // Prune: remove lower-priority pairs with same local/remote addresses
        self.prune_check_list();

        // Set initial states per RFC 8445 Section 6.1.2.6:
        // - First pair for each foundation: Waiting
        // - All others: Frozen
        let mut seen_foundations = std::collections::HashSet::new();
        for pair in &mut self.check_list {
            let foundation_key = format!("{}:{}", pair.local.foundation, pair.remote.foundation);
            if seen_foundations.insert(foundation_key) {
                pair.state = PairState::Waiting;
            }
            // else remains Frozen
        }

        debug!(
            pairs = self.check_list.len(),
            "ICE check list formed"
        );
    }

    /// Prune redundant pairs from the check list.
    fn prune_check_list(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.check_list.retain(|pair| {
            let key = (
                pair.local.address,
                pair.remote.address,
                pair.local.component_id,
            );
            seen.insert(key)
        });
    }

    // ----- Connectivity checks -----

    /// Build a STUN Binding Request for a connectivity check.
    pub fn build_check_request(&self, pair: &CandidatePair) -> StunMessage {
        let mut msg = StunMessage::binding_request();

        // USERNAME = remote_ufrag:local_ufrag
        let username = format!("{}:{}", self.remote_ufrag, self.local_ufrag);
        msg.attributes.push(StunAttrValue::Username(username));

        // PRIORITY = computed priority for a peer-reflexive candidate
        let prflx_priority = IceCandidate::compute_priority(
            CandidateType::PeerReflexive,
            65535,
            pair.local.component_id,
        );
        msg.attributes.push(StunAttrValue::Priority(prflx_priority));

        // Role attributes
        match self.role {
            IceRole::Controlling => {
                msg.attributes
                    .push(StunAttrValue::IceControlling(self.tie_breaker));
                // Aggressive nomination: always include USE-CANDIDATE
                if self.nomination == NominationMode::Aggressive {
                    msg.attributes.push(StunAttrValue::UseCandidate);
                }
            }
            IceRole::Controlled => {
                msg.attributes
                    .push(StunAttrValue::IceControlled(self.tie_breaker));
            }
        }

        msg
    }

    /// Process an incoming STUN Binding Request (from a remote ICE check).
    ///
    /// Returns a response message and optionally a triggered check pair index.
    pub fn process_incoming_check(
        &mut self,
        request: &StunMessage,
        source: SocketAddr,
        local_addr: SocketAddr,
    ) -> (StunMessage, Option<usize>) {
        // Validate USERNAME
        let expected_username = format!("{}:{}", self.local_ufrag, self.remote_ufrag);
        if request.username() != Some(expected_username.as_str()) {
            return (
                StunMessage::binding_error(&request.transaction_id, 401, "Bad Username"),
                None,
            );
        }

        // Handle role conflict (RFC 8445 Section 7.3.1.1)
        if let Some(remote_tb) = request.ice_controlling() {
            if self.role == IceRole::Controlling {
                if self.tie_breaker >= remote_tb {
                    // We win: return 487 Role Conflict
                    return (
                        StunMessage::binding_error(
                            &request.transaction_id,
                            487,
                            "Role Conflict",
                        ),
                        None,
                    );
                } else {
                    // We lose: switch to controlled
                    debug!("ICE role conflict: switching to controlled");
                    self.role = IceRole::Controlled;
                }
            }
        }
        if let Some(remote_tb) = request.ice_controlled() {
            if self.role == IceRole::Controlled {
                if self.tie_breaker >= remote_tb {
                    // We win: switch to controlling
                    debug!("ICE role conflict: switching to controlling");
                    self.role = IceRole::Controlling;
                } else {
                    // We lose: return 487
                    return (
                        StunMessage::binding_error(
                            &request.transaction_id,
                            487,
                            "Role Conflict",
                        ),
                        None,
                    );
                }
            }
        }

        // Build success response
        let response = StunMessage::binding_response(&request.transaction_id, source);

        // Triggered check: find or create a pair for this source
        let triggered_idx = self.handle_triggered_check(source, local_addr, request);

        // Handle nomination
        if request.has_use_candidate() {
            if let Some(idx) = triggered_idx {
                if self.check_list[idx].state == PairState::Succeeded {
                    self.check_list[idx].nominated = true;
                    let pair = self.check_list[idx].clone();
                    self.nominated_pairs.insert(pair.local.component_id, pair);
                    self.update_state();
                }
            }
        }

        (response, triggered_idx)
    }

    /// Handle a triggered check from an incoming STUN request.
    /// Returns the index of the pair in the check list.
    fn handle_triggered_check(
        &mut self,
        source: SocketAddr,
        local_addr: SocketAddr,
        request: &StunMessage,
    ) -> Option<usize> {
        // Look for an existing pair matching source -> local_addr
        let existing = self.check_list.iter().position(|p| {
            p.remote.address == source && p.local.address == local_addr
        });

        if let Some(idx) = existing {
            // If the pair is in Frozen or Waiting, move to Waiting for triggered check
            if self.check_list[idx].state == PairState::Frozen {
                self.check_list[idx].state = PairState::Waiting;
            }
            return Some(idx);
        }

        // Pair not found — learn a new peer-reflexive candidate
        let component_id = self
            .local_candidates
            .iter()
            .find(|c| c.address == local_addr)
            .map(|c| c.component_id)
            .unwrap_or(COMPONENT_RTP);

        let prflx_priority = request.priority().unwrap_or_else(|| {
            IceCandidate::compute_priority(CandidateType::PeerReflexive, 65535, component_id)
        });

        let remote_candidate = IceCandidate::new_prflx(
            source,
            source, // For remote, base is itself
            component_id,
            prflx_priority,
        );

        debug!(
            addr = %source,
            "ICE discovered peer-reflexive remote candidate"
        );
        self.remote_candidates.push(remote_candidate.clone());

        // Find the local candidate
        let local = self
            .local_candidates
            .iter()
            .find(|c| c.address == local_addr && c.component_id == component_id)
            .cloned();

        if let Some(local) = local {
            let mut pair = CandidatePair::new(local, remote_candidate, self.role);
            pair.state = PairState::Waiting;
            self.check_list.push(pair);
            Some(self.check_list.len() - 1)
        } else {
            None
        }
    }

    /// Process a STUN Binding Response to a connectivity check.
    pub fn process_check_response(
        &mut self,
        response: &StunMessage,
        pair_idx: usize,
    ) {
        if pair_idx >= self.check_list.len() {
            return;
        }

        match response.class {
            MessageClass::SuccessResponse => {
                self.check_list[pair_idx].state = PairState::Succeeded;
                let pair = self.check_list[pair_idx].clone();

                debug!(
                    local = %pair.local.address,
                    remote = %pair.remote.address,
                    "ICE check succeeded"
                );

                self.valid_pairs.push(pair.clone());

                // Check for peer-reflexive discovery
                if let Some(mapped) = response.get_mapped_address() {
                    if mapped != pair.local.address {
                        // Discovered a new peer-reflexive local candidate
                        let prflx = IceCandidate::new_prflx(
                            mapped,
                            pair.local.address,
                            pair.local.component_id,
                            pair.local.priority,
                        );
                        debug!(
                            mapped = %mapped,
                            "ICE discovered peer-reflexive local candidate"
                        );
                        if !self.local_candidates.iter().any(|c| c.address == mapped) {
                            self.local_candidates.push(prflx);
                        }
                    }
                }

                // Handle nomination
                if self.nomination == NominationMode::Aggressive
                    && self.role == IceRole::Controlling
                {
                    self.check_list[pair_idx].nominated = true;
                    self.nominated_pairs
                        .insert(pair.local.component_id, pair);
                }

                // Unfreeze pairs with the same foundation
                self.unfreeze_related(pair_idx);
                self.update_state();
            }
            MessageClass::ErrorResponse => {
                if let Some((code, _reason)) = response.error_code() {
                    if code == 487 {
                        // Role conflict — switch roles and retry
                        self.role = match self.role {
                            IceRole::Controlling => IceRole::Controlled,
                            IceRole::Controlled => IceRole::Controlling,
                        };
                        debug!(new_role = ?self.role, "ICE role conflict, switching");
                        self.check_list[pair_idx].state = PairState::Waiting;
                        return;
                    }
                }
                self.check_list[pair_idx].state = PairState::Failed;
                debug!(
                    local = %self.check_list[pair_idx].local.address,
                    remote = %self.check_list[pair_idx].remote.address,
                    "ICE check failed"
                );
                self.update_state();
            }
            _ => {}
        }
    }

    /// Nominate a succeeded pair (for regular nomination).
    pub fn nominate(&mut self, pair_idx: usize) {
        if pair_idx < self.check_list.len()
            && self.check_list[pair_idx].state == PairState::Succeeded
        {
            self.check_list[pair_idx].nominated = true;
            let pair = self.check_list[pair_idx].clone();
            self.nominated_pairs
                .insert(pair.local.component_id, pair);
            self.update_state();
        }
    }

    /// Unfreeze pairs with matching foundation after a successful check.
    fn unfreeze_related(&mut self, succeeded_idx: usize) {
        let succeeded_foundation = (
            self.check_list[succeeded_idx].local.foundation.clone(),
            self.check_list[succeeded_idx].remote.foundation.clone(),
        );

        for i in 0..self.check_list.len() {
            if i == succeeded_idx {
                continue;
            }
            if self.check_list[i].state == PairState::Frozen
                && self.check_list[i].local.foundation == succeeded_foundation.0
            {
                self.check_list[i].state = PairState::Waiting;
            }
        }
    }

    /// Update the overall ICE state based on check list.
    fn update_state(&mut self) {
        // Check if all components have nominated pairs
        let all_nominated = (1..=self.num_components)
            .all(|comp| self.nominated_pairs.contains_key(&comp));

        if all_nominated {
            self.state = IceState::Completed;
            info!("ICE completed");
            return;
        }

        // Check if all pairs have failed
        let all_done = self.check_list.iter().all(|p| {
            p.state == PairState::Succeeded || p.state == PairState::Failed
        });

        if all_done && !all_nominated {
            // Check if any succeeded
            let any_succeeded = self.check_list.iter().any(|p| p.state == PairState::Succeeded);
            if !any_succeeded {
                self.state = IceState::Failed;
                warn!("ICE failed: all pairs failed");
            }
        }
    }

    /// Get the next pair that needs a connectivity check.
    pub fn next_check_pair(&mut self) -> Option<usize> {
        // First look for Waiting pairs
        for (i, pair) in self.check_list.iter().enumerate() {
            if pair.state == PairState::Waiting {
                return Some(i);
            }
        }
        // Then unfreeze and check Frozen pairs
        for (i, pair) in self.check_list.iter_mut().enumerate() {
            if pair.state == PairState::Frozen {
                pair.state = PairState::Waiting;
                return Some(i);
            }
        }
        None
    }

    /// Mark a pair as in-progress.
    pub fn mark_in_progress(&mut self, pair_idx: usize, tid: TransactionId) {
        if pair_idx < self.check_list.len() {
            self.check_list[pair_idx].state = PairState::InProgress;
            self.check_list[pair_idx].transaction_id = Some(tid);
        }
    }

    /// Find a pair by its transaction ID.
    pub fn find_pair_by_transaction(&self, tid: &TransactionId) -> Option<usize> {
        self.check_list
            .iter()
            .position(|p| p.transaction_id.as_ref() == Some(tid))
    }

    /// ICE restart — reset state while keeping credentials.
    pub fn restart(&mut self) {
        self.local_ufrag = generate_ice_string(4);
        self.local_pwd = generate_ice_string(22);
        self.local_candidates.clear();
        self.remote_candidates.clear();
        self.check_list.clear();
        self.nominated_pairs.clear();
        self.valid_pairs.clear();
        self.state = IceState::Idle;
        self.tie_breaker = rand::thread_rng().gen();
    }

    /// Get the nominated pair for a component.
    pub fn nominated_pair(&self, component_id: u16) -> Option<&CandidatePair> {
        self.nominated_pairs.get(&component_id)
    }

    /// Get the best valid (succeeded) pair for a component.
    pub fn best_valid_pair(&self, component_id: u16) -> Option<&CandidatePair> {
        self.valid_pairs
            .iter()
            .filter(|p| p.local.component_id == component_id)
            .max_by_key(|p| p.priority)
    }

    /// Start ICE checking.
    pub fn start_checking(&mut self) {
        if self.state == IceState::Gathering || self.state == IceState::Idle {
            self.form_check_list();
            self.state = IceState::Checking;
            debug!(
                pairs = self.check_list.len(),
                "ICE checking started"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// ICE credential generation
// ---------------------------------------------------------------------------

/// Generate a random ICE string of at least `min_len` characters.
///
/// Uses alphanumeric characters + '+' and '/' per RFC 8445 Section 5.3.
fn generate_ice_string(min_len: usize) -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/";
    let mut rng = rand::thread_rng();
    (0..min_len)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

// ---------------------------------------------------------------------------
// Gather local network addresses (platform helper)
// ---------------------------------------------------------------------------

/// Enumerate local non-loopback IPv4 addresses.
///
/// This is a simplified version; a production implementation would use
/// `getifaddrs` on Unix or platform-specific APIs.
pub fn enumerate_local_addresses() -> Vec<IpAddr> {
    // Use a simple approach: try to connect UDP to a public address
    // and see what local address is used.
    let mut addrs = Vec::new();

    // Try to discover our address by connecting to a well-known IP
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(local) = sock.local_addr() {
                if !local.ip().is_loopback() && !local.ip().is_unspecified() {
                    addrs.push(local.ip());
                }
            }
        }
    }

    // Also include loopback for testing
    if addrs.is_empty() {
        addrs.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    addrs
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stun::{Method, MessageClass};

    #[test]
    fn test_candidate_priority_host() {
        // Host, local_pref=65535, component=1
        // priority = 126 * 2^24 + 65535 * 2^8 + (256 - 1)
        //          = 2113929216 + 16776960 + 255
        //          = 2130706431
        let prio = IceCandidate::compute_priority(CandidateType::Host, 65535, 1);
        assert_eq!(prio, 2130706431);
    }

    #[test]
    fn test_candidate_priority_srflx() {
        // Srflx, local_pref=65535, component=1
        // priority = 100 * 2^24 + 65535 * 2^8 + 255
        //          = 1677721600 + 16776960 + 255
        //          = 1694498815
        let prio = IceCandidate::compute_priority(CandidateType::ServerReflexive, 65535, 1);
        assert_eq!(prio, 1694498815);
    }

    #[test]
    fn test_candidate_priority_relay() {
        // Relay, local_pref=65535, component=1
        // priority = 0 * 2^24 + 65535 * 2^8 + 255
        //          = 16777215
        let prio = IceCandidate::compute_priority(CandidateType::Relay, 65535, 1);
        assert_eq!(prio, 16777215);
    }

    #[test]
    fn test_candidate_priority_component2() {
        // Host, local_pref=65535, component=2
        // priority = 126 * 2^24 + 65535 * 2^8 + (256 - 2) = 254
        let prio = IceCandidate::compute_priority(CandidateType::Host, 65535, 2);
        assert_eq!(prio, 2130706430);
    }

    #[test]
    fn test_pair_priority() {
        // G=2130706431 (host), D=1694498815 (srflx)
        // min=1694498815, max=2130706431
        // pair_priority = 2^32 * 1694498815 + 2 * 2130706431 + 1
        //               = 7277816997156249600 + 4261412862 + 1
        //               = 7277816997156249600 + 4261412863
        let priority = CandidatePair::compute_priority(2130706431, 1694498815);
        let expected = (1u64 << 32) * 1694498815 + 2 * 2130706431 + 1;
        assert_eq!(priority, expected);
    }

    #[test]
    fn test_pair_priority_symmetric() {
        // When G == D, tie-breaker is 0
        let p = CandidatePair::compute_priority(100, 100);
        let expected = (1u64 << 32) * 100 + 2 * 100 + 0;
        assert_eq!(p, expected);
    }

    #[test]
    fn test_candidate_sdp_roundtrip() {
        let candidate = IceCandidate::new_host(
            "192.168.1.100:5060".parse().unwrap(),
            1,
            65535,
        );
        let sdp = candidate.to_sdp_attribute();
        let parsed = IceCandidate::from_sdp_attribute(&sdp).unwrap();
        assert_eq!(parsed.foundation, candidate.foundation);
        assert_eq!(parsed.component_id, 1);
        assert_eq!(parsed.priority, candidate.priority);
        assert_eq!(parsed.address, candidate.address);
        assert_eq!(parsed.candidate_type, CandidateType::Host);
    }

    #[test]
    fn test_candidate_sdp_srflx() {
        let sdp_value = "S203.0.113.501 1 UDP 1694498815 203.0.113.50 12345 typ srflx raddr 192.168.1.100 rport 5060";
        let candidate = IceCandidate::from_sdp_attribute(sdp_value).unwrap();
        assert_eq!(candidate.candidate_type, CandidateType::ServerReflexive);
        assert_eq!(candidate.address, "203.0.113.50:12345".parse::<SocketAddr>().unwrap());
        assert_eq!(candidate.related_address, Some("192.168.1.100".parse().unwrap()));
        assert_eq!(candidate.related_port, Some(5060));
    }

    #[test]
    fn test_check_list_formation() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        agent.local_candidates.push(IceCandidate::new_host(
            "192.168.1.1:5000".parse().unwrap(),
            1,
            65535,
        ));
        agent.local_candidates.push(IceCandidate::new_srflx(
            "203.0.113.1:5000".parse().unwrap(),
            "192.168.1.1:5000".parse().unwrap(),
            1,
            65534,
        ));

        agent.add_remote_candidate(IceCandidate::new_host(
            "10.0.0.1:6000".parse().unwrap(),
            1,
            65535,
        ));

        agent.form_check_list();

        assert_eq!(agent.check_list.len(), 2);
        // Should be sorted by priority (highest first)
        assert!(agent.check_list[0].priority >= agent.check_list[1].priority);
        // First pair should be Waiting
        assert_eq!(agent.check_list[0].state, PairState::Waiting);
    }

    #[test]
    fn test_ice_state_transitions() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        assert_eq!(agent.state, IceState::Idle);

        agent.gather_host_candidates(&["192.168.1.1:5000".parse().unwrap()]);
        assert_eq!(agent.state, IceState::Gathering);

        agent.add_remote_candidate(IceCandidate::new_host(
            "10.0.0.1:6000".parse().unwrap(),
            1,
            65535,
        ));

        agent.start_checking();
        assert_eq!(agent.state, IceState::Checking);
    }

    #[test]
    fn test_ice_credentials() {
        let agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        assert!(agent.local_ufrag.len() >= 4);
        assert!(agent.local_pwd.len() >= 22);
    }

    #[test]
    fn test_ice_lite_agent() {
        let agent = IceAgent::new_lite(1);
        assert_eq!(agent.mode, IceMode::Lite);
        assert_eq!(agent.role, IceRole::Controlled);
    }

    #[test]
    fn test_check_request_building() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        agent.remote_ufrag = "remoteufrag".to_string();
        agent.local_ufrag = "localufrag".to_string();

        let local = IceCandidate::new_host("192.168.1.1:5000".parse().unwrap(), 1, 65535);
        let remote = IceCandidate::new_host("10.0.0.1:6000".parse().unwrap(), 1, 65535);
        let pair = CandidatePair::new(local, remote, IceRole::Controlling);

        let request = agent.build_check_request(&pair);
        assert_eq!(request.method, Method::Binding);
        assert_eq!(request.class, MessageClass::Request);
        assert_eq!(request.username(), Some("remoteufrag:localufrag"));
        assert!(request.priority().is_some());
        assert!(request.ice_controlling().is_some());
        // Aggressive nomination
        assert!(request.has_use_candidate());
    }

    #[test]
    fn test_nomination_completion() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        let local = IceCandidate::new_host("192.168.1.1:5000".parse().unwrap(), 1, 65535);
        let remote = IceCandidate::new_host("10.0.0.1:6000".parse().unwrap(), 1, 65535);

        agent.local_candidates.push(local.clone());
        agent.add_remote_candidate(remote.clone());
        agent.start_checking();

        // Simulate a succeeded check
        assert!(!agent.check_list.is_empty());
        agent.check_list[0].state = PairState::Succeeded;

        // Nominate
        agent.nominate(0);
        assert_eq!(agent.state, IceState::Completed);
        assert!(agent.nominated_pair(1).is_some());
    }

    #[test]
    fn test_ice_restart() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        let old_ufrag = agent.local_ufrag.clone();

        agent.gather_host_candidates(&["192.168.1.1:5000".parse().unwrap()]);
        assert!(!agent.local_candidates.is_empty());

        agent.restart();
        assert_eq!(agent.state, IceState::Idle);
        assert!(agent.local_candidates.is_empty());
        assert_ne!(agent.local_ufrag, old_ufrag);
    }

    #[test]
    fn test_generate_ice_string() {
        let s = generate_ice_string(22);
        assert!(s.len() >= 22);
        // All characters should be valid
        for c in s.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '+' || c == '/',
                "invalid char: {}",
                c
            );
        }
    }

    #[test]
    fn test_candidate_type_sdp() {
        assert_eq!(CandidateType::Host.sdp_str(), "host");
        assert_eq!(CandidateType::ServerReflexive.sdp_str(), "srflx");
        assert_eq!(CandidateType::PeerReflexive.sdp_str(), "prflx");
        assert_eq!(CandidateType::Relay.sdp_str(), "relay");

        assert_eq!(CandidateType::from_sdp_str("host"), Some(CandidateType::Host));
        assert_eq!(CandidateType::from_sdp_str("srflx"), Some(CandidateType::ServerReflexive));
        assert_eq!(CandidateType::from_sdp_str("unknown"), None);
    }

    // -----------------------------------------------------------------------
    // ADVERSARIAL ICE TESTS
    // -----------------------------------------------------------------------

    #[test]
    fn test_ice_role_conflict_both_controlling() {
        // Both sides claim Controlling. The side with the lower tie-breaker
        // should switch to Controlled and the higher returns 487.
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        agent.tie_breaker = 100; // Our tie-breaker
        agent.local_ufrag = "local".to_string();
        agent.remote_ufrag = "remote".to_string();
        agent.local_pwd = "localpassword1234567890".to_string();
        agent.remote_pwd = "remotepassword1234567890".to_string();

        agent.gather_host_candidates(&["192.168.1.1:5000".parse().unwrap()]);
        agent.add_remote_candidate(IceCandidate::new_host(
            "10.0.0.1:6000".parse().unwrap(), 1, 65535,
        ));
        agent.start_checking();

        // Incoming check from remote who also claims Controlling with HIGHER tie-breaker
        let mut request = StunMessage::binding_request();
        request.attributes.push(StunAttrValue::Username("local:remote".to_string()));
        request.attributes.push(StunAttrValue::Priority(2130706431));
        request.attributes.push(StunAttrValue::IceControlling(200)); // Higher

        let (response, _) = agent.process_incoming_check(
            &request,
            "10.0.0.1:6000".parse().unwrap(),
            "192.168.1.1:5000".parse().unwrap(),
        );
        // We have lower tie-breaker (100 < 200), so WE switch to controlled
        assert_eq!(agent.role, IceRole::Controlled);
        // Response should be success (not 487)
        assert_eq!(response.class, MessageClass::SuccessResponse);
    }

    #[test]
    fn test_ice_role_conflict_we_win() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        agent.tie_breaker = 200; // Our tie-breaker (higher)
        agent.local_ufrag = "local".to_string();
        agent.remote_ufrag = "remote".to_string();
        agent.local_pwd = "localpassword1234567890".to_string();
        agent.remote_pwd = "remotepassword1234567890".to_string();

        agent.gather_host_candidates(&["192.168.1.1:5000".parse().unwrap()]);

        let mut request = StunMessage::binding_request();
        request.attributes.push(StunAttrValue::Username("local:remote".to_string()));
        request.attributes.push(StunAttrValue::Priority(2130706431));
        request.attributes.push(StunAttrValue::IceControlling(100)); // Lower

        let (response, _) = agent.process_incoming_check(
            &request,
            "10.0.0.1:6000".parse().unwrap(),
            "192.168.1.1:5000".parse().unwrap(),
        );
        // We have higher tie-breaker, we win => return 487
        assert_eq!(response.class, MessageClass::ErrorResponse);
        assert_eq!(response.error_code().unwrap().0, 487);
        assert_eq!(agent.role, IceRole::Controlling); // We stay Controlling
    }

    #[test]
    fn test_ice_candidate_priority_zero() {
        let prio = IceCandidate::compute_priority(CandidateType::Relay, 0, 1);
        // Relay type_pref=0, local_pref=0, component=1
        // = 0 << 24 | 0 << 8 | (256-1) = 255
        assert_eq!(prio, 255);
    }

    #[test]
    fn test_ice_candidate_priority_max() {
        let prio = IceCandidate::compute_priority(CandidateType::Host, 65535, 1);
        assert_eq!(prio, 2130706431);
        // Verify this is close to 2^31 (max safe priority)
        assert!(prio > (1u32 << 30));
    }

    #[test]
    fn test_ice_empty_candidate_list() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        // No local or remote candidates
        agent.form_check_list();
        assert!(agent.check_list.is_empty());
        // Should not crash, and state should reflect failure if checking starts
        // with empty list
    }

    #[test]
    fn test_ice_all_pairs_failed() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        agent.gather_host_candidates(&["192.168.1.1:5000".parse().unwrap()]);
        agent.add_remote_candidate(IceCandidate::new_host(
            "10.0.0.1:6000".parse().unwrap(), 1, 65535,
        ));
        agent.start_checking();

        // Fail all pairs
        for i in 0..agent.check_list.len() {
            agent.check_list[i].state = PairState::Failed;
        }
        agent.update_state();
        assert_eq!(agent.state, IceState::Failed);
    }

    #[test]
    fn test_ice_nomination_before_success_ignored() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        agent.gather_host_candidates(&["192.168.1.1:5000".parse().unwrap()]);
        agent.add_remote_candidate(IceCandidate::new_host(
            "10.0.0.1:6000".parse().unwrap(), 1, 65535,
        ));
        agent.start_checking();

        // Try to nominate a pair that hasn't succeeded
        assert_eq!(agent.check_list[0].state, PairState::Waiting);
        agent.nominate(0);
        // Should NOT be nominated since state is not Succeeded
        assert!(!agent.check_list[0].nominated);
        assert!(agent.nominated_pairs.is_empty());
    }

    #[test]
    fn test_ice_restart_clears_all_state() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        let old_ufrag = agent.local_ufrag.clone();
        let old_pwd = agent.local_pwd.clone();
        let old_tb = agent.tie_breaker;

        agent.gather_host_candidates(&["192.168.1.1:5000".parse().unwrap()]);
        agent.add_remote_candidate(IceCandidate::new_host(
            "10.0.0.1:6000".parse().unwrap(), 1, 65535,
        ));
        agent.start_checking();
        // Simulate nomination
        agent.check_list[0].state = PairState::Succeeded;
        agent.nominate(0);

        agent.restart();

        assert_eq!(agent.state, IceState::Idle);
        assert!(agent.local_candidates.is_empty());
        assert!(agent.remote_candidates.is_empty());
        assert!(agent.check_list.is_empty());
        assert!(agent.nominated_pairs.is_empty());
        assert!(agent.valid_pairs.is_empty());
        assert_ne!(agent.local_ufrag, old_ufrag);
        assert_ne!(agent.local_pwd, old_pwd);
        assert_ne!(agent.tie_breaker, old_tb);
    }

    #[test]
    fn test_ice_duplicate_candidate_deduplicated() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        let candidate = IceCandidate::new_host(
            "10.0.0.1:6000".parse().unwrap(), 1, 65535,
        );

        agent.add_remote_candidate(candidate.clone());
        agent.add_remote_candidate(candidate.clone());
        agent.add_remote_candidate(candidate);

        assert_eq!(agent.remote_candidates.len(), 1, "Duplicates must be deduplicated");
    }

    #[test]
    fn test_ice_487_role_conflict_response_triggers_retry() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);
        agent.gather_host_candidates(&["192.168.1.1:5000".parse().unwrap()]);
        agent.add_remote_candidate(IceCandidate::new_host(
            "10.0.0.1:6000".parse().unwrap(), 1, 65535,
        ));
        agent.start_checking();

        let error_response = StunMessage::binding_error(
            &agent.check_list[0].transaction_id.clone().unwrap_or_default(),
            487,
            "Role Conflict",
        );

        let old_role = agent.role;
        agent.process_check_response(&error_response, 0);

        // Role should have switched
        assert_ne!(agent.role, old_role);
        // Pair should be set to Waiting for retry
        assert_eq!(agent.check_list[0].state, PairState::Waiting);
    }

    #[test]
    fn test_ice_check_list_pruning() {
        let mut agent = IceAgent::new(IceMode::Full, IceRole::Controlling, 1);

        // Add two local candidates with same address but different types
        let host = IceCandidate::new_host(
            "192.168.1.1:5000".parse().unwrap(), 1, 65535,
        );
        let srflx = IceCandidate::new_srflx(
            "192.168.1.1:5000".parse().unwrap(),
            "192.168.1.1:5000".parse().unwrap(),
            1, 65534,
        );
        agent.local_candidates.push(host);
        agent.local_candidates.push(srflx);

        agent.add_remote_candidate(IceCandidate::new_host(
            "10.0.0.1:6000".parse().unwrap(), 1, 65535,
        ));

        agent.form_check_list();

        // Both local candidates have the same address, so pruning should
        // remove the lower-priority duplicate
        assert_eq!(agent.check_list.len(), 1, "Redundant pairs should be pruned");
    }
}
