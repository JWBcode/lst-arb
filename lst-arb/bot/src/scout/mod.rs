//! Scout Module - Active Defense Mechanisms
//!
//! Provides safety checks and defensive mechanisms including:
//! - Honey pot detection for scam tokens
//! - Token safety verification before trading

mod safety;

pub use safety::{SafetyChecker, TokenSafetyResult};
