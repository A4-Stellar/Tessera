//! Bounded event resequencing for RPC streams which may briefly disagree.
//!
//! Events are held for three ledgers before delivery.  This small delay lets a
//! replacement ledger arrive before it is exposed to consumers; if it arrives
//! after delivery, [`Resequencer::rewind`] returns the affected events again.

use std::collections::BTreeMap;

pub const REORG_DEPTH: u32 = 3;

/// The stable ordering key used by Soroban event consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventPosition {
    pub ledger_seq: u32,
    pub tx_index: u32,
    pub event_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequencedEvent<T> {
    pub position: EventPosition,
    pub value: T,
}

/// A priority queue (implemented as a `BTreeMap`) plus a short delivered
/// history. `BTreeMap` makes the ordering rule explicit and deterministic.
#[derive(Debug)]
pub struct Resequencer<T> {
    pending: BTreeMap<EventPosition, T>,
    delivered: BTreeMap<EventPosition, T>,
    highest_ledger: u32,
}

impl<T: Clone> Default for Resequencer<T> {
    fn default() -> Self { Self::new() }
}

impl<T: Clone> Resequencer<T> {
    pub fn new() -> Self {
        Self { pending: BTreeMap::new(), delivered: BTreeMap::new(), highest_ledger: 0 }
    }

    /// Adds an event and emits only ledgers outside the reorganisation window.
    /// A duplicate position replaces the old RPC observation.
    pub fn push(&mut self, event: SequencedEvent<T>) -> Vec<SequencedEvent<T>> {
        self.highest_ledger = self.highest_ledger.max(event.position.ledger_seq);
        self.pending.insert(event.position, event.value);
        self.flush_stable()
    }

    /// Replace a forked suffix. The returned sequence contains both previously
    /// delivered events in that suffix and newly supplied events, sorted, so a
    /// downstream projection can rewind and replay it atomically.
    pub fn rewind<I>(&mut self, from_ledger: u32, replacement: I) -> Vec<SequencedEvent<T>>
    where I: IntoIterator<Item = SequencedEvent<T>> {
        let lower = EventPosition { ledger_seq: from_ledger, tx_index: 0, event_index: 0 };
        let old: Vec<_> = self.delivered.range(lower..).map(|(&position, value)| SequencedEvent { position, value: value.clone() }).collect();
        self.delivered.retain(|position, _| position.ledger_seq < from_ledger);
        self.pending.retain(|position, _| position.ledger_seq < from_ledger);
        for event in replacement {
            self.highest_ledger = self.highest_ledger.max(event.position.ledger_seq);
            self.pending.insert(event.position, event.value);
        }
        // Include immediate replay output. Consumers receive a strictly sorted
        // suffix, including old entries they must undo before applying it.
        let mut replay = old;
        replay.extend(self.flush_stable());
        replay.sort_by_key(|event| event.position);
        replay
    }

    /// Flush every pending event when the source is known to have ended.
    pub fn finish(&mut self) -> Vec<SequencedEvent<T>> {
        self.highest_ledger = u32::MAX;
        self.flush_stable()
    }

    fn flush_stable(&mut self) -> Vec<SequencedEvent<T>> {
        let safe_through = self.highest_ledger.saturating_sub(REORG_DEPTH);
        let keys: Vec<_> = self.pending.range(..=EventPosition { ledger_seq: safe_through, tx_index: u32::MAX, event_index: u32::MAX }).map(|(&key, _)| key).collect();
        keys.into_iter().filter_map(|position| self.pending.remove(&position).map(|value| {
            self.delivered.insert(position, value.clone());
            SequencedEvent { position, value }
        })).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(ledger: u32, tx: u32, index: u32, value: &str) -> SequencedEvent<String> {
        SequencedEvent { position: EventPosition { ledger_seq: ledger, tx_index: tx, event_index: index }, value: value.into() }
    }
    #[test]
    fn emits_in_strict_position_order_after_window() {
        let mut buffer = Resequencer::new();
        buffer.push(event(12, 1, 0, "late"));
        buffer.push(event(10, 3, 0, "c"));
        buffer.push(event(10, 1, 1, "b"));
        buffer.push(event(10, 1, 0, "a"));
        let out = buffer.finish();
        assert_eq!(out.into_iter().map(|e| e.value).collect::<Vec<_>>(), ["a", "b", "c", "late"]);
    }
    #[test]
    fn replays_a_micro_fork_suffix() {
        let mut buffer = Resequencer::new();
        for ledger in 1..=5 { buffer.push(event(ledger, 0, 0, &format!("old-{ledger}"))); }
        let replay = buffer.rewind(2, [event(2, 0, 0, "new-2"), event(3, 0, 0, "new-3")]);
        assert!(replay.windows(2).all(|pair| pair[0].position <= pair[1].position));
        assert!(replay.iter().any(|event| event.value == "old-2"));
        assert_eq!(buffer.finish().last().map(|event| event.value.as_str()), Some("new-3"));
    }
}
