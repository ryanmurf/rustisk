//! SIP transaction state machines (RFC 3261 Section 17).
//!
//! Implements INVITE and non-INVITE client/server transaction state machines
//! with proper timer management and retransmission handling.
//!
//! Timer runtime:
//! - `ClientTransaction::start()`: spawns a tokio task that drives Timer A
//!   (retransmission) and Timer B (timeout) for INVITE client transactions.
//! - `ServerTransaction::start()`: spawns a tokio task that drives Timer H
//!   (ACK wait) for INVITE server transactions.
//! - Non-INVITE client: Timer E (retransmit) and Timer F (timeout).
//!
//! Timer values per RFC 3261:
//! - Timer A: starts at T1 (500ms), doubles up to T2 (4s) for INVITE retransmit
//! - Timer B: 64*T1 (32s) INVITE client timeout
//! - Timer E: starts at T1, doubles up to T2 for non-INVITE retransmit
//! - Timer F: 64*T1 (32s) non-INVITE client timeout
//! - Timer G: starts at T1, doubles for response retransmit (server)
//! - Timer H: 64*T1 (32s) server ACK wait
//! - Timer I: T4 (5s) for ACK retransmit absorption
//! - Timer J: 64*T1 for non-INVITE request retransmit absorption
//! - Timer K: T4 for response retransmit absorption

use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::debug;

use crate::parser::SipMessage;

/// RFC 3261 timer values.
pub mod timers {
    use std::time::Duration;

    /// T1: RTT estimate (default 500ms).
    pub const T1: Duration = Duration::from_millis(500);
    /// T2: Maximum retransmit interval for non-INVITE requests and INVITE responses.
    pub const T2: Duration = Duration::from_secs(4);
    /// T4: Maximum duration a message will remain in the network.
    pub const T4: Duration = Duration::from_secs(5);
    /// Timer A: INVITE request retransmit interval (initially T1, doubles).
    pub const TIMER_A_INITIAL: Duration = T1;
    /// Timer B: INVITE transaction timeout (64*T1 = 32s).
    pub const TIMER_B: Duration = Duration::from_secs(32);
    /// Timer D: Wait time for response retransmits (>32s for UDP, 0s for TCP).
    pub const TIMER_D_UDP: Duration = Duration::from_secs(32);
    pub const TIMER_D_TCP: Duration = Duration::from_secs(0);
    /// Timer E: non-INVITE request retransmit interval (initially T1).
    pub const TIMER_E_INITIAL: Duration = T1;
    /// Timer F: non-INVITE transaction timeout (64*T1).
    pub const TIMER_F: Duration = Duration::from_secs(32);
    /// Timer H: Wait time for ACK receipt (64*T1).
    pub const TIMER_H: Duration = Duration::from_secs(32);
    /// Timer I: Wait time for ACK retransmits (T4 for UDP, 0 for TCP).
    pub const TIMER_I_UDP: Duration = T4;
    pub const TIMER_I_TCP: Duration = Duration::from_secs(0);
    /// Timer J: Wait time for non-INVITE request retransmits (64*T1 for UDP, 0 for TCP).
    pub const TIMER_J_UDP: Duration = Duration::from_secs(32);
    pub const TIMER_J_TCP: Duration = Duration::from_secs(0);
    /// Timer K: Wait time for response retransmits (T4 for UDP, 0 for TCP).
    pub const TIMER_K_UDP: Duration = T4;
    pub const TIMER_K_TCP: Duration = Duration::from_secs(0);
}

/// Transaction state for INVITE client transactions (RFC 3261 Figure 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteClientState {
    /// Initial state: INVITE sent, waiting for response.
    Calling,
    /// 1xx received, waiting for final response.
    Proceeding,
    /// 2xx received (success) or non-2xx received.
    Completed,
    /// ACK sent for non-2xx, waiting to absorb retransmissions.
    Terminated,
}

