//! Signing backends for transactions the API cannot submit itself.
//!
//! The API server only reads chain state, but the RWA contracts also need
//! scheduled writes — daily dividend distributions, rent payouts and registry
//! updates. Those are submitted by a trusted backend job, and the keys that
//! authorise them must never live in an environment variable. See
//! [`kms`] for the HSM / AWS KMS signing path.

pub mod kms;

pub use kms::{
    ed25519_public_key_from_spki, envelope_digest, network_id, AwsCredentials, AwsKmsClient,
    KmsClient, KmsError, KmsSignRequest, KmsSignResponse, KmsSigner, MessageType, SecretBytes,
    SignerError, SigningAlgorithm, MAINNET_PASSPHRASE, MAX_SIGNATURES, TESTNET_PASSPHRASE,
};
