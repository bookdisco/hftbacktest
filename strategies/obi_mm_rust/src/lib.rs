//! OBI Market Making Strategy
//!
//! A market making strategy that uses Order Book Imbalance (OBI) as the alpha signal,
//! with volatility-based spread adjustment and stop loss protection.
//!
//! This is the Rust port of the Python obi_mm strategy, designed to work with
//! both backtesting and live trading environments.

pub mod config;
pub mod strategy;
pub mod warmup;

pub use config::{GlobalConfig, StrategyParams};
pub use strategy::ObiMmStrategy;
pub use warmup::{WarmupConfig, WarmupResult};
