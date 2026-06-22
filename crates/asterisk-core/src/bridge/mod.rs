//! Bridge framework -- connects channels for media exchange.
//!
//! Port of the Asterisk C bridge subsystem (bridge.c, bridge_channel.c).
//! Provides the global bridge container, bridge lifecycle management
//! (create/join/leave/dissolve), and the trait for bridge technologies.

pub mod implementations;
pub mod builtin_features;
pub mod native_rtp;
pub mod bridge_channel;
pub mod event_loop;
pub mod softmix;
pub mod basic;

use asterisk_types::{AsteriskResult, AsteriskError, BridgeCapability, BridgeFlags, Frame};
use crate::channel::{Channel, ChannelId};
use dashmap::DashMap;
use std::sync::LazyLock;
use std::fmt;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Global Bridge Container
// ---------------------------------------------------------------------------

/// Global bridge container: all active bridges, keyed by unique_id.
///
/// Port of `static struct ao2_container *bridges;` from bridge.c.
static BRIDGE_STORE: LazyLock<DashMap<String, Arc<Mutex<Bridge>>>> =
    LazyLock::new(DashMap::new);

/// Find a bridge by its unique ID.
pub fn find_bridge(id: &str) -> Option<Arc<Mutex<Bridge>>> {
    BRIDGE_STORE.get(id).map(|entry: dashmap::mapref::one::Ref<'_, String, Arc<Mutex<Bridge>>>| entry.value().clone())
}

/// List snapshots of all active bridges.
pub fn list_bridges() -> Vec<BridgeSnapshot> {
    BRIDGE_STORE
        .iter()
        .filter_map(|entry: dashmap::mapref::multiple::RefMulti<'_, String, Arc<Mutex<Bridge>>>| {
            // Try to get a snapshot without blocking; skip if locked.
            let bridge = entry.value().try_lock().ok()?;
            Some(bridge.snapshot())
        })
        .collect()
}

/// Return the number of active bridges.
pub fn bridge_count() -> usize {
    BRIDGE_STORE.len()
}

/// Deregister a bridge from the global container.
fn deregister_bridge(id: &str) -> bool {
    BRIDGE_STORE.remove(id).is_some()
}

// ---------------------------------------------------------------------------
// Video Mode
// ---------------------------------------------------------------------------

/// Video source mode for a bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoMode {
    /// No video
    #[default]
    None,
    /// Single source
    SingleSource,
    /// Talker-based source selection
    TalkerSource,
    /// Selective Forwarding Unit
    Sfu,
}

impl fmt::Display for VideoMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::SingleSource => write!(f, "single_source"),
            Self::TalkerSource => write!(f, "talker_source"),
            Self::Sfu => write!(f, "sfu"),
        }
    }
}

// ---------------------------------------------------------------------------
// Bridge Channel State & Struct
// ---------------------------------------------------------------------------

/// State of a channel within a bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BridgeChannelState {
    #[default]
    Waiting,
    Joined,
    Suspended,
    Leaving,
}

/// A channel's participation in a bridge.
#[derive(Debug, Clone)]
pub struct BridgeChannel {
    pub channel_id: ChannelId,
    pub channel_name: String,
    pub state: BridgeChannelState,
    pub features: u32,
}

