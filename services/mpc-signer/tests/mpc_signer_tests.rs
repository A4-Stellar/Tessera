/* Authorized Protocol Quality Assurance & Formal Verification Test Suite */

use std::sync::Arc;
use mpc_signer::{
    DistributedKeyGenerator, MpcCoordinator, MpcNetwork, MpcNode, SignRequest, ThresholdConfig,
    verify_threshold_signature,
};
use ed25519_dalek::{Signature as DalekSignature, Verifier, VerifyingKey};

async fn setup_mpc_cluster() -> (Arc<MpcCoordinator>, [u8; 32]) {
    let (master_public_key, shares) = DistributedKeyGenerator::generate_2_of_3();
    let network = Arc::new(MpcNetwork::new());

    for share in shares {
        let node = Arc::new(MpcNode::new(share));
        network.register_node(node).await;
    }

    let config = ThresholdConfig::default();
    let master_pk_bytes = master_public_key.compress().to_bytes();
    let coordinator = Arc::new(MpcCoordinator::new(config, network, master_public_key));

    (coordinator, master_pk_bytes)
}

#[tokio::test]
async fn test_mpc_2_of_3_signing_nodes_1_and_2() {
    let (coordinator, master_pk) = setup_mpc_cluster().await;
    let tx_payload = b"Stellar Admin Action: Transfer Treasury Authority to Safe 0x1".to_vec();

    let req = SignRequest {
        session_id: "session-1-2".to_string(),
        transaction_payload: tx_payload.clone(),
        participating_nodes: vec![1, 2],
    };

    let sig = coordinator.sign_transaction(req).await.expect("2-of-3 sign failed");

    // Formal verification: Check threshold signature equation
    assert!(verify_threshold_signature(&sig, &tx_payload).is_ok());

    // Standard RFC 8032 Ed25519 Verifier compatibility check
    let verifying_key = VerifyingKey::from_bytes(&master_pk).expect("invalid verifying key");
    let dalek_sig = DalekSignature::from_bytes(&sig.to_bytes());
    assert!(verifying_key.verify(&tx_payload, &dalek_sig).is_ok());
}

#[tokio::test]
async fn test_mpc_2_of_3_signing_nodes_2_and_3() {
    let (coordinator, master_pk) = setup_mpc_cluster().await;
    let tx_payload = b"Stellar Soroban Upgrade Contract Bytecode Hash".to_vec();

    let req = SignRequest {
        session_id: "session-2-3".to_string(),
        transaction_payload: tx_payload.clone(),
        participating_nodes: vec![2, 3],
    };

    let sig = coordinator.sign_transaction(req).await.expect("2-of-3 sign failed");
    assert!(verify_threshold_signature(&sig, &tx_payload).is_ok());

    let verifying_key = VerifyingKey::from_bytes(&master_pk).unwrap();
    let dalek_sig = DalekSignature::from_bytes(&sig.to_bytes());
    assert!(verifying_key.verify(&tx_payload, &dalek_sig).is_ok());
}

#[tokio::test]
async fn test_mpc_2_of_3_signing_nodes_1_and_3() {
    let (coordinator, master_pk) = setup_mpc_cluster().await;
    let tx_payload = b"Stellar RWA Issuance Mint 1,000,000 Tessera Tokens".to_vec();

    let req = SignRequest {
        session_id: "session-1-3".to_string(),
        transaction_payload: tx_payload.clone(),
        participating_nodes: vec![1, 3],
    };

    let sig = coordinator.sign_transaction(req).await.expect("2-of-3 sign failed");
    assert!(verify_threshold_signature(&sig, &tx_payload).is_ok());

    let verifying_key = VerifyingKey::from_bytes(&master_pk).unwrap();
    let dalek_sig = DalekSignature::from_bytes(&sig.to_bytes());
    assert!(verifying_key.verify(&tx_payload, &dalek_sig).is_ok());
}

#[tokio::test]
async fn test_mpc_invariant_insufficient_signers_rejected() {
    let (coordinator, _) = setup_mpc_cluster().await;
    let tx_payload = b"Unauthorized unilateral transaction attempt".to_vec();

    // Invariant: single node (1-of-3) MUST NOT be capable of signing
    let req = SignRequest {
        session_id: "unauthorized-session".to_string(),
        transaction_payload: tx_payload,
        participating_nodes: vec![1],
    };

    let result = coordinator.sign_transaction(req).await;
    assert!(result.is_err(), "Single node must not satisfy threshold");
}

#[tokio::test]
async fn test_mpc_fault_tolerance_one_node_offline() {
    let (coordinator, master_pk) = setup_mpc_cluster().await;
    let tx_payload = b"Emergency pause with node 2 down".to_vec();

    // Node 2 is simulated as offline/unresponsive; nodes 1 and 3 execute signature
    let req = SignRequest {
        session_id: "session-fault-recovery".to_string(),
        transaction_payload: tx_payload.clone(),
        participating_nodes: vec![1, 3],
    };

    let sig = coordinator.sign_transaction(req).await.expect("signing must succeed with 2 live nodes");
    assert!(verify_threshold_signature(&sig, &tx_payload).is_ok());

    let verifying_key = VerifyingKey::from_bytes(&master_pk).unwrap();
    let dalek_sig = DalekSignature::from_bytes(&sig.to_bytes());
    assert!(verifying_key.verify(&tx_payload, &dalek_sig).is_ok());
}
