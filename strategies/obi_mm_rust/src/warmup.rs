//! Warmup module for pre-loading factor state from historical data.
//!
//! This module provides functionality to warm up the strategy's factors
//! by replaying historical market data before starting live trading.
//!
//! Supports two data formats:
//! - `.jsonl` files: Current IPC collector format (can read while being written)
//! - `.gz` files: Compressed JSON format from IPC collector (previous days)
//! - `.npz` files: Numpy format from data converters

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use hftbacktest::{
    backtest::data::read_npz_file,
    depth::HashMapMarketDepth,
    prelude::{ApplySnapshot, L2MarketDepth, MarketDepth, BUY_EVENT, DEPTH_EVENT, DEPTH_SNAPSHOT_EVENT},
    types::Event,
};
use serde::Deserialize;
use tracing::{info, warn};

use crate::ObiMmStrategy;

/// Result of warmup attempt
#[derive(Debug)]
pub enum WarmupResult {
    /// Warmup succeeded with the given number of events processed
    Success {
        events_processed: usize,
        data_age_seconds: i64,
    },
    /// No suitable data files found
    NoDataFound,
    /// Data is too old (older than max_age_seconds)
    DataTooOld { age_seconds: i64 },
    /// Error loading data
    Error(String),
}

/// Configuration for warmup
#[derive(Debug, Clone)]
pub struct WarmupConfig {
    /// Directory containing npz files
    pub data_dir: PathBuf,
    /// Symbol name (lowercase, e.g., "btcusdt")
    pub symbol: String,
    /// Maximum age of data in seconds (default: 2 hours)
    pub max_age_seconds: i64,
    /// Minimum required data duration in seconds for warmup
    pub min_duration_seconds: i64,
}

impl WarmupConfig {
    pub fn new(data_dir: impl Into<PathBuf>, symbol: impl Into<String>) -> Self {
        Self {
            data_dir: data_dir.into(),
            symbol: symbol.into(),
            max_age_seconds: 2 * 3600, // 2 hours
            min_duration_seconds: 3600, // 1 hour minimum
        }
    }

    pub fn with_max_age(mut self, seconds: i64) -> Self {
        self.max_age_seconds = seconds;
        self
    }

    pub fn with_min_duration(mut self, seconds: i64) -> Self {
        self.min_duration_seconds = seconds;
        self
    }
}

// JSON structures for parsing IPC collector .gz files
#[derive(Deserialize)]
#[allow(non_snake_case, dead_code)]
struct DepthUpdateJson {
    e: String,           // "depthUpdate" or "depthSnapshot"
    E: i64,              // event time (ms)
    s: String,           // symbol
    b: Vec<[String; 2]>, // bids
    a: Vec<[String; 2]>, // asks
}

#[derive(Deserialize)]
#[allow(non_snake_case, dead_code)]
struct TradeJson {
    e: String,  // "trade"
    E: i64,     // event time (ms)
    s: String,  // symbol
    p: String,  // price
    q: String,  // quantity
    m: bool,    // is buyer maker
}

/// Find the most recent data file (npz or gz) for the given symbol
fn find_recent_data_file(config: &WarmupConfig) -> Option<PathBuf> {
    let dir = &config.data_dir;
    if !dir.exists() {
        warn!("Warmup data directory does not exist: {:?}", dir);
        return None;
    }

    let symbol_pattern = config.symbol.to_lowercase();

    // Check if directory name contains the symbol (e.g., data/btcusdt/)
    let dir_contains_symbol = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase().contains(&symbol_pattern))
        .unwrap_or(false);

    // Find all matching data files (npz, gz, or jsonl)
    // Prefer .jsonl files as they are the current file being written and can be read in real-time
    let mut files: Vec<(PathBuf, SystemTime)> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let name_matches = name.contains(&symbol_pattern) || dir_contains_symbol;
            let ext_matches = name.ends_with(".npz") || name.ends_with(".gz") || name.ends_with(".jsonl");
            name_matches && ext_matches
        })
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            let modified = metadata.modified().ok()?;
            Some((entry.path(), modified))
        })
        .collect();

    // Sort by modification time (most recent first)
    files.sort_by(|a, b| b.1.cmp(&a.1));

    if let Some((path, _)) = files.first() {
        info!(
            "Found {} data files, using most recent: {:?}",
            files.len(),
            path
        );
    }

    files.first().map(|(path, _)| path.clone())
}

