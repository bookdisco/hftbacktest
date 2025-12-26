//! Optimized Tardis.dev data converter.
//!
//! This module provides an optimized version of the Tardis converter with:
//! - Zero-copy parsing (no intermediate String allocations)
//! - Direct Event construction (no intermediate record structs)
//! - Larger I/O buffers
//! - Inline parsing functions

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use flate2::read::GzDecoder;

use crate::types::{
    Event, BUY_EVENT, SELL_EVENT, DEPTH_EVENT, DEPTH_CLEAR_EVENT, DEPTH_SNAPSHOT_EVENT,
    TRADE_EVENT,
};

use super::validation::{
    argsort_by_exch_ts, argsort_by_local_ts, correct_event_order, correct_local_timestamp,
    validate_event_order,
};
use super::tardis::{SnapshotMode, TardisConvertConfig};

/// Buffer size for file I/O (8MB)
const IO_BUFFER_SIZE: usize = 8 * 1024 * 1024;

/// Detected file type based on header
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileType {
    Trades,
    Depth,
}

/// Opens a file with optimized buffer, handling .gz compression automatically
fn open_file_fast(path: &Path) -> std::io::Result<Box<dyn BufRead>> {
    let file = File::open(path)?;
    if path.extension().map_or(false, |ext| ext == "gz") {
        Ok(Box::new(BufReader::with_capacity(IO_BUFFER_SIZE, GzDecoder::new(file))))
    } else {
        Ok(Box::new(BufReader::with_capacity(IO_BUFFER_SIZE, file)))
    }
}

/// Detects file type from header line
#[inline]
fn detect_file_type_fast(header: &str) -> Option<FileType> {
    // Quick check based on 5th field
    if header.contains(",id,side,") {
        Some(FileType::Trades)
    } else if header.contains(",is_snapshot,side,") {
        Some(FileType::Depth)
    } else {
        None
    }
}

/// Fast integer parsing without error handling overhead
#[inline(always)]
fn parse_i64_fast(s: &str) -> i64 {
    let mut result: i64 = 0;
    for b in s.bytes() {
        if b >= b'0' && b <= b'9' {
            result = result * 10 + (b - b'0') as i64;
        }
    }
    result
}

/// Fast float parsing (simplified, assumes valid input)
#[inline(always)]
fn parse_f64_fast(s: &str) -> f64 {
    // Use standard parser for floats (fast-float is used internally by Rust)
    s.parse().unwrap_or(0.0)
}

/// Parses a trade line directly into an Event
#[inline]
fn parse_trade_to_event(line: &str) -> Option<Event> {
    // Format: exchange,symbol,timestamp,local_timestamp,id,side,price,amount
    let mut iter = line.split(',');

    // Skip exchange (0), symbol (1)
    iter.next()?;
    iter.next()?;

    // Parse timestamp (2), local_timestamp (3)
    let timestamp = parse_i64_fast(iter.next()?);
    let local_timestamp = parse_i64_fast(iter.next()?);

    // Skip id (4)
    iter.next()?;

    // Parse side (5)
    let side_str = iter.next()?;
    let side_flag = match side_str.as_bytes().first()? {
        b'b' => BUY_EVENT,  // "buy"
        b's' => SELL_EVENT, // "sell"
        _ => 0,
    };

    // Parse price (6), amount (7)
    let price = parse_f64_fast(iter.next()?);
    let amount = parse_f64_fast(iter.next()?);

    Some(Event {
        ev: TRADE_EVENT | side_flag,
        exch_ts: timestamp * 1000, // microseconds to nanoseconds
        local_ts: local_timestamp * 1000,
        px: price,
        qty: amount,
        order_id: 0,
        ival: 0,
        fval: 0.0,
    })
}