/// Transaction state for INVITE server transactions (RFC 3261 Figure 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteServerState {
    /// INVITE received, processing.
    Proceeding,
    /// Final response sent (non-2xx), waiting for ACK.
    Completed,
    /// ACK received for non-2xx response.
    Confirmed,
    /// Transaction done.
    Terminated,
}

/// Transaction state for non-INVITE client transactions (RFC 3261 Figure 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonInviteClientState {
    /// Request sent, waiting for response.
    Trying,
    /// 1xx received.
    Proceeding,
    /// Final response received.
    Completed,
    /// Done.
    Terminated,
}

/// Transaction state for non-INVITE server transactions (RFC 3261 Figure 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonInviteServerState {
    /// Request received, processing.
    Trying,
    /// 1xx sent.
    Proceeding,
    /// Final response sent.
    Completed,
    /// Done.
    Terminated,
}

/// An INVITE client transaction.
#[derive(Debug)]
pub struct ClientTransaction {
    pub branch: String,
    pub state: InviteClientState,
    pub request: SipMessage,
    pub remote_addr: SocketAddr,
    timer_a_interval: Duration,
    started_at: Instant,
    last_retransmit: Instant,
    pub last_response: Option<SipMessage>,
}

impl ClientTransaction {
    /// Create a new INVITE client transaction.
    pub fn new(request: SipMessage, remote_addr: SocketAddr, branch: String) -> Self {
        let now = Instant::now();
        Self {
            branch,
            state: InviteClientState::Calling,
            request,
            remote_addr,
            timer_a_interval: timers::TIMER_A_INITIAL,
            started_at: now,
            last_retransmit: now,
            last_response: None,
        }
    }

    /// Process a received response.
    pub fn on_response(&mut self, response: SipMessage) {
        let status = response.status_code().unwrap_or(0);
        self.last_response = Some(response);

        match self.state {
            InviteClientState::Calling => {
                if (100..200).contains(&status) {
                    self.state = InviteClientState::Proceeding;
                    debug!(branch = %self.branch, status, "INVITE client -> Proceeding");
                } else if (200..300).contains(&status) {
                    self.state = InviteClientState::Terminated;
                    debug!(branch = %self.branch, status, "INVITE client -> Terminated (2xx)");
                } else if (300..700).contains(&status) {
                    self.state = InviteClientState::Completed;
                    debug!(branch = %self.branch, status, "INVITE client -> Completed");
                }
            }
            InviteClientState::Proceeding => {
                if (200..300).contains(&status) {
                    self.state = InviteClientState::Terminated;
                    debug!(branch = %self.branch, status, "INVITE client -> Terminated (2xx)");
                } else if (300..700).contains(&status) {
                    self.state = InviteClientState::Completed;
                    debug!(branch = %self.branch, status, "INVITE client -> Completed");
                }
            }
            _ => {}
        }
    }

    /// Check if a retransmission is needed.
    pub fn needs_retransmit(&self) -> bool {
        if self.state != InviteClientState::Calling {
            return false;
        }
        self.last_retransmit.elapsed() >= self.timer_a_interval
    }

    /// Advance retransmission timer (double Timer A).
    pub fn advance_retransmit_timer(&mut self) {
        self.timer_a_interval *= 2;
        self.last_retransmit = Instant::now();
    }

    /// Check if Timer B (transaction timeout) has expired.
    pub fn is_timed_out(&self) -> bool {
        self.state == InviteClientState::Calling
            && self.started_at.elapsed() >= timers::TIMER_B
    }

    /// Mark as terminated.
    pub fn terminate(&mut self) {
        self.state = InviteClientState::Terminated;
    }

