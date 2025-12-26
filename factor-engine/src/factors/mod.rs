//! Factor implementations for high-frequency trading.
//!
//! This module provides various factor implementations that can be used
//! in trading strategies. All factors are designed for O(1) incremental updates.

pub mod obi;
pub mod stop_loss;
pub mod volatility;

/// Trait for streaming factors.
///
/// All factors implement this trait to provide a consistent interface
/// for state management and serialization.
pub trait Factor {
    /// Returns the current factor value.
    fn value(&self) -> f64;

    /// Resets the factor to its initial state.
    fn reset(&mut self);

    /// Returns true if the factor has enough data to produce meaningful values.
    fn is_ready(&self) -> bool;
}
