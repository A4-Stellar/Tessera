use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::crypto::signing::NodeRoundState;
use crate::types::{KeyShare, MpcError, NodeId, Round1Commitment, Round2Response};

/// Independent Server Node holding a single KeyShare in secure memory
pub struct MpcNode {
    pub node_id: NodeId,
    key_share: KeyShare,
    active_sessions: Arc<Mutex<HashMap<String, NodeRoundState>>>,
}

impl MpcNode {
    pub fn new(key_share: KeyShare) -> Self {
        Self {
            node_id: key_share.node_id,
            key_share,
            active_sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Process Round 1 request: generate ephemeral nonce commitment
    pub async fn handle_round1(&self, session_id: &str) -> Result<Round1Commitment, MpcError> {
        let (state, commitment) = NodeRoundState::round1(self.node_id);
        let mut sessions = self.active_sessions.lock().await;
        sessions.insert(session_id.to_string(), state);
        Ok(commitment)
    }

    /// Process Round 2 request: compute partial signature share
    pub async fn handle_round2(
        &self,
        session_id: &str,
        participating_nodes: &[NodeId],
        commitments: &[Round1Commitment],
        message: &[u8],
    ) -> Result<Round2Response, MpcError> {
        let state = {
            let mut sessions = self.active_sessions.lock().await;
            sessions.remove(session_id).ok_or_else(|| {
                MpcError::RoundFailure(format!("Session {} not found on node {}", session_id, self.node_id))
            })?
        };

        state.round2(&self.key_share, participating_nodes, commitments, message)
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.key_share.public_key.compress().to_bytes()
    }

    pub fn master_public_key_bytes(&self) -> [u8; 32] {
        self.key_share.master_public_key.compress().to_bytes()
    }
}
