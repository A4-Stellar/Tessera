//! Jurisdictional tax withholding facade for issue #266.
//!
//! The dividend contract already contains the treaty-rate implementation in
//! `tax_withholding`. This module exposes the requested domain-specific path
//! while keeping the existing contract type and storage layout unchanged.

pub use super::tax_withholding::{DataKey, Error, TaxWithholdingContract};