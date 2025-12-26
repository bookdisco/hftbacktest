//! Factor Combiner
//!
//! Provides utilities for combining multiple factors into a single signal.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Linear combiner for multi-factor signals.
///
/// Combines multiple factor values using weighted linear combination:
/// `combined = sum(weight[i] * factor[i])`
///
/// # Example
///
/// ```rust
/// use factor_engine::LinearCombiner;
///
/// // Create combiner with 3 factors
/// let mut combiner = LinearCombiner::new(&[0.4, 0.3, -0.3]);
///
/// // Compute combined signal
/// let factors = [0.5, -0.2, 0.8];
/// let combined = combiner.compute(&factors);
/// // combined = 0.4 * 0.5 + 0.3 * (-0.2) + (-0.3) * 0.8
/// //          = 0.2 - 0.06 - 0.24 = -0.1
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LinearCombiner {
    /// Weights for each factor
    weights: Vec<f64>,
    /// Last computed combined value
    combined_value: f64,
}

impl LinearCombiner {
    /// Creates a new linear combiner with the specified weights.
    #[inline]
    pub fn new(weights: &[f64]) -> Self {
        Self {
            weights: weights.to_vec(),
            combined_value: 0.0,
        }
    }

    /// Creates a combiner with equal weights (1/n for each factor).
    #[inline]
    pub fn equal_weights(n: usize) -> Self {
        let weight = 1.0 / n as f64;
        Self {
            weights: vec![weight; n],
            combined_value: 0.0,
        }
    }

    /// Computes the weighted combination of factor values.
    ///
    /// # Arguments
    ///
    /// * `factors` - Slice of factor values (must match the number of weights)
    ///
    /// # Returns
    ///
    /// The weighted linear combination.
    ///
    /// # Panics
    ///
    /// Panics if the number of factors doesn't match the number of weights.
    #[inline]
    pub fn compute(&mut self, factors: &[f64]) -> f64 {
        debug_assert_eq!(
            factors.len(),
            self.weights.len(),
            "Number of factors must match number of weights"
        );

        self.combined_value = factors
            .iter()
            .zip(self.weights.iter())
            .map(|(f, w)| f * w)
            .sum();

        self.combined_value
    }

    /// Computes the combination without storing the result.
    #[inline]
    pub fn compute_stateless(&self, factors: &[f64]) -> f64 {
        factors
            .iter()
            .zip(self.weights.iter())
            .map(|(f, w)| f * w)
            .sum()
    }

    /// Returns the last computed combined value.
    #[inline]
    pub fn value(&self) -> f64 {
        self.combined_value
    }

    /// Updates a specific weight.
    #[inline]
    pub fn set_weight(&mut self, index: usize, weight: f64) {
        if index < self.weights.len() {
            self.weights[index] = weight;
        }
    }

    /// Updates all weights.
    #[inline]
    pub fn set_weights(&mut self, weights: &[f64]) {
        self.weights.clear();
        self.weights.extend_from_slice(weights);
    }

    /// Returns the current weights.
    #[inline]
    pub fn weights(&self) -> &[f64] {
        &self.weights
    }

    /// Returns the number of factors.
    #[inline]
    pub fn len(&self) -> usize {
        self.weights.len()
    }

    /// Returns true if the combiner has no factors.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }

    /// Normalizes weights so they sum to 1.
    #[inline]
    pub fn normalize_weights(&mut self) {
        let sum: f64 = self.weights.iter().map(|w| w.abs()).sum();
        if sum > 1e-10 {
            for w in &mut self.weights {
                *w /= sum;
            }
        }
    }
}

impl Default for LinearCombiner {
    fn default() -> Self {
        Self::new(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linear_combiner() {
        let mut combiner = LinearCombiner::new(&[0.5, 0.3, 0.2]);

        let factors = [1.0, 2.0, 3.0];
        let result = combiner.compute(&factors);

        // 0.5 * 1 + 0.3 * 2 + 0.2 * 3 = 0.5 + 0.6 + 0.6 = 1.7
        assert!((result - 1.7).abs() < 1e-10);
    }

    #[test]
    fn test_equal_weights() {
        let mut combiner = LinearCombiner::equal_weights(3);

        let factors = [1.0, 2.0, 3.0];
        let result = combiner.compute(&factors);

        // (1 + 2 + 3) / 3 = 2.0
        assert!((result - 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_normalize_weights() {
        let mut combiner = LinearCombiner::new(&[2.0, -1.0, 1.0]);
        combiner.normalize_weights();

        let sum: f64 = combiner.weights().iter().map(|w| w.abs()).sum();
        assert!((sum - 1.0).abs() < 1e-10);
    }
}
