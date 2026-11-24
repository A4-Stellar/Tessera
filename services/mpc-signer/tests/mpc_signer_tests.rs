/* Authorized Protocol Quality Assurance & Formal Verification Test Suite */

use std::sync::atomic::Ordering;
use std::sync::Arc;
use ed25519_dalek::{Signature as DalekSignature, Verifier, VerifyingKey};
use mpc_signer::{
    DistributedKeyGenerator, KeyShare, MpcCoordinator, MpcError, MpcNetwork, MpcNode,
    Round2Response, SignRequest, ThresholdConfig, verify_threshold_signature,
};

/// Sets up a production-grade 3-of-5 FROST MPC cluster
async fn setup_frost_3_of_5_cluster() -> (Arc<MpcCoordinator>, Arc<MpcNetwork>, [u8; 32], Vec<KeyShare>) {
    let (master_public_key, shares) = DistributedKeyGenerator::generate_3_of_5();
    let network = Arc::new(MpcNetwork::new());

    for share in &shares {
        let node = Arc::new(MpcNode::new(share.clone()));
        network.register_node(node).await;
    }

    let config = ThresholdConfig::default(); // default 3-of-5
    let master_pk_bytes = master_public_key.compress().to_bytes();
    let coordinator = Arc::new(MpcCoordinator::with_key_shares(
        config,
        network.clone(),
        master_public_key,
        shares.clone(),
    ));

    (coordinator, network, master_pk_bytes, shares)
}

/// Sets up legacy 2-of-3 cluster for backward compatibility tests
async fn setup_mpc_2_of_3_cluster() -> (Arc<MpcCoordinator>, [u8; 32]) {
    let (master_public_key, shares) = DistributedKeyGenerator::generate_2_of_3();
    let network = Arc::new(MpcNetwork::new());

    for share in &shares {
        let node = Arc::new(MpcNode::new(share.clone()));
        network.register_node(node).await;
    }

    let config = ThresholdConfig::new(2, 3).unwrap();
    let master_pk_bytes = master_public_key.compress().to_bytes();
    let coordinator = Arc::new(MpcCoordinator::with_key_shares(
        config,
        network,
        master_public_key,
        shares,
    ));

    (coordinator, master_pk_bytes)
}

#[tokio::test]
async fn test_frost_3_of_5_signing_nodes_1_2_3() {
    let (coordinator, _, master_pk, _) = setup_frost_3_of_5_cluster().await;
    let tx_payload = b"Stellar Admin Action: Authorize Soroban Upgrade".to_vec();

    let req = SignRequest {
        session_id: "frost-session-1-2-3".to_string(),
        transaction_payload: tx_payload.clone(),
        participating_nodes: vec![1, 2, 3],
    };

    let sig = coordinator
        .sign_transaction(req)
        .await
        .expect("FROST 3-of-5 sign failed");

    // Formal verification: Check threshold signature equation
    assert!(verify_threshold_signature(&sig, &tx_payload).is_ok());

    // Standard RFC 8032 Ed25519 Verifier compatibility check
    let verifying_key = VerifyingKey::from_bytes(&master_pk).expect("invalid verifying key");
    let dalek_sig = DalekSignature::from_bytes(&sig.to_bytes());
    assert!(verifying_key.verify(&tx_payload, &dalek_sig).is_ok());
}

#[tokio::test]
async fn test_frost_3_of_5_signing_different_subsets() {
    let (coordinator, _, master_pk, _) = setup_frost_3_of_5_cluster().await;
    let tx_payload = b"Stellar RWA Issuance Mint 5,000,000 USD Tokens".to_vec();
    let verifying_key = VerifyingKey::from_bytes(&master_pk).unwrap();

    let subsets: Vec<Vec<u8>> = vec![
        vec![2, 3, 4],
        vec![3, 4, 5],
        vec![1, 3, 5],
        vec![2, 4, 5],
    ];

    for subset in subsets {
        let session = format!("session-subset-{:?}", subset);
        let req = SignRequest {
            session_id: session,
            transaction_payload: tx_payload.clone(),
            participating_nodes: subset,
        };

        let sig = coordinator.sign_transaction(req).await.expect("subset sign failed");
        assert!(verify_threshold_signature(&sig, &tx_payload).is_ok());

        let dalek_sig = DalekSignature::from_bytes(&sig.to_bytes());
        assert!(verifying_key.verify(&tx_payload, &dalek_sig).is_ok());
    }
}

#[tokio::test]
async fn test_frost_invariant_insufficient_signers_rejected() {
    let (coordinator, _, _, _) = setup_frost_3_of_5_cluster().await;
    let tx_payload = b"Unauthorized unilateral action".to_vec();

    // 1 signer (must be rejected)
    let req1 = SignRequest {
        session_id: "session-1-only".to_string(),
        transaction_payload: tx_payload.clone(),
        participating_nodes: vec![1],
    };
    assert!(coordinator.sign_transaction(req1).await.is_err());

    // 2 signers (must be rejected for 3-of-5 threshold)
    let req2 = SignRequest {
        session_id: "session-2-only".to_string(),
        transaction_payload: tx_payload,
        participating_nodes: vec![1, 2],
    };
    let res = coordinator.sign_transaction(req2).await;
    assert!(matches!(res, Err(MpcError::InsufficientSigners { threshold: 3, actual: 2 })));
}