/// Parses a depth line, returning parsed fields
#[inline]
fn parse_depth_line(line: &str) -> Option<(i64, i64, bool, bool, f64, f64)> {
    // Format: exchange,symbol,timestamp,local_timestamp,is_snapshot,side,price,amount
    let mut iter = line.split(',');

    // Skip exchange (0), symbol (1)
    iter.next()?;
    iter.next()?;

    // Parse timestamp (2), local_timestamp (3)
    let timestamp = parse_i64_fast(iter.next()?);
    let local_timestamp = parse_i64_fast(iter.next()?);

    // Parse is_snapshot (4)
    let is_snapshot_str = iter.next()?;
    let is_snapshot = is_snapshot_str == "true";

    // Parse side (5)
    let side_str = iter.next()?;
    let is_bid = matches!(side_str.as_bytes().first(), Some(b'b')); // "bid" or "buy"

    // Parse price (6), amount (7)
    let price = parse_f64_fast(iter.next()?);
    let amount = parse_f64_fast(iter.next()?);

    Some((timestamp, local_timestamp, is_snapshot, is_bid, price, amount))
}

/// Optimized depth converter state
struct DepthConverterFast {
    ss_bid: Vec<Event>,
    ss_ask: Vec<Event>,
    is_sod_snapshot: bool,
    is_snapshot: bool,
    snapshot_mode: SnapshotMode,
}

impl DepthConverterFast {
    fn new(ss_buffer_size: usize, snapshot_mode: SnapshotMode) -> Self {
        Self {
            ss_bid: Vec::with_capacity(ss_buffer_size),
            ss_ask: Vec::with_capacity(ss_buffer_size),
            is_sod_snapshot: true,
            is_snapshot: false,
            snapshot_mode,
        }
    }

    /// Processes a parsed depth record
    #[inline]
    fn process(
        &mut self,
        timestamp: i64,
        local_timestamp: i64,
        is_snapshot: bool,
        is_bid: bool,
        price: f64,
        amount: f64,
        output: &mut Vec<Event>,
    ) {
        let side_flag = if is_bid { BUY_EVENT } else { SELL_EVENT };
        let exch_ts = timestamp * 1000;
        let local_ts = local_timestamp * 1000;

        if is_snapshot {
            // Check if we should skip this snapshot
            if self.snapshot_mode == SnapshotMode::Ignore {
                return;
            }
            if self.snapshot_mode == SnapshotMode::IgnoreSod && self.is_sod_snapshot {
                return;
            }

            // Start collecting snapshot
            if !self.is_snapshot {
                self.is_snapshot = true;
                self.ss_bid.clear();
                self.ss_ask.clear();
            }

            let event = Event {
                ev: DEPTH_SNAPSHOT_EVENT | side_flag,
                exch_ts,
                local_ts,
                px: price,
                qty: amount,
                order_id: 0,
                ival: 0,
                fval: 0.0,
            };

            if is_bid {
                self.ss_bid.push(event);
            } else {
                self.ss_ask.push(event);
            }
        } else {
            // Regular depth update
            self.is_sod_snapshot = false;

            if self.is_snapshot {
                // End of snapshot - flush accumulated snapshot events
                self.is_snapshot = false;
                self.flush_snapshot(output);
            }

            // Add regular depth event
            output.push(Event {
                ev: DEPTH_EVENT | side_flag,
                exch_ts,
                local_ts,
                px: price,
                qty: amount,
                order_id: 0,
                ival: 0,
                fval: 0.0,
            });
        }
    }

    /// Flushes accumulated snapshot events
    fn flush_snapshot(&mut self, output: &mut Vec<Event>) {
        // Flush bid snapshot
        if !self.ss_bid.is_empty() {
            let min_px = self.ss_bid.iter().map(|e| e.px).fold(f64::INFINITY, f64::min);

            output.push(Event {
                ev: DEPTH_CLEAR_EVENT | BUY_EVENT,
                exch_ts: self.ss_bid[0].exch_ts,
                local_ts: self.ss_bid[0].local_ts,
                px: min_px,
                qty: 0.0,
                order_id: 0,
                ival: 0,
                fval: 0.0,
            });

            output.extend(self.ss_bid.drain(..));
        }

        // Flush ask snapshot
        if !self.ss_ask.is_empty() {
            let max_px = self.ss_ask.iter().map(|e| e.px).fold(f64::NEG_INFINITY, f64::max);

            output.push(Event {
                ev: DEPTH_CLEAR_EVENT | SELL_EVENT,
                exch_ts: self.ss_ask[0].exch_ts,
                local_ts: self.ss_ask[0].local_ts,
                px: max_px,
                qty: 0.0,
                order_id: 0,
                ival: 0,
                fval: 0.0,
            });

            output.extend(self.ss_ask.drain(..));
        }
    }
}