    /// Start the INVITE client transaction timers.
    ///
    /// Spawns a tokio task that:
    /// - Timer A: Retransmits the INVITE, starting at T1 (500ms) and
    ///   doubling up to T2 (4s).
    /// - Timer B: Terminates the transaction after 64*T1 (32s).
    ///
    /// The `retransmit_tx` channel receives (request, remote_addr) when
    /// a retransmission is needed. The `timeout_tx` channel fires when
    /// Timer B expires.
    pub fn start(
        request: SipMessage,
        remote_addr: SocketAddr,
        retransmit_tx: mpsc::Sender<(SipMessage, SocketAddr)>,
        timeout_tx: mpsc::Sender<String>,
        branch: String,
    ) -> tokio::task::JoinHandle<()> {
        let branch_clone = branch.clone();
        tokio::spawn(async move {
            let mut timer_a = timers::TIMER_A_INITIAL;
            let deadline = Instant::now() + timers::TIMER_B;

            loop {
                let sleep_duration = timer_a.min(deadline.saturating_duration_since(Instant::now()));

                tokio::time::sleep(sleep_duration).await;

                if Instant::now() >= deadline {
                    // Timer B expired
                    debug!(branch = %branch_clone, "Timer B expired (INVITE timeout)");
                    let _ = timeout_tx.send(branch_clone).await;
                    break;
                }

                // Timer A: retransmit
                debug!(branch = %branch_clone, interval = ?timer_a, "Timer A: retransmitting INVITE");
                let _ = retransmit_tx.send((request.clone(), remote_addr)).await;

                // Double Timer A up to T2
                timer_a = (timer_a * 2).min(timers::T2);
            }
        })
    }
}

/// An INVITE server transaction.
#[derive(Debug)]
pub struct ServerTransaction {
    pub branch: String,
    pub state: InviteServerState,
    pub request: SipMessage,
    pub remote_addr: SocketAddr,
    pub last_response: Option<SipMessage>,
    started_at: Instant,
}

impl ServerTransaction {
    pub fn new(request: SipMessage, remote_addr: SocketAddr, branch: String) -> Self {
        Self {
            branch,
            state: InviteServerState::Proceeding,
            request,
            remote_addr,
            last_response: None,
            started_at: Instant::now(),
        }
    }

    /// Send a provisional response (1xx).
    pub fn send_provisional(&mut self, response: SipMessage) {
        self.last_response = Some(response);
        // State stays in Proceeding.
    }

    /// Send a final response.
    pub fn send_final(&mut self, response: SipMessage) {
        let status = response.status_code().unwrap_or(0);
        self.last_response = Some(response);

        if (200..300).contains(&status) {
            // For 2xx, the TU handles retransmission, not the transaction layer.
            self.state = InviteServerState::Terminated;
            debug!(branch = %self.branch, status, "INVITE server -> Terminated (2xx)");
        } else if (300..700).contains(&status) {
            self.state = InviteServerState::Completed;
            debug!(branch = %self.branch, status, "INVITE server -> Completed");
        }
    }

    /// Process received ACK.
    pub fn on_ack(&mut self) {
        if self.state == InviteServerState::Completed {
            self.state = InviteServerState::Confirmed;
            debug!(branch = %self.branch, "INVITE server -> Confirmed");
        }
    }

    /// Check if Timer H has expired (waiting for ACK).
    pub fn is_timed_out(&self) -> bool {
        self.state == InviteServerState::Completed
            && self.started_at.elapsed() >= timers::TIMER_H
    }

    pub fn terminate(&mut self) {
        self.state = InviteServerState::Terminated;
    }

    /// Start the INVITE server transaction timers.
    ///
    /// Spawns a tokio task that:
    /// - Timer G: Retransmits the response, starting at T1 and doubling
    ///   up to T2 (only for non-2xx responses over unreliable transport).
    /// - Timer H: Terminates after 64*T1 (32s) if no ACK received.
    ///
    /// The `retransmit_tx` channel receives the response when
    /// retransmission is needed.
    pub fn start(
        response: SipMessage,
        remote_addr: SocketAddr,
        retransmit_tx: mpsc::Sender<(SipMessage, SocketAddr)>,
        timeout_tx: mpsc::Sender<String>,
        branch: String,
    ) -> tokio::task::JoinHandle<()> {
        let branch_clone = branch.clone();
        tokio::spawn(async move {
            let mut timer_g = timers::T1;
            let deadline = Instant::now() + timers::TIMER_H;

            loop {
                let sleep_duration = timer_g.min(deadline.saturating_duration_since(Instant::now()));

                tokio::time::sleep(sleep_duration).await;

                if Instant::now() >= deadline {
                    // Timer H expired -- no ACK received
                    debug!(branch = %branch_clone, "Timer H expired (no ACK)");
                    let _ = timeout_tx.send(branch_clone).await;
                    break;
                }

                // Timer G: retransmit response
                debug!(branch = %branch_clone, interval = ?timer_g, "Timer G: retransmitting response");
                let _ = retransmit_tx.send((response.clone(), remote_addr)).await;

                // Double Timer G up to T2
                timer_g = (timer_g * 2).min(timers::T2);
            }
        })
    }
}

