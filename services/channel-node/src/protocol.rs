//! Network protocol for state channel communication

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateUpdate {
    #[serde_as(as = "[_; 32]")]
    pub channel_id: [u8; 32],
    pub sequence: u64,
    pub balance_a: i64,
    pub balance_b: i64,
    #[serde_as(as = "[_; 64]")]
    pub sig_a: [u8; 64],
    #[serde_as(as = "[_; 64]")]
    pub sig_b: [u8; 64],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ChannelMessage {
    /// Propose a new state update
    ProposeState(StateUpdate),
    /// Accept the proposed state
    AcceptState { 
        #[serde(with = "serde_bytes")]
        channel_id: [u8; 32], 
        sequence: u64 
    },
    /// Reject the proposed state
    RejectState {
        #[serde(with = "serde_bytes")]
        channel_id: [u8; 32],
        sequence: u64,
        reason: String,
    },
    /// Request cooperative channel closure
    RequestClose {
        #[serde(with = "serde_bytes")]
        channel_id: [u8; 32],
        final_state: StateUpdate,
    },
    /// Acknowledge channel closure
    AcknowledgeClose { 
        #[serde(with = "serde_bytes")]
        channel_id: [u8; 32] 
    },
}

impl StateUpdate {
    /// Serialize for wire transmission
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize from wire format
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

impl ChannelMessage {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization() {
        let update = StateUpdate {
            channel_id: [1u8; 32],
            sequence: 42,
            balance_a: 900,
            balance_b: 1100,
            sig_a: [2u8; 64],
            sig_b: [3u8; 64],
        };

        let json = update.to_json().unwrap();
        let deserialized = StateUpdate::from_json(&json).unwrap();

        assert_eq!(deserialized.sequence, 42);
        assert_eq!(deserialized.balance_a, 900);
        assert_eq!(deserialized.balance_b, 1100);
    }

    #[test]
    fn test_channel_message_variants() {
        let msg = ChannelMessage::ProposeState(StateUpdate {
            channel_id: [1u8; 32],
            sequence: 1,
            balance_a: 1000,
            balance_b: 1000,
            sig_a: [0u8; 64],
            sig_b: [0u8; 64],
        });

        let json = msg.to_json().unwrap();
        let deserialized = ChannelMessage::from_json(&json).unwrap();

        match deserialized {
            ChannelMessage::ProposeState(update) => {
                assert_eq!(update.sequence, 1);
            }
            _ => panic!("Wrong variant"),
        }
    }
}
