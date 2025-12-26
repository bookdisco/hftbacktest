//! Benchmark: Compare original vs fast Tardis converter
//!
//! Usage:
//!   cargo run --release --example tardis_benchmark -- trades.csv.gz depth.csv.gz

use std::env;
use std::time::Instant;

use hftbacktest::data::{
    convert_tardis, convert_tardis_fast, TardisConvertConfig, SnapshotMode,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        println!("Usage: {} <trades.csv.gz> <depth.csv.gz>", args[0]);
        return Ok(());
    }

    let input_files: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    let config = TardisConvertConfig {
        buffer_size: 100_000_000,
        ss_buffer_size: 1_000_000,
        base_latency: 0,
        snapshot_mode: SnapshotMode::IgnoreSod,
    };

    println!("=== Tardis Converter Benchmark ===\n");
    println!("Input files: {:?}\n", input_files);

    // Benchmark original version
    println!("--- Original Version ---");
    let start = Instant::now();
    let events_orig = convert_tardis(&input_files, &config)?;
    let elapsed_orig = start.elapsed();
    let rate_orig = events_orig.len() as f64 / elapsed_orig.as_secs_f64();
    println!(
        "  Events: {}\n  Time: {:.3}s\n  Rate: {:.0} events/sec\n",
        events_orig.len(),
        elapsed_orig.as_secs_f64(),
        rate_orig
    );

    // Benchmark fast version
    println!("--- Fast Version ---");
    let start = Instant::now();
    let events_fast = convert_tardis_fast(&input_files, &config)?;
    let elapsed_fast = start.elapsed();
    let rate_fast = events_fast.len() as f64 / elapsed_fast.as_secs_f64();
    println!(
        "  Events: {}\n  Time: {:.3}s\n  Rate: {:.0} events/sec\n",
        events_fast.len(),
        elapsed_fast.as_secs_f64(),
        rate_fast
    );

    // Verify consistency
    println!("--- Verification ---");
    if events_orig.len() != events_fast.len() {
        println!("  ERROR: Event count mismatch! {} vs {}", events_orig.len(), events_fast.len());
    } else {
        let mut mismatches = 0;
        for (i, (orig, fast)) in events_orig.iter().zip(events_fast.iter()).enumerate() {
            if orig.ev != fast.ev || orig.exch_ts != fast.exch_ts || orig.local_ts != fast.local_ts
                || (orig.px - fast.px).abs() > 1e-9 || (orig.qty - fast.qty).abs() > 1e-9
            {
                mismatches += 1;
                if mismatches <= 5 {
                    println!("  Mismatch at index {}: {:?} vs {:?}", i, orig, fast);
                }
            }
        }
        if mismatches == 0 {
            println!("  ✓ All {} events match!", events_orig.len());
        } else {
            println!("  ✗ {} mismatches found", mismatches);
        }
    }

    // Summary
    println!("\n--- Summary ---");
    let speedup = elapsed_orig.as_secs_f64() / elapsed_fast.as_secs_f64();
    println!("  Speedup: {:.2}x", speedup);
    println!(
        "  Time saved: {:.3}s",
        elapsed_orig.as_secs_f64() - elapsed_fast.as_secs_f64()
    );

    Ok(())
}
