//! Bounded, deterministic ordering for events returned out of order by RPCs.
//!
//! An event is held until it is more than `REORG_DEPTH` ledgers behind the
//! newest observed ledger.  This deliberately trades a tiny amount of latency
//! for a stable stream during the short reorgs common with RPC failover.

use std::collections::BTreeMap;

/// The largest reorganisation this buffer automatically corrects.
pub const REORG_DEPTH: u32 = 3;

/// A total ordering key for a Soroban event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventPosition {
    pub ledger_seq: u32,
    pub tx_index: u32,
    pub event_index: u32,
}

impl EventPosition {
    pub const fn new(ledger_seq: u32, tx_index: u32, event_index: u32) -> Self {
        Self { ledger_seq, tx_index, event_index }
    }
}

/// An event plus the ledger hash that makes a fork detectable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedEvent<T> {
    pub position: EventPosition,
    pub ledger_hash: String,
    pub payload: T,
}

/// Output from [`Resequencer::push`]. `replayed` is true for events emitted
/// again after a corrected micro-fork, allowing idempotent consumers to
/// replace their previous projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmittedEvent<T> {
    pub event: IndexedEvent<T>,
    pub replayed: bool,
}

/// Sliding-window event resequencer. It is intentionally transport agnostic:
/// feed it RPC events and forward the returned values downstream.
#[derive(Debug)]
pub struct Resequencer<T> {
    pending: BTreeMap<EventPosition, IndexedEvent<T>>,
    emitted: BTreeMap<EventPosition, IndexedEvent<T>>,
    highest_ledger: Option<u32>,
}

impl<T: Clone> Default for Resequencer<T> {
    fn default() -> Self { Self::new() }
}

impl<T: Clone> Resequencer<T> {
    pub fn new() -> Self {
        Self { pending: BTreeMap::new(), emitted: BTreeMap::new(), highest_ledger: None }
    }

    /// Add one event and return all events now safe to consume. Returned
    /// events are always strictly ordered by `(ledger_seq, tx_index,
    /// event_index)`.
    pub fn push(&mut self, event: IndexedEvent<T>) -> Vec<EmittedEvent<T>> {
        self.highest_ledger = Some(self.highest_ledger.map_or(event.position.ledger_seq, |tip| tip.max(event.position.ledger_seq)));

        let fork_at = self.emitted.iter().find_map(|(position, old)| {
            (position.ledger_seq == event.position.ledger_seq && old.ledger_hash != event.ledger_hash)
                .then_some(event.position.ledger_seq)
        });
        let mut output = Vec::new();
        if let Some(ledger) = fork_at {
            // The bounded history is exactly the automatically-supported fork
            // window. Requeue invalidated records, then replay in canonical
            // position order once the normal watermark permits them.
            let invalidated: Vec<_> = self.emitted
                .range(EventPosition::new(ledger, 0, 0)..)
                .map(|(position, value)| (*position, value.clone()))
                .collect();
            for (position, old) in invalidated {
                self.emitted.remove(&position);
                self.pending.insert(position, old);
            }
            // A new observation replaces the stale event at this exact slot.
            self.pending.insert(event.position, event);
            output.extend(self.drain(true));
        } else {
            self.pending.insert(event.position, event);
            output.extend(self.drain(false));
        }
        output
    }

    /// Flush buffered events, for shutdown or when an RPC page is known to be
    /// complete. This is the only path that emits the newest three ledgers.
    pub fn flush(&mut self) -> Vec<EmittedEvent<T>> {
        let events = std::mem::take(&mut self.pending);
        events.into_values().map(|event| {
            self.emitted.insert(event.position, event.clone());
            EmittedEvent { event, replayed: false }
        }).collect()
    }

    fn drain(&mut self, replayed: bool) -> Vec<EmittedEvent<T>> {
        let watermark = self.highest_ledger.unwrap_or_default().saturating_sub(REORG_DEPTH);
        let safe: Vec<_> = self.pending
            .iter()
            .filter(|(_, event)| event.position.ledger_seq <= watermark)
            .map(|(position, _)| *position)
            .collect();
        safe.into_iter().filter_map(|position| {
            let event = self.pending.remove(&position)?;
            self.emitted.insert(position, event.clone());
            Some(EmittedEvent { event, replayed })
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(ledger: u32, tx: u32, index: u32, hash: &str) -> IndexedEvent<u32> {
        IndexedEvent { position: EventPosition::new(ledger, tx, index), ledger_hash: hash.into(), payload: index }
    }

    #[test]
    fn emits_out_of_order_events_in_strict_position_order() {
        let mut buffer = Resequencer::new();
        buffer.push(event(10, 1, 0, "a"));
        buffer.push(event(8, 2, 0, "b"));
        buffer.push(event(8, 1, 1, "b"));
        let emitted = buffer.push(event(11, 0, 0, "c"));
        assert_eq!(emitted.iter().map(|item| item.event.position).collect::<Vec<_>>(), vec![EventPosition::new(8, 1, 1), EventPosition::new(8, 2, 0)]);
    }

    #[test]
    fn flushes_the_tail_in_order() {
        let mut buffer = Resequencer::new();
        buffer.push(event(12, 2, 0, "a"));
        buffer.push(event(12, 1, 0, "a"));
        assert_eq!(buffer.flush().iter().map(|item| item.event.position).collect::<Vec<_>>(), vec![EventPosition::new(12, 1, 0), EventPosition::new(12, 2, 0)]);
    }
}
