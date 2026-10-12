//! Live stream: one `snapshot` on connect, then one `event` per change.
//! A subscriber too slow to keep up gets a fresh `snapshot` instead of
//! blocking anyone. A guest follows its rooms only: their devices and the
//! orders held for them, never the journal nor the house's notices.

use std::collections::HashSet;
use std::convert::Infallible;

use axum::Extension;
use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures::Stream;
use futures::stream::{self, StreamExt};
use moli_core::{DeviceId, Event};
use moli_runtime::Hub;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use crate::caller::Caller;
use crate::people::PersonRef;

pub(crate) async fn events(
    State(hub): State<Hub>,
    Extension(shutdown): Extension<CancellationToken>,
    caller: Caller,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let mut view = View {
        guest: caller.person.filter(PersonRef::is_guest),
        sight: HashSet::new(),
    };
    // Subscribe before the snapshot: nothing can fall between the two.
    let rx = hub.subscribe();
    let first = view.snapshot(&hub);
    let rest = stream::unfold((hub, rx, view), |(hub, mut rx, mut view)| async move {
        let event = loop {
            match rx.recv().await {
                Ok(event) => match view.pass(&hub, &event) {
                    Pass::Event => {
                        break SseEvent::default().event("event").json_data(&event).ok()?;
                    }
                    Pass::Snapshot => break view.snapshot(&hub),
                    Pass::Drop => {}
                },
                Err(RecvError::Lagged(missed)) => {
                    tracing::debug!(missed, "sse subscriber lagged, resending snapshot");
                    break view.snapshot(&hub);
                }
                Err(RecvError::Closed) => return None,
            }
        };
        Some((Ok(event), (hub, rx, view)))
    });
    let stream = stream::once(async { Ok(first) })
        .chain(rest)
        .take_until(shutdown.cancelled_owned());
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// What one subscriber may see: the whole house, or a guest's rooms.
struct View {
    guest: Option<PersonRef>,
    /// The devices the guest saw in its last snapshot.
    sight: HashSet<DeviceId>,
}

enum Pass {
    Event,
    /// What the guest sees changed: a fresh snapshot instead.
    Snapshot,
    Drop,
}

impl View {
    fn snapshot(&mut self, hub: &Hub) -> SseEvent {
        let snapshot = match &self.guest {
            Some(guest) => {
                let (snapshot, sight) = guest.guest_view(hub);
                self.sight = sight;
                snapshot
            }
            None => hub.snapshot(),
        };
        SseEvent::default()
            .event("snapshot")
            .json_data(snapshot)
            .unwrap_or_else(|_| SseEvent::default().event("error"))
    }

    /// Whether `event` goes to this subscriber.
    fn pass(&self, hub: &Hub, event: &Event) -> Pass {
        let Some(guest) = &self.guest else {
            return Pass::Event;
        };
        let (device, moved) = match event {
            Event::State { point, .. } => (point.split().map(|(id, _)| id), false),
            Event::Availability { device, .. } => (Some(device.clone()), false),
            Event::ApprovalRequested { request } => {
                (request.point.split().map(|(id, _)| id), false)
            }
            Event::DeviceUpserted { device } => (Some(device.id.clone()), true),
            Event::DeviceRemoved { device } | Event::LabelChanged { device, .. } => {
                (Some(device.clone()), true)
            }
            // A driver's state, a decision on a request: no room in them.
            Event::DriverStatus { .. } | Event::ApprovalResolved { .. } => return Pass::Event,
            // The house's journal and notices are the household's.
            Event::Journal { .. } | Event::Notice { .. } => return Pass::Drop,
        };
        // Described anew, moved, gone: what the guest sees may have changed.
        if moved && guest.guest_devices(hub, &hub.snapshot()) != self.sight {
            return Pass::Snapshot;
        }
        match device {
            Some(id) if self.sight.contains(&id) => Pass::Event,
            _ => Pass::Drop,
        }
    }
}