/// Find the most recent npz file for the given symbol (legacy function)
fn find_recent_npz_file(config: &WarmupConfig) -> Option<PathBuf> {
    find_recent_data_file(config).filter(|p| p.extension().map(|e| e == "npz").unwrap_or(false))
}

/// Check if the npz data is fresh enough
fn check_data_freshness(events: &[Event], max_age_seconds: i64) -> Result<i64, i64> {
    if events.is_empty() {
        return Err(i64::MAX);
    }

    // Get the last event's exchange timestamp
    let last_event_ts = events.last().unwrap().exch_ts;
    let last_event_secs = last_event_ts / 1_000_000_000;

    // Get current time
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let age_seconds = now - last_event_secs;

    if age_seconds > max_age_seconds {
        Err(age_seconds)
    } else {
        Ok(age_seconds)
    }
}

/// Get the data duration in seconds
fn get_data_duration(events: &[Event]) -> i64 {
    if events.len() < 2 {
        return 0;
    }

    let first_ts = events.first().unwrap().exch_ts;
    let last_ts = events.last().unwrap().exch_ts;

    (last_ts - first_ts) / 1_000_000_000
}

/// Load events from npz file, filtering to only include recent data
fn load_warmup_events_npz(
    path: &Path,
    min_duration_seconds: i64,
) -> Result<Vec<Event>, String> {
    info!("Loading warmup data from npz: {:?}", path);

    let data = read_npz_file::<Event>(path.to_str().unwrap(), "data")
        .map_err(|e| format!("Failed to read npz file: {}", e))?;

    if data.len() == 0 {
        return Err("Empty npz file".to_string());
    }

    // Get the time range
    let first_ts = data[0].exch_ts;
    let last_ts = data[data.len() - 1].exch_ts;
    let total_duration = (last_ts - first_ts) / 1_000_000_000;

    info!(
        "Loaded {} events, duration: {} seconds",
        data.len(),
        total_duration
    );

    // Only keep the last `min_duration_seconds` of data for warmup
    let warmup_start_ts = last_ts - (min_duration_seconds * 1_000_000_000);

    let events: Vec<Event> = (0..data.len())
        .filter(|&i| data[i].exch_ts >= warmup_start_ts)
        .map(|i| data[i].clone())
        .collect();

    info!(
        "Filtered to {} events for warmup ({} seconds of data)",
        events.len(),
        min_duration_seconds
    );

    Ok(events)
}