impl BridgeChannel {
    pub fn new(channel_id: ChannelId, channel_name: String) -> Self {
        BridgeChannel {
            channel_id,
            channel_name,
            state: BridgeChannelState::Waiting,
            features: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Bridge
// ---------------------------------------------------------------------------

/// A bridge connects two or more channels for media exchange.
#[derive(Debug)]
pub struct Bridge {
    pub unique_id: String,
    pub name: String,
    pub technology: String,
    pub channels: Vec<BridgeChannel>,
    pub flags: BridgeFlags,
    pub video_mode: VideoMode,
    /// Whether the bridge has been dissolved.
    pub dissolved: bool,
    /// Cause code for dissolution.
    pub cause: i32,
    /// Number of active channels.
    pub num_active: usize,
}

impl Bridge {
    pub fn new(name: impl Into<String>) -> Self {
        Bridge {
            unique_id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            technology: String::new(),
            channels: Vec::new(),
            flags: BridgeFlags::default(),
            video_mode: VideoMode::default(),
            dissolved: false,
            cause: 0,
            num_active: 0,
        }
    }

    pub fn with_flags(name: impl Into<String>, flags: BridgeFlags) -> Self {
        let mut bridge = Self::new(name);
        bridge.flags = flags;
        bridge
    }

    pub fn add_channel(&mut self, channel_id: ChannelId, channel_name: String) {
        if self.channels.iter().any(|bc| bc.channel_id == channel_id) {
            return;
        }
        let mut bc = BridgeChannel::new(channel_id, channel_name);
        bc.state = BridgeChannelState::Joined;
        self.channels.push(bc);
        self.num_active = self.channels.len();
    }

    pub fn remove_channel(&mut self, channel_id: &ChannelId) -> bool {
        let len_before = self.channels.len();
        self.channels.retain(|bc| bc.channel_id != *channel_id);
        let removed = self.channels.len() < len_before;
        if removed {
            self.num_active = self.channels.len();
        }
        removed
    }

    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    pub fn has_channel(&self, channel_id: &ChannelId) -> bool {
        self.channels.iter().any(|bc| bc.channel_id == *channel_id)
    }

    pub fn snapshot(&self) -> BridgeSnapshot {
        BridgeSnapshot {
            unique_id: self.unique_id.clone(),
            name: self.name.clone(),
            technology: self.technology.clone(),
            num_channels: self.channels.len(),
            channel_ids: self.channels.iter().map(|bc| bc.channel_id.clone()).collect(),
            video_mode: self.video_mode,
        }
    }
}

/// Immutable snapshot of a bridge.
#[derive(Debug, Clone)]
pub struct BridgeSnapshot {
    pub unique_id: String,
    pub name: String,
    pub technology: String,
    pub num_channels: usize,
    pub channel_ids: Vec<ChannelId>,
    pub video_mode: VideoMode,
}

// ---------------------------------------------------------------------------
// Bridge Technology Trait
// ---------------------------------------------------------------------------

/// Bridge technology trait.
#[async_trait::async_trait]
pub trait BridgeTechnology: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;
    fn capabilities(&self) -> BridgeCapability;
    fn preference(&self) -> u32;
    async fn create(&self, bridge: &mut Bridge) -> AsteriskResult<()>;
    async fn start(&self, bridge: &mut Bridge) -> AsteriskResult<()>;
    async fn stop(&self, bridge: &mut Bridge) -> AsteriskResult<()>;
    async fn join(&self, bridge: &mut Bridge, channel: &BridgeChannel) -> AsteriskResult<()>;
    async fn leave(&self, bridge: &mut Bridge, channel: &BridgeChannel) -> AsteriskResult<()>;
    async fn write_frame(
        &self,
        bridge: &mut Bridge,
        from_channel: &BridgeChannel,
        frame: &Frame,
    ) -> AsteriskResult<()>;
}

// ---------------------------------------------------------------------------
// Bridge Lifecycle Functions
// ---------------------------------------------------------------------------

/// Create a new bridge with the given technology.
///
/// Port of bridge_alloc + bridge_base_init + bridge_register from C.
/// Allocates a bridge, calls technology's create(), and registers it
/// in the global container.
pub async fn bridge_create(
    name: impl Into<String>,
    technology: &Arc<dyn BridgeTechnology>,
) -> AsteriskResult<Arc<Mutex<Bridge>>> {
    let name = name.into();
    let mut bridge = Bridge::new(&name);

    // Call technology's create method.
    technology.create(&mut bridge).await?;
    bridge.technology = technology.name().to_string();

    let bridge_id = bridge.unique_id.clone();
    let bridge_arc = Arc::new(Mutex::new(bridge));

    // Register in global container.
    if BRIDGE_STORE.contains_key(&bridge_id) {
        return Err(AsteriskError::AlreadyExists(format!(
            "Bridge {} already exists",
            bridge_id
        )));
    }
    BRIDGE_STORE.insert(bridge_id.clone(), bridge_arc.clone());

    info!(bridge_id = %bridge_id, name = %name, tech = technology.name(), "Bridge created and registered");

    // Emit BridgeCreate AMI event
    crate::channel::publish_channel_event("BridgeCreate", &[
        ("BridgeUniqueid", &bridge_id),
        ("BridgeType", "basic"),
        ("BridgeTechnology", "simple_bridge"),
        ("BridgeNumChannels", "0"),
    ]);

    // Call technology's start method.
    {
        let mut br = bridge_arc.lock().await;
        technology.start(&mut br).await?;
    }

    Ok(bridge_arc)
}

/// Join a channel to a bridge.
///
/// Port of ast_bridge_join from C:
/// 1. Creates a BridgeChannel for this channel
/// 2. Sets channel state to bridged
/// 3. Calls technology's join() method
/// 4. Returns the BridgeChannel (caller can then spawn bridge_channel_run)
pub async fn bridge_join(
    bridge: &Arc<Mutex<Bridge>>,
    channel: &Arc<Mutex<Channel>>,
    technology: &Arc<dyn BridgeTechnology>,
) -> AsteriskResult<Arc<Mutex<BridgeChannel>>> {
    let (channel_id, channel_name, bridge_id) = {
        let chan = channel.lock().await;
        let br = bridge.lock().await;

        // Check if channel is already in a bridge.
        if chan.bridge_id.is_some() {
            return Err(AsteriskError::InvalidArgument(format!(
                "Channel {} is already in a bridge",
                chan.name
            )));
        }

        // Check if bridge is dissolved.
        if br.dissolved {
            return Err(AsteriskError::InvalidArgument(format!(
                "Bridge {} is dissolved",
                br.unique_id
            )));
        }

        (chan.unique_id.clone(), chan.name.clone(), br.unique_id.clone())
    };

    // Create the BridgeChannel.
    let mut bc = BridgeChannel::new(channel_id.clone(), channel_name.clone());
    bc.state = BridgeChannelState::Joined;

    // Add channel to bridge and call technology join.
    {
        let mut br = bridge.lock().await;
        br.add_channel(channel_id.clone(), channel_name.clone());
        technology.join(&mut br, &bc).await?;
    }

    // Set channel's bridge_id.
    {
        let mut chan = channel.lock().await;
        chan.bridge_id = Some(bridge_id.clone());
    }

    let bc_arc = Arc::new(Mutex::new(bc));

    // Get the current channel count for the event
    let num_channels = {
        let br = bridge.lock().await;
        br.channels.len().to_string()
    };

    info!(
        channel = %channel_name,
        bridge_id = %bridge_id,
        "Channel joined bridge"
    );

    // Emit BridgeEnter AMI event
    crate::channel::publish_channel_event("BridgeEnter", &[
        ("BridgeUniqueid", &bridge_id),
        ("BridgeType", "basic"),
        ("BridgeTechnology", "simple_bridge"),
        ("BridgeNumChannels", &num_channels),
        ("Channel", &channel_name),
        ("Uniqueid", &channel_id.0),
    ]);

    Ok(bc_arc)
}

/// Remove a channel from a bridge.
///
/// Port of ast_bridge_depart / bridge_channel_internal_pull from C:
/// 1. Removes channel from bridge's channel list
/// 2. Calls technology's leave() method
/// 3. If bridge is now empty or has < 2 channels for simple bridge, dissolve
pub async fn bridge_leave(
    bridge: &Arc<Mutex<Bridge>>,
    bridge_chan: &Arc<Mutex<BridgeChannel>>,
    channel: &Arc<Mutex<Channel>>,
    technology: &Arc<dyn BridgeTechnology>,
) -> AsteriskResult<()> {
    let (channel_id, channel_name) = {
        let bc = bridge_chan.lock().await;
        (bc.channel_id.clone(), bc.channel_name.clone())
    };

    // Remove from bridge and call technology leave.
    {
        let bc = bridge_chan.lock().await;
        let mut br = bridge.lock().await;
        technology.leave(&mut br, &bc).await?;
        br.remove_channel(&channel_id);
    }

    // Clear channel's bridge_id.
    {
        let mut chan = channel.lock().await;
        chan.bridge_id = None;
    }

    // Mark bridge channel as leaving.
    {
        let mut bc = bridge_chan.lock().await;
        bc.state = BridgeChannelState::Leaving;
    }

    // Get the bridge_id for the event
    let bridge_id = {
        let br = bridge.lock().await;
        br.unique_id.clone()
    };

    info!(
        channel = %channel_name,
        "Channel left bridge"
    );

    // Emit BridgeLeave AMI event
    crate::channel::publish_channel_event("BridgeLeave", &[
        ("BridgeUniqueid", &bridge_id),
        ("BridgeType", "basic"),
        ("BridgeTechnology", "simple_bridge"),
        ("Channel", &channel_name),
        ("Uniqueid", &channel_id.0),
    ]);

    // Check if bridge should dissolve.
    let should_dissolve = {
        let br = bridge.lock().await;
        if br.dissolved {
            false // Already dissolving.
        } else if br.flags.contains(BridgeFlags::DISSOLVE_EMPTY) && br.num_channels() == 0 {
            true
        } else { br.flags.contains(BridgeFlags::DISSOLVE_HANGUP) && br.num_channels() < 2 }
    };

    if should_dissolve {
        bridge_dissolve(bridge, technology).await?;
    }

    Ok(())
}

/// Dissolve a bridge: request all channels to leave, stop, and deregister.
///
/// Port of bridge_dissolve from C:
/// 1. Mark bridge as dissolved
/// 2. Request all channels to leave (set their state to Leaving)
/// 3. Call technology's stop() method
/// 4. Deregister from global container
pub async fn bridge_dissolve(
    bridge: &Arc<Mutex<Bridge>>,
    technology: &Arc<dyn BridgeTechnology>,
) -> AsteriskResult<()> {
    let bridge_id;

    {
        let mut br = bridge.lock().await;
        if br.dissolved {
            debug!(bridge_id = %br.unique_id, "Bridge already dissolved");
            return Ok(());
        }
        br.dissolved = true;
        bridge_id = br.unique_id.clone();

        info!(
            bridge_id = %bridge_id,
            name = %br.name,
            num_channels = br.num_channels(),
            "Dissolving bridge"
        );

        // Request all channels to leave.
        for bc in br.channels.iter_mut() {
            debug!(channel = %bc.channel_name, "Kicking channel from dissolving bridge");
            bc.state = BridgeChannelState::Leaving;
        }
    }

    // Call technology stop.
    {
        let mut br = bridge.lock().await;
        if let Err(e) = technology.stop(&mut br).await {
            warn!(
                bridge_id = %bridge_id,
                error = %e,
                "Error stopping bridge technology"
            );
        }
    }

    // Deregister from global container.
    deregister_bridge(&bridge_id);

    // Emit BridgeDestroy AMI event
    crate::channel::publish_channel_event("BridgeDestroy", &[
        ("BridgeUniqueid", &bridge_id),
        ("BridgeType", "basic"),
        ("BridgeTechnology", "simple_bridge"),
    ]);

    info!(bridge_id = %bridge_id, "Bridge dissolved and deregistered");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::implementations::SimpleBridge;
    use crate::channel::Channel;

    #[test]
    fn test_bridge_create_snapshot() {
        let mut bridge = Bridge::new("test-bridge");
        bridge.add_channel(
            ChannelId::from_name("chan1"),
            "SIP/alice-001".to_string(),
        );
        let snap = bridge.snapshot();
        assert_eq!(snap.name, "test-bridge");
        assert_eq!(snap.num_channels, 1);
    }

    #[test]
    fn test_bridge_dissolved_field() {
        let mut bridge = Bridge::new("test");
        assert!(!bridge.dissolved);
        bridge.dissolved = true;
        assert!(bridge.dissolved);
    }

    #[test]
    fn test_bridge_container_operations() {
        // Note: other tests may be running concurrently with the global
        // BRIDGE_STORE, so we only assert on our specific bridge's
        // presence/absence rather than exact counts.
        let initial = bridge_count();

        let bridge = Bridge::new("container-test");
        let id = bridge.unique_id.clone();
        let arc = Arc::new(Mutex::new(bridge));

        BRIDGE_STORE.insert(id.clone(), arc.clone());
        assert!(
            bridge_count() > initial,
            "count should increase after insert"
        );
        assert!(find_bridge(&id).is_some());

        let bridges = list_bridges();
        assert!(bridges.iter().any(|b| b.unique_id == id));

        deregister_bridge(&id);
        assert!(find_bridge(&id).is_none());
    }

    #[tokio::test]
    async fn test_bridge_lifecycle_create_dissolve() {
        let tech: Arc<dyn BridgeTechnology> = Arc::new(SimpleBridge::new());

        let bridge = bridge_create("lifecycle-test", &tech).await.unwrap();
        let bridge_id = {
            let br = bridge.lock().await;
            br.unique_id.clone()
        };

        assert!(find_bridge(&bridge_id).is_some());

        bridge_dissolve(&bridge, &tech).await.unwrap();

        assert!(find_bridge(&bridge_id).is_none());

        let br = bridge.lock().await;
        assert!(br.dissolved);
    }

    #[tokio::test]
    async fn test_bridge_join_leave() {
        let tech: Arc<dyn BridgeTechnology> = Arc::new(SimpleBridge::new());
        let bridge = bridge_create("join-test", &tech).await.unwrap();

        let channel = Arc::new(Mutex::new(Channel::new("SIP/alice-001")));

        let bc = bridge_join(&bridge, &channel, &tech).await.unwrap();

        {
            let br = bridge.lock().await;
            assert_eq!(br.num_channels(), 1);
        }
        {
            let chan = channel.lock().await;
            assert!(chan.bridge_id.is_some());
        }

        bridge_leave(&bridge, &bc, &channel, &tech).await.unwrap();

        {
            let chan = channel.lock().await;
            assert!(chan.bridge_id.is_none());
        }
        {
            let bc_inner = bc.lock().await;
            assert_eq!(bc_inner.state, BridgeChannelState::Leaving);
        }

        // Clean up.
        bridge_dissolve(&bridge, &tech).await.ok();
    }

    #[tokio::test]
    async fn test_bridge_dissolve_kicks_channels() {
        let tech: Arc<dyn BridgeTechnology> = Arc::new(SimpleBridge::new());
        let bridge = bridge_create("kick-test", &tech).await.unwrap();

        // Manually add channels to the bridge (without the full join lifecycle).
        {
            let mut br = bridge.lock().await;
            br.add_channel(ChannelId::from_name("chan1"), "SIP/alice-001".to_string());
            br.add_channel(ChannelId::from_name("chan2"), "SIP/bob-001".to_string());
        }

        bridge_dissolve(&bridge, &tech).await.unwrap();

        let br = bridge.lock().await;
        assert!(br.dissolved);
        // All channels should be marked as leaving.
        for bc in &br.channels {
            assert_eq!(bc.state, BridgeChannelState::Leaving);
        }
    }

    #[tokio::test]
    async fn test_bridge_join_already_in_bridge() {
        let tech: Arc<dyn BridgeTechnology> = Arc::new(SimpleBridge::new());
        let bridge = bridge_create("double-join", &tech).await.unwrap();

        let channel = Arc::new(Mutex::new(Channel::new("SIP/alice-001")));

        // First join should succeed.
        let _bc = bridge_join(&bridge, &channel, &tech).await.unwrap();

        // Second join should fail -- channel is already in a bridge.
        let result = bridge_join(&bridge, &channel, &tech).await;
        assert!(result.is_err());

        // Clean up.
        bridge_dissolve(&bridge, &tech).await.ok();
    }

    #[tokio::test]
    async fn test_bridge_join_dissolved() {
        let tech: Arc<dyn BridgeTechnology> = Arc::new(SimpleBridge::new());
        let bridge = bridge_create("dissolved-join", &tech).await.unwrap();

        // Dissolve the bridge first.
        bridge_dissolve(&bridge, &tech).await.unwrap();

        let channel = Arc::new(Mutex::new(Channel::new("SIP/alice-001")));
        let result = bridge_join(&bridge, &channel, &tech).await;
        assert!(result.is_err());
    }
}