/// Optimized Tardis converter
pub fn convert_tardis_fast<P: AsRef<Path>>(
    input_files: &[P],
    config: &TardisConvertConfig,
) -> Result<Vec<Event>, String> {
    let mut events: Vec<Event> = Vec::with_capacity(config.buffer_size);
    let mut depth_converter = DepthConverterFast::new(config.ss_buffer_size, config.snapshot_mode);

    for file_path in input_files {
        let path = file_path.as_ref();
        println!("Reading {:?}", path);

        let mut reader = open_file_fast(path).map_err(|e| format!("Failed to open file: {}", e))?;

        // Read header
        let mut header = String::new();
        reader.read_line(&mut header).map_err(|e| format!("Failed to read header: {}", e))?;

        let file_type = detect_file_type_fast(&header)
            .ok_or_else(|| format!("Unknown file type for {:?}", path))?;

        // Reuse line buffer
        let mut line = String::with_capacity(256);

        match file_type {
            FileType::Trades => {
                while reader.read_line(&mut line).map_err(|e| format!("Read error: {}", e))? > 0 {
                    if let Some(event) = parse_trade_to_event(line.trim_end()) {
                        events.push(event);
                    }
                    line.clear();
                }
            }
            FileType::Depth => {
                while reader.read_line(&mut line).map_err(|e| format!("Read error: {}", e))? > 0 {
                    if let Some((ts, local_ts, is_snap, is_bid, px, qty)) = parse_depth_line(line.trim_end()) {
                        depth_converter.process(ts, local_ts, is_snap, is_bid, px, qty, &mut events);
                    }
                    line.clear();
                }
            }
        }
    }

    if events.is_empty() {
        return Ok(events);
    }

    println!("Correcting the latency");
    let min_latency = correct_local_timestamp(&mut events, config.base_latency);
    if min_latency < 0 {
        println!("local_timestamp is ahead of exch_timestamp by {}", -min_latency);
    }

    println!("Correcting the event order");
    let sorted_exch_indices = argsort_by_exch_ts(&events);
    let sorted_local_indices = argsort_by_local_ts(&events);
    let corrected = correct_event_order(&events, &sorted_exch_indices, &sorted_local_indices);

    println!("Validating event order");
    validate_event_order(&corrected)?;

    println!("Conversion complete: {} events", corrected.len());
    Ok(corrected)
}

/// Convenience function with default configuration
pub fn convert_fast<P: AsRef<Path>>(input_files: &[P]) -> Result<Vec<Event>, String> {
    convert_tardis_fast(input_files, &TardisConvertConfig::default())
}

/// Converts Tardis files and saves the result to an NPZ file (fast version).
#[cfg(feature = "backtest")]
pub fn convert_and_save_fast<P: AsRef<Path>>(
    input_files: &[P],
    output_path: P,
    config: &TardisConvertConfig,
) -> Result<Vec<Event>, String> {
    let events = convert_tardis_fast(input_files, config)?;
    super::tardis::write_npz_file(&events, output_path)?;
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_i64_fast() {
        assert_eq!(parse_i64_fast("12345"), 12345);
        assert_eq!(parse_i64_fast("1731715200006000"), 1731715200006000);
    }

    #[test]
    fn test_parse_trade_to_event() {
        let line = "binance-futures,BTCUSDT,1234567890123,1234567890456,trade123,buy,50000.5,0.001";
        let event = parse_trade_to_event(line).unwrap();
        assert_eq!(event.ev, TRADE_EVENT | BUY_EVENT);
        assert_eq!(event.exch_ts, 1234567890123 * 1000);
        assert_eq!(event.local_ts, 1234567890456 * 1000);
        assert_eq!(event.px, 50000.5);
        assert_eq!(event.qty, 0.001);
    }

    #[test]
    fn test_parse_depth_line() {
        let line = "binance-futures,BTCUSDT,1234567890123,1234567890456,false,bid,50000.0,1.5";
        let result = parse_depth_line(line).unwrap();
        assert_eq!(result.0, 1234567890123); // timestamp
        assert_eq!(result.1, 1234567890456); // local_timestamp
        assert!(!result.2); // is_snapshot
        assert!(result.3);  // is_bid
        assert_eq!(result.4, 50000.0); // price
        assert_eq!(result.5, 1.5); // amount
    }
}
