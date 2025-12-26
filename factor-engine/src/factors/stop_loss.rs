//! Stop Loss Checker
//!
//! Monitors volatility and price changes to trigger stop loss conditions.

use super::Factor;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Stop loss status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StopLossStatus {
    /// Normal operation - no stop loss triggered
    Normal = 0,
    /// Stop loss triggered due to volatility or price change
    Triggered = 1,
    /// Cooldown period after stop loss
    Cooldown = 2,
}

/// Stop loss checker based on volatility and price change thresholds.
///
/// This component monitors market conditions and triggers stop loss when:
/// - Volatility exceeds a threshold (based on z-score)
/// - Price change exceeds a threshold (based on z-score)
///
/// # Example
///
/// ```rust,ignore
/// use factor_engine::StopLossChecker;
///
/// let mut stop_loss = StopLossChecker::new(
///     0.005,   // volatility_mean (historical)
///     0.002,   // volatility_std (historical)
///     0.001,   // change_mean (historical)
///     0.003,   // change_std (historical)
///     3.0,     // volatility threshold (z-score)
///     3.0,     // change threshold (z-score)
/// );
///
/// // In your strategy loop:
/// let status = stop_loss.check(current_volatility, minute_change);
/// if status != StopLossStatus::Normal {
///     // Execute stop loss logic
/// }
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct StopLossChecker {
    /// Historical mean volatility
    volatility_mean: f64,
    /// Historical std of volatility
    volatility_std: f64,
    /// Historical mean price change
    change_mean: f64,
    /// Historical std of price change
    change_std: f64,
    /// Threshold for volatility z-score to trigger stop loss
    volatility_threshold: f64,
    /// Threshold for change z-score to trigger stop loss
    change_threshold: f64,
    /// Current status
    status: StopLossStatus,
}

impl StopLossChecker {
    /// Creates a new stop loss checker.
    ///
    /// # Arguments
    ///
    /// * `volatility_mean` - Historical mean volatility
    /// * `volatility_std` - Historical std of volatility
    /// * `change_mean` - Historical mean price change
    /// * `change_std` - Historical std of price change
    /// * `volatility_threshold` - Z-score threshold for volatility
    /// * `change_threshold` - Z-score threshold for price change
    #[inline]
    pub fn new(
        volatility_mean: f64,
        volatility_std: f64,
        change_mean: f64,
        change_std: f64,
        volatility_threshold: f64,
        change_threshold: f64,
    ) -> Self {
        Self {
            volatility_mean,
            volatility_std,
            change_mean,
            change_std,
            volatility_threshold,
            change_threshold,
            status: StopLossStatus::Normal,
        }
    }

    /// Checks the current market conditions and returns stop loss status.
    ///
    /// # Arguments
    ///
    /// * `volatility` - Current rolling volatility
    /// * `minute_change` - Recent price change (e.g., 1-minute return)
    ///
    /// # Returns
    ///
    /// The current stop loss status.
    #[inline]
    pub fn check(&mut self, volatility: f64, minute_change: f64) -> StopLossStatus {
        // Compute z-scores
        let vol_zscore = if self.volatility_std > 1e-10 {
            (volatility - self.volatility_mean) / self.volatility_std
        } else {
            0.0
        };

        let change_zscore = if self.change_std > 1e-10 {
            (minute_change.abs() - self.change_mean) / self.change_std
        } else {
            0.0
        };

        // Check thresholds
        if vol_zscore > self.volatility_threshold || change_zscore > self.change_threshold {
            self.status = StopLossStatus::Triggered;
        } else {
            self.status = StopLossStatus::Normal;
        }

        self.status
    }

    /// Returns the current stop loss status.
    #[inline]
    pub fn status(&self) -> StopLossStatus {
        self.status
    }

    /// Returns the historical mean volatility.
    #[inline]
    pub fn volatility_mean(&self) -> f64 {
        self.volatility_mean
    }

    /// Resets the status to normal.
    #[inline]
    pub fn reset_status(&mut self) {
        self.status = StopLossStatus::Normal;
    }

    /// Returns true if stop loss is triggered.
    #[inline]
    pub fn is_triggered(&self) -> bool {
        self.status != StopLossStatus::Normal
    }
}

impl Factor for StopLossChecker {
    #[inline]
    fn value(&self) -> f64 {
        self.status as u8 as f64
    }

    #[inline]
    fn reset(&mut self) {
        self.status = StopLossStatus::Normal;
    }

    #[inline]
    fn is_ready(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stop_loss_normal() {
        let mut checker = StopLossChecker::new(0.005, 0.002, 0.001, 0.003, 3.0, 3.0);

        // Normal conditions
        let status = checker.check(0.005, 0.001);
        assert_eq!(status, StopLossStatus::Normal);
    }

    #[test]
    fn test_stop_loss_volatility_trigger() {
        let mut checker = StopLossChecker::new(0.005, 0.002, 0.001, 0.003, 3.0, 3.0);

        // High volatility (> 3 std above mean)
        let high_vol = 0.005 + 3.5 * 0.002; // 3.5 std above mean
        let status = checker.check(high_vol, 0.001);
        assert_eq!(status, StopLossStatus::Triggered);
    }

    #[test]
    fn test_stop_loss_change_trigger() {
        let mut checker = StopLossChecker::new(0.005, 0.002, 0.001, 0.003, 3.0, 3.0);

        // Large price change (> 3 std above mean)
        let large_change = 0.001 + 3.5 * 0.003; // 3.5 std above mean
        let status = checker.check(0.005, large_change);
        assert_eq!(status, StopLossStatus::Triggered);
    }
}
