//! Issue #322 - Off-chain channel-node daemon
//!
//! This daemon manages off-chain state exchange for Layer-2 payment channels.
//! It maintains bidirectional micropayment streaming with Ed25519 signatures
//! and monotonic sequence numbers.

mod channel;
mod protocol;

use channel::ChannelManager;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Starting channel-node daemon for Layer-2 micropayment streaming");

    let manager = Arc::new(RwLock::new(ChannelManager::new()));

    // In production, this would be a WebSocket or TCP server accepting connections
    // For demonstration, we'll show an example flow

    info!("Channel node daemon running. Ready for micropayment streams.");
    info!("In production: WebSocket server listening on ws://0.0.0.0:8080/channel");

    // Example: demonstrate micropayment stream (would be driven by network I/O in production)
    demo_micropayment_stream(manager).await;
}

async fn demo_micropayment_stream(manager: Arc<RwLock<ChannelManager>>) {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    // Generate keypairs for demonstration
    let mut csprng = OsRng;
    let party_a_key = SigningKey::generate(&mut csprng);
    let party_b_key = SigningKey::generate(&mut csprng);

    let party_a_pubkey = party_a_key.verifying_key();
    let party_b_pubkey = party_b_key.verifying_key();

    info!("Party A pubkey: {}", hex::encode(party_a_pubkey.as_bytes()));
    info!("Party B pubkey: {}", hex::encode(party_b_pubkey.as_bytes()));

    let channel_id = [1u8; 32];
    let initial_balance_a = 5000;
    let initial_balance_b = 5000;

    // Initialize channel
    let mut mgr = manager.write().await;
    mgr.create_channel(
        channel_id,
        party_a_pubkey,
        party_b_pubkey,
        initial_balance_a,
        initial_balance_b,
    );
    drop(mgr);

    info!("Channel initialized with 5000:5000 balance split");

    // Simulate 5000 micropayments: 1 unit per payment from A to B
    for i in 1..=5000 {
        let new_balance_a = initial_balance_a - i;
        let new_balance_b = initial_balance_b + i;

        let mut mgr = manager.write().await;
        match mgr.update_state(
            &channel_id,
            new_balance_a,
            new_balance_b,
            &party_a_key,
            &party_b_key,
        ) {
            Ok(state) => {
                if i % 1000 == 0 {
                    info!(
                        "Micropayment #{}: balance_a={}, balance_b={}, seq={}",
                        i, state.balance_a, state.balance_b, state.sequence
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Failed to update state at payment {}: {:?}", i, e);
                break;
            }
        }
        drop(mgr);
    }

    let mgr = manager.read().await;
    if let Some(channel) = mgr.get_channel(&channel_id) {
        info!("Final off-chain state after 5000 micropayments:");
        info!("  Sequence: {}", channel.sequence);
        info!("  Balance A: {}", channel.balance_a);
        info!("  Balance B: {}", channel.balance_b);
        info!("  Ready for on-chain cooperative settlement");
    }
}
