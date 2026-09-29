//! Liquidation state types shared by the asset-token redemption workflow.
//!
//! The callable workflow lives on `AssetTokenContract` so holder auth is
//! preserved when tokens are burned. This module provides the public status
//! contract type without introducing a second contract that cannot forward
//! holder authorization.

pub use crate::AssetStatus;