#[tokio::test]
async fn test_frost_round1_message_loss_deterministic_retransmission() {
    let (coordinator, network, master_pk, _) = setup_frost_3_of_5_cluster().await;
    let tx_payload = b"Payload experiencing network packet drops".to_vec();

    // Node 2 drops its first 2 transmission attempts in Round 1
    network.simulate_packet_drop(2, 2).await;

    let req = SignRequest {
        session_id: "session-with-packet-drops".to_string(),
        transaction_payload: tx_payload.clone(),
        participating_nodes: vec![1, 2, 3],
    };

    // Re-transmission recovers and signing succeeds!
    let sig = coordinator
        .sign_transaction(req)
        .await
        .expect("Deterministic re-transmission must recover dropped Round 1 messages");

    assert!(network.retransmission_count.load(Ordering::SeqCst) >= 2);
    assert!(verify_threshold_signature(&sig, &tx_payload).is_ok());

    let verifying_key = VerifyingKey::from_bytes(&master_pk).unwrap();
    let dalek_sig = DalekSignature::from_bytes(&sig.to_bytes());
    assert!(verifying_key.verify(&tx_payload, &dalek_sig).is_ok());
}

#[tokio::test]
async fn test_frost_byzantine_node_detection_and_fault_isolation() {
    let (coordinator, _, master_pk, shares) = setup_frost_3_of_5_cluster().await;
    let tx_payload = b"Security critical Treasury settlement payout".to_vec();

    // Create a scenario where Node 2 acts Byzantinely by submitting a corrupted signature share
    // Generate valid Round 1 commitments
    let (state1, comm1) = mpc_signer::crypto::signing::NodeRoundState::round1(1);
    let (_state2, comm2) = mpc_signer::crypto::signing::NodeRoundState::round1(2);
    let (state3, comm3) = mpc_signer::crypto::signing::NodeRoundState::round1(3);

    let commitments = vec![comm1, comm2, comm3];
    let participating = vec![1, 2, 3];

    // Node 1 and Node 3 compute honest shares
    let share1 = shares.iter().find(|s| s.node_id == 1).unwrap();
    let share3 = shares.iter().find(|s| s.node_id == 3).unwrap();

    let resp1 = state1.round2(share1, &participating, &commitments, &tx_payload).unwrap();
    let resp3 = state3.round2(share3, &participating, &commitments, &tx_payload).unwrap();

    // Node 2 acts Byzantinely: submits corrupted/tampered bytes
    let byzantine_resp2 = Round2Response {
        node_id: 2,
        response_bytes: [0xDE; 32], // Malicious corrupted partial signature
    };

    let responses = vec![resp1, byzantine_resp2, resp3];

    // Aggregation MUST detect Node 2's Byzantine behavior and reject
    let agg_result = mpc_signer::crypto::signing::aggregate_signature(
        &commitments,
        &responses,
        &shares,
        &shares[0].master_public_key,
        &tx_payload,
    );

    match agg_result {
        Err(MpcError::InvalidPartialSignature(byzantine_id)) => {
            assert_eq!(byzantine_id, 2, "Coordinator correctly pinpointed Byzantine node 2");
        }
        other => panic!("Expected Byzantine node 2 detection, got {:?}", other),
    }

    // Now isolate Byzantine node 2 and execute with honest quorum {1, 3, 4}
    let honest_req = SignRequest {
        session_id: "session-isolated-byzantine-recovery".to_string(),
        transaction_payload: tx_payload.clone(),
        participating_nodes: vec![1, 3, 4],
    };

    let recovery_sig = coordinator
        .sign_transaction(honest_req)
        .await
        .expect("Quorum of honest nodes must succeed after isolating Byzantine node");

    let verifying_key = VerifyingKey::from_bytes(&master_pk).unwrap();
    let dalek_sig = DalekSignature::from_bytes(&recovery_sig.to_bytes());
    assert!(verifying_key.verify(&tx_payload, &dalek_sig).is_ok());
}

#[tokio::test]
async fn test_mpc_2_of_3_backward_compatibility() {
    let (coordinator, master_pk) = setup_mpc_2_of_3_cluster().await;
    let tx_payload = b"Backward compatibility 2-of-3 transaction".to_vec();

    let req = SignRequest {
        session_id: "compat-2-of-3".to_string(),
        transaction_payload: tx_payload.clone(),
        participating_nodes: vec![1, 2],
    };

    let sig = coordinator.sign_transaction(req).await.expect("2-of-3 must succeed");
    assert!(verify_threshold_signature(&sig, &tx_payload).is_ok());

    let verifying_key = VerifyingKey::from_bytes(&master_pk).unwrap();
    let dalek_sig = DalekSignature::from_bytes(&sig.to_bytes());
    assert!(verifying_key.verify(&tx_payload, &dalek_sig).is_ok());
}
