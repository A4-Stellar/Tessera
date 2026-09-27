This PR implements solutions for several expert and hard micro-tasks across both contracts and the API:

- **Contracts (AMM)**: Implemented a constant-product Automated Market Maker (`contracts/amm/src/lib.rs`) that intercepts liquidity and swap calls to verify cross-contract `is_allowed` compliance checks.
- **Contracts (Compliance)**: Added a zero-knowledge investor identity verification circuit skeleton (`contracts/compliance/src/zk_verifier.rs`) for Groth16 zk-SNARK proofs on Soroban.
- **API (GraphQL)**: Built a GraphQL layer with subscriptions and schema stitching (`api/src/graphql/mod.rs`), integrating with Axum and providing an interactive GraphiQL playground at `/graphql`.
- **API (Events)**: Developed a Soroban RPC event ledger indexing pipeline using NATS (`api/src/events/kafka_producer.rs`) that partitions events by contract address and supports a Dead-Letter Queue (DLQ).

closes #34
closes #39
closes #44
closes #45