/// Load events from IPC collector .gz file (JSON format)
fn load_warmup_events_gz(
    path: &Path,
    min_duration_seconds: i64,
) -> Result<Vec<Event>, String> {
    info!("Loading warmup data from gz: {:?}", path);

    let file = std::fs::File::open(path).map_err(|e| format!("Failed to open gz file: {}", e))?;
    let decoder = GzDecoder::new(file);
    let reader = BufReader::new(decoder);

    let mut events = Vec::new();
    let mut line_count = 0;
    let mut parse_errors = 0;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                // Handle truncated/corrupted gz files gracefully
                warn!("Error reading line {} (may be truncated gz file): {}", line_count + 1, e);
                break; // Stop reading on error - file may be incomplete
            }
        };
        line_count += 1;

        // Log progress every 10000 lines
        if line_count % 10000 == 0 {
            info!("Processing line {}, events so far: {}", line_count, events.len());
        }

        // Parse format: "local_timestamp json_data"
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() != 2 {
            continue;
        }

        let local_ts: i64 = match parts[0].parse() {
            Ok(ts) => ts,
            Err(_) => continue,
        };

        let json_str = parts[1];

        // Try to parse as depth update first
        if let Ok(depth) = serde_json::from_str::<DepthUpdateJson>(json_str) {
            let exch_ts = depth.E * 1_000_000; // ms to ns
            let is_snapshot = depth.e == "depthSnapshot";
            let ev_type = if is_snapshot {
                DEPTH_SNAPSHOT_EVENT
            } else {
                DEPTH_EVENT
            };

            // Add bid events
            for bid in &depth.b {
                let px: f64 = bid[0].parse().unwrap_or(0.0);
                let qty: f64 = bid[1].parse().unwrap_or(0.0);
                if px > 0.0 {
                    events.push(Event {
                        ev: ev_type | BUY_EVENT,
                        exch_ts,
                        local_ts,
                        px,
                        qty,
                        order_id: 0,
                        ival: 0,
                        fval: 0.0,
                    });
                }
            }

            // Add ask events
            for ask in &depth.a {
                let px: f64 = ask[0].parse().unwrap_or(0.0);
                let qty: f64 = ask[1].parse().unwrap_or(0.0);
                if px > 0.0 {
                    events.push(Event {
                        ev: ev_type, // No BUY_EVENT flag = sell side
                        exch_ts,
                        local_ts,
                        px,
                        qty,
                        order_id: 0,
                        ival: 0,
                        fval: 0.0,
                    });
                }
            }
        } else if let Ok(_trade) = serde_json::from_str::<TradeJson>(json_str) {
            // Skip trade events for warmup - we only need depth for OBI
            continue;
        } else {
            parse_errors += 1;
        }
    }

    if events.is_empty() {
        return Err(format!(
            "No events parsed from gz file (lines: {}, errors: {})",
            line_count, parse_errors
        ));
    }

    // Sort by exchange timestamp
    events.sort_by_key(|e| e.exch_ts);

    // Get the time range
    let first_ts = events.first().unwrap().exch_ts;
    let last_ts = events.last().unwrap().exch_ts;
    let total_duration = (last_ts - first_ts) / 1_000_000_000;

    info!(
        "Loaded {} events from {} lines, duration: {} seconds",
        events.len(),
        line_count,
        total_duration
    );

    // Only keep the last `min_duration_seconds` of data for warmup
    let warmup_start_ts = last_ts - (min_duration_seconds * 1_000_000_000);
    events.retain(|e| e.exch_ts >= warmup_start_ts);

    info!(
        "Filtered to {} events for warmup ({} seconds of data)",
        events.len(),
        min_duration_seconds
    );

    Ok(events)
}

