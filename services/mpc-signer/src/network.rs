use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::node::MpcNode;
use crate::types::{MpcError, NodeId, Round1Commitment, Round2Response};

/// Multi-Party communication network interconnecting 3 server nodes
#[derive(Clone)]
pub struct MpcNetwork {
    nodes: Arc<RwLock<HashMap<NodeId, Arc<MpcNode>>>>,
}

impl MpcNetwork {
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register_node(&self, node: Arc<MpcNode>) {
        let mut map = self.nodes.write().await;
        map.insert(node.node_id, node);
    }

    /// Broadcast Round 1 to participating nodes
    pub async fn broadcast_round1(
        &self,
        session_id: &str,
        participating_nodes: &[NodeId],
    ) -> Result<Vec<Round1Commitment>, MpcError> {
        let nodes = self.nodes.read().await;
        let mut commitments = Vec::new();

        for &id in participating_nodes {
            let node = nodes.get(&id).ok_or(MpcError::NodeNotFound(id))?;
            let comm = node.handle_round1(session_id).await?;
            commitments.push(comm);
        }

        Ok(commitments)
    }

    /// Broadcast Round 2 to participating nodes
    pub async fn broadcast_round2(
        &self,
        session_id: &str,
        participating_nodes: &[NodeId],
        commitments: &[Round1Commitment],
        message: &[u8],
    ) -> Result<Vec<Round2Response>, MpcError> {
        let nodes = self.nodes.read().await;
        let mut responses = Vec::new();

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
