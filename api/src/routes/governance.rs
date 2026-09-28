//! Governance API routes for querying the Governance contract.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::indexer::AppState;
use crate::models::ApiErrorBody;
use crate::routes::ApiError;

#[derive(Debug, Serialize, Deserialize)]
pub struct ProposalResponse {
    pub proposals: Vec<Proposal>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub title: String,
    pub description: String,
    pub proposer: String,
    pub start_time: u64,
    pub end_time: u64,
    pub quorum: String,
    pub votes_for: String,
    pub votes_against: String,
    pub votes_abstain: String,
    pub executed: bool,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VoteWeightResponse {
    pub address: String,
    pub weight: String,
}

/// GET /v1/governance/:contract_address/proposals
///
/// Returns all proposals for a governance contract.
pub async fn list_proposals(
    State(state): State<Arc<AppState>>,
    Path(contract_address): Path<String>,
) -> Result<Json<ProposalResponse>, ApiError> {
    // In a real implementation, this would query the Soroban contract
    // For now, return mock data to satisfy the GovernancePortal component
    let proposals = vec![
        Proposal {
            id: "1".to_string(),
            title: "Upgrade Asset Token Contract".to_string(),
            description: "Proposal to upgrade the asset token contract to v2.1 with enhanced compliance features.".to_string(),
            proposer: "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF".to_string(),
            start_time: 1700000000,
            end_time: 1700086400,
            quorum: "1000000".to_string(),
            votes_for: "750000".to_string(),
            votes_against: "100000".to_string(),
            votes_abstain: "50000".to_string(),
            executed: false,
            status: "active".to_string(),
        },
        Proposal {
            id: "2".to_string(),
            title: "Increase Dividend Distribution Cap".to_string(),
            description: "Proposal to increase the maximum dividend distribution per period from 5% to 7.5% of TVL.".to_string(),
            proposer: "GBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBWHF".to_string(),
            start_time: 1699913600,
            end_time: 1700000000,
            quorum: "1000000".to_string(),
            votes_for: "600000".to_string(),
            votes_against: "300000".to_string(),
            votes_abstain: "100000".to_string(),
            executed: true,
            status: "executed".to_string(),
        },
    ];

    Ok(Json(ProposalResponse { proposals }))
}

/// GET /v1/governance/:contract_address/vote-weight/:address
///
/// Returns the voting weight for a specific address.
pub async fn get_vote_weight(
    State(_state): State<Arc<AppState>>,
    Path((contract_address, address)): Path<(String, String)>,
) -> Result<Json<VoteWeightResponse>, ApiError> {
    // In a real implementation, this would query the Soroban contract
    // For now, return mock data
    let weight = "125000".to_string();

    Ok(Json(VoteWeightResponse {
        address,
        weight,
    }))
}

/// GET /v1/governance/:contract_address/proposal/:id
///
/// Returns a single proposal by ID.
pub async fn get_proposal(
    State(_state): State<Arc<AppState>>,
    Path((contract_address, proposal_id)): Path<(String, String)>,
) -> Result<Json<Proposal>, ApiError> {
    // In a real implementation, this would query the Soroban contract
    let proposal = Proposal {
        id: proposal_id.clone(),
        title: "Sample Proposal".to_string(),
        description: "This is a sample proposal for demonstration.".to_string(),
        proposer: "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF".to_string(),
        start_time: 1700000000,
        end_time: 1700086400,
        quorum: "1000000".to_string(),
        votes_for: "750000".to_string(),
        votes_against: "100000".to_string(),
        votes_abstain: "50000".to_string(),
        executed: false,
        status: "active".to_string(),
    };

    Ok(Json(proposal))
}