/// Load events from IPC collector .jsonl file (uncompressed JSON format)
/// This function can read files that are currently being written to.
fn load_warmup_events_jsonl(
    path: &Path,
    min_duration_seconds: i64,
) -> Result<Vec<Event>, String> {
    info!("Loading warmup data from jsonl: {:?}", path);

    let file = std::fs::File::open(path).map_err(|e| format!("Failed to open jsonl file: {}", e))?;
    let reader = BufReader::new(file);

    let mut events = Vec::new();
    let mut line_count = 0;
    let mut parse_errors = 0;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                // For jsonl files being written, we might hit EOF - this is expected
                warn!("Error reading line {} (file may still be written): {}", line_count + 1, e);
                break;
            }
        };
        line_count += 1;

        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }

        // Log progress every 10000 lines
        if line_count % 10000 == 0 {
            info!("Processing line {}, events so far: {}", line_count, events.len());
        }

        // Parse format: "local_timestamp json_data"
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() != 2 {
            continue;
        }

        let local_ts: i64 = match parts[0].parse() {
            Ok(ts) => ts,
            Err(_) => continue,
        };

        let json_str = parts[1];

        // Try to parse as depth update first
        if let Ok(depth) = serde_json::from_str::<DepthUpdateJson>(json_str) {
            let exch_ts = depth.E * 1_000_000; // ms to ns
            let is_snapshot = depth.e == "depthSnapshot";
            let ev_type = if is_snapshot {
                DEPTH_SNAPSHOT_EVENT
            } else {
                DEPTH_EVENT
            };

            // Add bid events
            for bid in &depth.b {
                let px: f64 = bid[0].parse().unwrap_or(0.0);
                let qty: f64 = bid[1].parse().unwrap_or(0.0);
                if px > 0.0 {
                    events.push(Event {
                        ev: ev_type | BUY_EVENT,
                        exch_ts,
                        local_ts,
                        px,
                        qty,
                        order_id: 0,
                        ival: 0,
                        fval: 0.0,
                    });
                }
            }

            // Add ask events
            for ask in &depth.a {
                let px: f64 = ask[0].parse().unwrap_or(0.0);
                let qty: f64 = ask[1].parse().unwrap_or(0.0);
                if px > 0.0 {
                    events.push(Event {
                        ev: ev_type, // No BUY_EVENT flag = sell side
                        exch_ts,
                        local_ts,
                        px,
                        qty,
                        order_id: 0,
                        ival: 0,
                        fval: 0.0,
                    });
                }
            }
        } else if let Ok(_trade) = serde_json::from_str::<TradeJson>(json_str) {
            // Skip trade events for warmup - we only need depth for OBI
            continue;
        } else {
            parse_errors += 1;
        }
    }

    if events.is_empty() {
        return Err(format!(
            "No events parsed from jsonl file (lines: {}, errors: {})",
            line_count, parse_errors
        ));
    }

    // Sort by exchange timestamp
    events.sort_by_key(|e| e.exch_ts);

    // Get the time range
    let first_ts = events.first().unwrap().exch_ts;
    let last_ts = events.last().unwrap().exch_ts;
    let total_duration = (last_ts - first_ts) / 1_000_000_000;

    info!(
        "Loaded {} events from {} lines, duration: {} seconds",
        events.len(),
        line_count,
        total_duration
    );

    // Only keep the last `min_duration_seconds` of data for warmup
    let warmup_start_ts = last_ts - (min_duration_seconds * 1_000_000_000);
    events.retain(|e| e.exch_ts >= warmup_start_ts);

    info!(
        "Filtered to {} events for warmup ({} seconds of data)",
        events.len(),
        min_duration_seconds
    );

    Ok(events)
}

/// Load warmup events from npz, gz, or jsonl file
fn load_warmup_events(path: &Path, min_duration_seconds: i64) -> Result<Vec<Event>, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    match ext {
        "npz" => load_warmup_events_npz(path, min_duration_seconds),
        "gz" => load_warmup_events_gz(path, min_duration_seconds),
        "jsonl" => load_warmup_events_jsonl(path, min_duration_seconds),
        _ => Err(format!("Unsupported file format: {}", ext)),
    }
}

