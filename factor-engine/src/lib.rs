//! # Factor Engine
//!
//! A streaming factor computation engine for high-frequency trading.
//!
//! This crate provides O(1) incremental computation primitives and factor implementations
//! that can be used with the hftbacktest framework.
//!
//! ## Core Components
//!
//! - **Primitives**: `RingBuffer`, `WelfordRolling`, `EMA`, `TimeWindowQueue`
//! - **Factors**: `OBIFactor`, `VolatilityFactor`, `TradeImbalanceFactor`
//! - **Combiner**: `LinearCombiner` for multi-factor weighted combination
//!
//! ## Example
//!
//! ```rust,ignore
//! use factor_engine::{OBIFactor, VolatilityFactor, LinearCombiner};
//!
//! let mut obi = OBIFactor::new(0.001, 3600);
//! let mut vol = VolatilityFactor::new(60_000_000_000, 60);
//! let mut combiner = LinearCombiner::new(&[0.6, -0.4]);
//!
//! // In your strategy loop:
//! let obi_value = obi.update(&depth);
//! let vol_value = vol.update(timestamp, mid_price);
//! let alpha = combiner.compute(&[obi_value, vol_value]);
//! ```

pub mod combiner;
pub mod factors;
pub mod primitives;

// Re-exports
pub use combiner::LinearCombiner;
pub use factors::{
    obi::OBIFactor,
    stop_loss::StopLossChecker,
    volatility::VolatilityFactor,
    Factor,
};
pub use primitives::{EMA, RingBuffer, TimeWindowQueue, WelfordRolling};
