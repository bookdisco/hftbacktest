//! Data conversion utilities for HftBacktest.
//!
//! This module provides functionality to convert market data from various sources
//! (like Tardis.dev) into the Event format used by HftBacktest.

mod tardis;
mod validation;

pub use tardis::{convert, convert_tardis, TardisConvertConfig, SnapshotMode};
#[cfg(feature = "backtest")]
pub use tardis::{write_npz_file, convert_and_save};
pub use validation::{
    correct_local_timestamp, correct_event_order, validate_event_order,
    argsort_by_exch_ts, argsort_by_local_ts,
};