/// A non-INVITE client transaction.
#[derive(Debug)]
pub struct NonInviteClientTransaction {
    pub branch: String,
    pub state: NonInviteClientState,
    pub request: SipMessage,
    pub remote_addr: SocketAddr,
    timer_e_interval: Duration,
    started_at: Instant,
    last_retransmit: Instant,
    pub last_response: Option<SipMessage>,
}

impl NonInviteClientTransaction {
    pub fn new(request: SipMessage, remote_addr: SocketAddr, branch: String) -> Self {
        let now = Instant::now();
        Self {
            branch,
            state: NonInviteClientState::Trying,
            request,
            remote_addr,
            timer_e_interval: timers::TIMER_E_INITIAL,
            started_at: now,
            last_retransmit: now,
            last_response: None,
        }
    }

    pub fn on_response(&mut self, response: SipMessage) {
        let status = response.status_code().unwrap_or(0);
        self.last_response = Some(response);

        match (self.state, status) {
            (NonInviteClientState::Trying, 100..=199) => {
                self.state = NonInviteClientState::Proceeding;
            }
            (
                NonInviteClientState::Trying | NonInviteClientState::Proceeding,
                200..=699,
            ) => {
                self.state = NonInviteClientState::Completed;
            }
            _ => {}
        }
    }

    pub fn needs_retransmit(&self) -> bool {
        matches!(
            self.state,
            NonInviteClientState::Trying | NonInviteClientState::Proceeding
        ) && self.last_retransmit.elapsed() >= self.timer_e_interval
    }

    pub fn advance_retransmit_timer(&mut self) {
        // Timer E doubles up to T2
        self.timer_e_interval = std::cmp::min(self.timer_e_interval * 2, timers::T2);
        self.last_retransmit = Instant::now();
    }

    pub fn is_timed_out(&self) -> bool {
        matches!(
            self.state,
            NonInviteClientState::Trying | NonInviteClientState::Proceeding
        ) && self.started_at.elapsed() >= timers::TIMER_F
    }

    pub fn terminate(&mut self) {
        self.state = NonInviteClientState::Terminated;
    }
}

/// A non-INVITE server transaction.
#[derive(Debug)]
pub struct NonInviteServerTransaction {
    pub branch: String,
    pub state: NonInviteServerState,
    pub request: SipMessage,
    pub remote_addr: SocketAddr,
    pub last_response: Option<SipMessage>,
    #[allow(dead_code)]
    started_at: Instant,
}

impl NonInviteServerTransaction {
    pub fn new(request: SipMessage, remote_addr: SocketAddr, branch: String) -> Self {
        Self {
            branch,
            state: NonInviteServerState::Trying,
            request,
            remote_addr,
            last_response: None,
            started_at: Instant::now(),
        }
    }

    pub fn send_provisional(&mut self, response: SipMessage) {
        self.last_response = Some(response);
        self.state = NonInviteServerState::Proceeding;
    }

    pub fn send_final(&mut self, response: SipMessage) {
        self.last_response = Some(response);
        self.state = NonInviteServerState::Completed;
    }

    pub fn terminate(&mut self) {
        self.state = NonInviteServerState::Terminated;
    }
}
