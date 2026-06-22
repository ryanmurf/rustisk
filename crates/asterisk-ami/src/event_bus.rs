//! Global AMI event bus.
//!
//! A process-wide broadcast channel that any part of the system can publish
//! AMI events to.  The AMI server subscribes to this bus and forwards events
//! to all connected (and authenticated) sessions.
//!
//! The bus is backed by a `tokio::sync::broadcast` channel with a generous
//! buffer (10 000 events).  Publishing when there are no subscribers is a
//! harmless no-op.

use crate::protocol::AmiEvent;
use std::sync::LazyLock;
use tokio::sync::broadcast;

/// Global AMI event bus sender.
///
/// Any crate that depends on `asterisk-ami` can import this static and call
/// `AMI_EVENT_BUS.send(event)` -- or, more conveniently, call
/// [`publish_event`].
pub static AMI_EVENT_BUS: LazyLock<broadcast::Sender<AmiEvent>> =
    LazyLock::new(|| broadcast::channel(10_000).0);

/// Convenience function: publish an event on the global AMI bus.
///
/// Sending when there are no receivers (no AMI sessions connected) silently
/// discards the event -- this is by design.
pub fn publish_event(event: AmiEvent) {
    let _ = AMI_EVENT_BUS.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventCategory;
    use std::time::Duration;

    #[tokio::test]
    async fn test_publish_and_receive() {
        let mut rx = AMI_EVENT_BUS.subscribe();
        let event =
            AmiEvent::new("TestBusEvent", EventCategory::SYSTEM.0).with_header("Key", "Value");
        publish_event(event);

        let received = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let event = rx.recv().await.unwrap();
                if event.name == "TestBusEvent" {
                    return event;
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(received.name, "TestBusEvent");
        assert_eq!(received.headers.get("Key").unwrap(), "Value");
    }

    #[test]
    fn test_publish_no_receivers() {
        // Should not panic when there are no subscribers
        let event = AmiEvent::new("Orphan", EventCategory::SYSTEM.0);
        publish_event(event);
    }
}
