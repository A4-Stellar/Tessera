//! Channel state management and signature verification

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ChannelError {
    #[error("Channel not found")]
    NotFound,
    #[error("Invalid signature from party A")]
    InvalidSignatureA,
    #[error("Invalid signature from party B")]
    InvalidSignatureB,
    #[error("Sequence number must be monotonically increasing")]
    InvalidSequence,
    #[error("Balances must sum to total channel capacity")]
    InvalidBalances,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelState {
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

#[derive(Clone, Debug)]
pub struct Channel {
    pub channel_id: [u8; 32],
    pub party_a: VerifyingKey,
    pub party_b: VerifyingKey,
    pub sequence: u64,
    pub balance_a: i64,
    pub balance_b: i64,
    pub total_capacity: i64,
    pub sig_a: Option<[u8; 64]>,
    pub sig_b: Option<[u8; 64]>,
}

impl Channel {
    /// Construct the message to be signed: channel_id || sequence || balance_a || balance_b
    pub fn message_to_sign(&self) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(&self.channel_id);
        msg.extend_from_slice(&self.sequence.to_be_bytes());
        msg.extend_from_slice(&self.balance_a.to_be_bytes());
        msg.extend_from_slice(&self.balance_b.to_be_bytes());
        msg
    }

    /// Verify both signatures on the current state
    pub fn verify_signatures(&self) -> Result<(), ChannelError> {
        let msg = self.message_to_sign();

        if let Some(sig_a_bytes) = &self.sig_a {
            let sig_a = Signature::from_bytes(sig_a_bytes);
            self.party_a
                .verify(&msg, &sig_a)
                .map_err(|_| ChannelError::InvalidSignatureA)?;
        }

        if let Some(sig_b_bytes) = &self.sig_b {
            let sig_b = Signature::from_bytes(sig_b_bytes);
            self.party_b
                .verify(&msg, &sig_b)
                .map_err(|_| ChannelError::InvalidSignatureB)?;
        }

        Ok(())
    }

    /// Check if balances sum to total capacity
    pub fn validate_balances(&self) -> Result<(), ChannelError> {
        if self.balance_a + self.balance_b != self.total_capacity {
            return Err(ChannelError::InvalidBalances);
        }
        Ok(())
    }
}

pub struct ChannelManager {
    channels: HashMap<[u8; 32], Channel>,
}

impl ChannelManager {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
        }
    }

    /// Create a new channel
    pub fn create_channel(
        &mut self,
        channel_id: [u8; 32],
        party_a: VerifyingKey,
        party_b: VerifyingKey,
        initial_balance_a: i64,
        initial_balance_b: i64,
    ) {
        let channel = Channel {
            channel_id,
            party_a,
            party_b,
            sequence: 0,
            balance_a: initial_balance_a,
            balance_b: initial_balance_b,
            total_capacity: initial_balance_a + initial_balance_b,
            sig_a: None,
            sig_b: None,
        };
        self.channels.insert(channel_id, channel);
    }

    /// Update channel state with new balances and signatures
    pub fn update_state(
        &mut self,
        channel_id: &[u8; 32],
        new_balance_a: i64,
        new_balance_b: i64,
        signing_key_a: &SigningKey,
        signing_key_b: &SigningKey,
    ) -> Result<ChannelState, ChannelError> {
        let channel = self.channels.get_mut(channel_id).ok_or(ChannelError::NotFound)?;

        // Increment sequence
        let new_sequence = channel.sequence + 1;

        // Update state
        channel.sequence = new_sequence;
        channel.balance_a = new_balance_a;
        channel.balance_b = new_balance_b;

        // Validate balances
        channel.validate_balances()?;

        // Sign the new state
        let msg = channel.message_to_sign();
        let sig_a = signing_key_a.sign(&msg);
        let sig_b = signing_key_b.sign(&msg);

        let sig_a_bytes = sig_a.to_bytes();
        let sig_b_bytes = sig_b.to_bytes();

        channel.sig_a = Some(sig_a_bytes);
        channel.sig_b = Some(sig_b_bytes);

        // Verify signatures
        channel.verify_signatures()?;

        Ok(ChannelState {
            channel_id: *channel_id,
            sequence: new_sequence,
            balance_a: new_balance_a,
            balance_b: new_balance_b,
            sig_a: sig_a_bytes,
            sig_b: sig_b_bytes,
        })
    }

    /// Apply a state update received from the network
    pub fn apply_state_update(
        &mut self,
        state: ChannelState,
    ) -> Result<(), ChannelError> {
        let channel = self.channels.get_mut(&state.channel_id).ok_or(ChannelError::NotFound)?;

        // Verify sequence is increasing
        if state.sequence <= channel.sequence {
            return Err(ChannelError::InvalidSequence);
        }

        // Update channel with new state
        channel.sequence = state.sequence;
        channel.balance_a = state.balance_a;
        channel.balance_b = state.balance_b;
        channel.sig_a = Some(state.sig_a);
        channel.sig_b = Some(state.sig_b);

        // Validate balances and signatures
        channel.validate_balances()?;
        channel.verify_signatures()?;

        Ok(())
    }

    pub fn get_channel(&self, channel_id: &[u8; 32]) -> Option<&Channel> {
        self.channels.get(channel_id)
    }

    /// Get the current state ready for on-chain settlement
    pub fn get_settlement_state(&self, channel_id: &[u8; 32]) -> Option<ChannelState> {
        let channel = self.channels.get(channel_id)?;
        Some(ChannelState {
            channel_id: *channel_id,
            sequence: channel.sequence,
            balance_a: channel.balance_a,
            balance_b: channel.balance_b,
            sig_a: channel.sig_a?,
            sig_b: channel.sig_b?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    #[test]
    fn test_channel_state_update() {
        let mut csprng = OsRng;
        let key_a = SigningKey::generate(&mut csprng);
        let key_b = SigningKey::generate(&mut csprng);

        let pubkey_a = key_a.verifying_key();
        let pubkey_b = key_b.verifying_key();

        let channel_id = [42u8; 32];

        let mut manager = ChannelManager::new();
        manager.create_channel(channel_id, pubkey_a, pubkey_b, 1000, 1000);

        // Update state: transfer 100 from A to B
        let state = manager
            .update_state(&channel_id, 900, 1100, &key_a, &key_b)
            .unwrap();

        assert_eq!(state.sequence, 1);
        assert_eq!(state.balance_a, 900);
        assert_eq!(state.balance_b, 1100);

        // Verify signatures are valid
        let channel = manager.get_channel(&channel_id).unwrap();
        assert!(channel.verify_signatures().is_ok());
    }

    #[test]
    fn test_monotonic_sequence() {
        let mut csprng = OsRng;
        let key_a = SigningKey::generate(&mut csprng);
        let key_b = SigningKey::generate(&mut csprng);

        let pubkey_a = key_a.verifying_key();
        let pubkey_b = key_b.verifying_key();

        let channel_id = [42u8; 32];

        let mut manager = ChannelManager::new();
        manager.create_channel(channel_id, pubkey_a, pubkey_b, 1000, 1000);

        // Multiple updates
        for i in 1..=100 {
            let balance_a = 1000 - i * 10;
            let balance_b = 1000 + i * 10;
            let state = manager
                .update_state(&channel_id, balance_a, balance_b, &key_a, &key_b)
                .unwrap();
            assert_eq!(state.sequence, i as u64);
        }

        let channel = manager.get_channel(&channel_id).unwrap();
        assert_eq!(channel.sequence, 100);
    }

    #[test]
    fn test_invalid_balances_rejected() {
        let mut csprng = OsRng;
        let key_a = SigningKey::generate(&mut csprng);
        let key_b = SigningKey::generate(&mut csprng);

        let pubkey_a = key_a.verifying_key();
        let pubkey_b = key_b.verifying_key();

        let channel_id = [42u8; 32];

        let mut manager = ChannelManager::new();
        manager.create_channel(channel_id, pubkey_a, pubkey_b, 1000, 1000);

        // Try to update with invalid sum
        let result = manager.update_state(&channel_id, 900, 900, &key_a, &key_b);
        assert!(matches!(result, Err(ChannelError::InvalidBalances)));
    }
}
