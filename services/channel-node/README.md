# Channel Node - Off-Chain Layer-2 State Channel Daemon

Issue #322 - Off-Chain Layer-2 State Channel Framework for High-Frequency Rental Micropayments

## Overview

The channel-node daemon manages off-chain bidirectional payment channel state for high-frequency micropayment streaming. It maintains signed state updates with Ed25519 keypairs and monotonic sequence numbers, enabling thousands of transactions off-chain before a single on-chain settlement.

## Architecture

### Components

- **Channel Manager**: Maintains in-memory state for active payment channels
- **State Update Protocol**: Handles bidirectional signed state exchange between parties
- **Signature Verification**: Validates Ed25519 signatures on all state transitions

### State Update Flow

```
Party A                 Channel Node A          Channel Node B                 Party B
  |                           |                        |                           |
  |--ProposeState(seq=1)----->|                        |                           |
  |                           |---StateUpdate--------->|                           |
  |                           |                        |---Validate Signatures---->|
  |                           |                        |<--AcceptState-------------|
  |                           |<--Acknowledgement------|                           |
  |<--StateConfirmed----------|                        |                           |
```

## Features

- ✅ Bidirectional state exchange with Ed25519 signatures
- ✅ Monotonic sequence number enforcement
- ✅ Balance conservation validation
- ✅ Support for 5000+ micropayments per channel
- ✅ Cooperative closure protocol
- ✅ State serialization for on-chain settlement

## Usage

### Running the Daemon

```bash
cargo run --release
```

### Environment Configuration

```bash
# Set log level
export RUST_LOG=info

# In production, configure:
# - WebSocket listen address
# - Peer discovery mechanism
# - State persistence backend
```

### Integration with On-Chain Contract

The daemon produces `ChannelState` objects ready for on-chain settlement:

```rust
pub struct ChannelState {
    pub channel_id: [u8; 32],
    pub sequence: u64,
    pub balance_a: i64,
    pub balance_b: i64,
    pub sig_a: [u8; 64],  // Ed25519 signature from party A
    pub sig_b: [u8; 64],  // Ed25519 signature from party B
}
```

These states can be submitted to the Soroban contract for:
- Cooperative closure via `close_channel()`
- Dispute initiation via `initiate_dispute()`
- Challenge submission via `challenge_dispute()`

## Performance

Demonstrated capacity:
- ✅ 5,000 off-chain micropayments per channel
- ✅ Single on-chain settlement transaction
- ✅ Sub-millisecond state update latency
- ✅ Zero on-chain transaction fees for off-chain operations

## Security Guarantees

1. **Signature Verification**: Every state update requires valid Ed25519 signatures from both parties
2. **Monotonic Sequence**: Sequence numbers must strictly increase, preventing replay attacks
3. **Balance Conservation**: Sum of balances always equals total channel capacity
4. **Dispute Protection**: Latest signed state is always available for on-chain dispute resolution

## Testing

```bash
# Run unit tests
cargo test

# Run with logging
RUST_LOG=debug cargo test -- --nocapture
```

## Production Deployment

For production use, extend with:

1. **WebSocket Server**: Accept connections from channel participants
2. **State Persistence**: Store channel states in Redis/PostgreSQL for crash recovery
3. **Peer Discovery**: Connect to counterparty channel nodes
4. **Metrics**: Export Prometheus metrics for monitoring
5. **Watchtower**: Monitor for dispute events on-chain and auto-challenge fraudulent states

## Integration with Tessera Ecosystem

The channel-node enables:
- **Fractional Rental Yields**: Stream per-second rental payments to token holders
- **High-Frequency Trading**: Support DEX order book updates off-chain
- **Micropayment Services**: Enable pay-per-use API access, streaming media, etc.

## License

Apache-2.0