impl ObiMmStrategy {
    /// Attempt to warm up factors from historical data.
    ///
    /// This method loads historical data from data files and replays it
    /// to initialize the factor engines before live trading starts.
    ///
    /// Supports:
    /// - `.jsonl` files: Current IPC collector format (can read while being written)
    /// - `.gz` files: Compressed IPC collector JSON format
    /// - `.npz` files: Converted numpy format
    ///
    /// # Arguments
    ///
    /// * `config` - Warmup configuration
    ///
    /// # Returns
    ///
    /// `WarmupResult` indicating success or failure reason.
    pub fn warmup_from_npz(&mut self, config: &WarmupConfig) -> WarmupResult {
        // Find the most recent data file (npz or gz)
        let data_path = match find_recent_data_file(config) {
            Some(path) => path,
            None => {
                warn!("No data files found for symbol {}", config.symbol);
                return WarmupResult::NoDataFound;
            }
        };

        info!("Found warmup file: {:?}", data_path);

        // Load events
        let events = match load_warmup_events(&data_path, config.min_duration_seconds) {
            Ok(events) => events,
            Err(e) => {
                warn!("Failed to load warmup events: {}", e);
                return WarmupResult::Error(e);
            }
        };

        if events.is_empty() {
            return WarmupResult::NoDataFound;
        }

        // Check data freshness
        let data_age = match check_data_freshness(&events, config.max_age_seconds) {
            Ok(age) => age,
            Err(age) => {
                warn!(
                    "Warmup data is too old: {} seconds (max: {})",
                    age, config.max_age_seconds
                );
                return WarmupResult::DataTooOld { age_seconds: age };
            }
        };

        info!(
            "Data freshness check passed. Age: {} seconds",
            data_age
        );

        // Create a temporary depth for warmup
        let tick_size = self.tick_size;
        let lot_size = 0.001; // Default lot size
        let mut depth = HashMapMarketDepth::new(tick_size, lot_size);

        // Replay events to warm up factors
        let events_processed = self.replay_events_for_warmup(&events, &mut depth);

        info!(
            "Warmup complete. Processed {} events. is_ready={}",
            events_processed,
            self.is_ready()
        );

        WarmupResult::Success {
            events_processed,
            data_age_seconds: data_age,
        }
    }

    /// Replay events to warm up factors without generating trading signals.
    fn replay_events_for_warmup<MD>(&mut self, events: &[Event], depth: &mut MD) -> usize
    where
        MD: MarketDepth + L2MarketDepth + ApplySnapshot,
    {
        let mut events_processed = 0;
        let mut last_mid_price = 0.0;
        let mut last_update_period: i64 = -1;
        let mut last_change_period: i64 = -1;

        for event in events {
            // Apply depth updates
            let ev_type = event.ev & 0xFF;
            let is_buy = (event.ev & BUY_EVENT) != 0;

            if ev_type == DEPTH_EVENT || ev_type == DEPTH_SNAPSHOT_EVENT {
                if is_buy {
                    depth.update_bid_depth(event.px, event.qty, event.exch_ts);
                } else {
                    depth.update_ask_depth(event.px, event.qty, event.exch_ts);
                }
            }

            // Get current market state
            let best_bid = depth.best_bid();
            let best_ask = depth.best_ask();

            if best_bid <= 0.0 || best_ask <= 0.0 {
                continue;
            }

            let mid_price = (best_bid + best_ask) / 2.0;
            let best_bid_tick = depth.best_bid_tick();
            let best_ask_tick = depth.best_ask_tick();

            // Check update interval
            let current_period = event.exch_ts / self.params.update_interval_ns;
            if current_period <= last_update_period {
                continue;
            }
            last_update_period = current_period;

            // Update volatility at appropriate intervals
            let change_period = current_period / 6;
            if change_period > last_change_period {
                if last_mid_price > 0.0 {
                    self.minute_change = mid_price / last_mid_price - 1.0;
                }

                let volatility = self.volatility_factor.update(event.exch_ts, mid_price);
                let vol = if volatility == 0.0 {
                    self.stop_loss.volatility_mean()
                } else {
                    volatility
                };
                self.stop_loss.check(vol, self.minute_change);

                last_mid_price = mid_price;
                last_change_period = change_period;
            }

            // Update OBI factor
            self.obi_factor.update_with_accessor(
                best_bid_tick,
                best_ask_tick,
                mid_price,
                |tick| depth.bid_qty_at_tick(tick),
                |tick| depth.ask_qty_at_tick(tick),
            );

            events_processed += 1;
        }

        events_processed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_warmup_config() {
        let config = WarmupConfig::new("/data", "btcusdt")
            .with_max_age(3600)
            .with_min_duration(1800);

        assert_eq!(config.max_age_seconds, 3600);
        assert_eq!(config.min_duration_seconds, 1800);
        assert_eq!(config.symbol, "btcusdt");
    }
}
