use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::node::MpcNode;
use crate::types::{MpcError, NodeId, Round1Commitment, Round2Response};

/// Multi-Party communication network interconnecting MPC server nodes.
///
/// Includes deterministic message loss simulation and automatic re-transmission
/// for FROST Round 1 commitment broadcast (Issue #46 / #427).
#[derive(Clone)]
pub struct MpcNetwork {
    nodes: Arc<RwLock<HashMap<NodeId, Arc<MpcNode>>>>,
    max_retries: usize,
    /// Simulates transient packet drops per node: (node_id -> remaining drops before success)
    simulated_packet_drops: Arc<RwLock<HashMap<NodeId, usize>>>,
    /// Global counter tracking total re-transmissions performed
    pub retransmission_count: Arc<AtomicUsize>,
}

impl MpcNetwork {
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(RwLock::new(HashMap::new())),
            max_retries: 3,
            simulated_packet_drops: Arc::new(RwLock::new(HashMap::new())),
            retransmission_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn with_max_retries(max_retries: usize) -> Self {
        Self {
            nodes: Arc::new(RwLock::new(HashMap::new())),
            max_retries,
            simulated_packet_drops: Arc::new(RwLock::new(HashMap::new())),
            retransmission_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn register_node(&self, node: Arc<MpcNode>) {
        let mut map = self.nodes.write().await;
        map.insert(node.node_id, node);
    }

    /// Configures simulated transient packet drops for a given node to test re-transmission.
    pub async fn simulate_packet_drop(&self, node_id: NodeId, drops: usize) {
        let mut map = self.simulated_packet_drops.write().await;
        map.insert(node_id, drops);
    }

    /// Broadcast FROST Round 1 to participating nodes with deterministic re-transmission.
    pub async fn broadcast_round1(
        &self,
        session_id: &str,
        participating_nodes: &[NodeId],
    ) -> Result<Vec<Round1Commitment>, MpcError> {
        let nodes = self.nodes.read().await;
        let mut commitments = Vec::with_capacity(participating_nodes.len());

        for &id in participating_nodes {
            let node = nodes.get(&id).ok_or(MpcError::NodeNotFound(id))?;

            // Deterministic re-transmission loop
            let mut delivered = false;
            let mut last_error = None;

            for attempt in 0..=self.max_retries {
                // Check if simulated packet loss applies
                let should_drop = {
                    let mut drops = self.simulated_packet_drops.write().await;
                    if let Some(remaining) = drops.get_mut(&id) {
                        if *remaining > 0 {
                            *remaining -= 1;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };

                if should_drop {
                    self.retransmission_count.fetch_add(1, Ordering::SeqCst);
                    last_error = Some(MpcError::NetworkError(format!(
                        "Round 1 packet dropped on attempt {} for node {}",
                        attempt + 1,
                        id
                    )));
                    // Retry deterministically
                    continue;
                }

                match node.handle_round1(session_id).await {
                    Ok(comm) => {
                        commitments.push(comm);
                        delivered = true;
                        break;
                    }
                    Err(e) => {
                        self.retransmission_count.fetch_add(1, Ordering::SeqCst);
                        last_error = Some(e);
                    }
                }
            }

            if !delivered {
                return Err(last_error.unwrap_or_else(|| {
                    MpcError::NetworkError(format!(
                        "Exhausted {} retries delivering Round 1 to node {}",
                        self.max_retries, id
                    ))
                }));
            }
        }

        Ok(commitments)
    }

    /// Broadcast FROST Round 2 to participating nodes.
    pub async fn broadcast_round2(
        &self,
        session_id: &str,
        participating_nodes: &[NodeId],
        commitments: &[Round1Commitment],
        message: &[u8],
    ) -> Result<Vec<Round2Response>, MpcError> {
        let nodes = self.nodes.read().await;
        let mut responses = Vec::with_capacity(participating_nodes.len());

        for &id in participating_nodes {
            let node = nodes.get(&id).ok_or(MpcError::NodeNotFound(id))?;
            let resp = node
                .handle_round2(session_id, participating_nodes, commitments, message)
                .await?;
            responses.push(resp);
        }

        Ok(responses)
    }
}
