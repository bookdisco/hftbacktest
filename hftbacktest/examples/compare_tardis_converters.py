#!/usr/bin/env python3
"""
Compare Rust and Python Tardis converters for consistency and performance.

Usage:
    python compare_tardis_converters.py [--generate-sample]
"""

import sys
import time
import numpy as np
from pathlib import Path

# Add py-hftbacktest to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent / "py-hftbacktest"))

from hftbacktest.data.utils.tardis import convert


def generate_sample_data():
    """Generate sample Tardis CSV files for testing."""
    trades_path = "/tmp/sample_trades.csv"
    depth_path = "/tmp/sample_depth.csv"

    base_ts = 1700000000000000  # microseconds

    # Generate trades
    with open(trades_path, 'w') as f:
        f.write("exchange,symbol,timestamp,local_timestamp,id,side,price,amount\n")
        for i in range(10000):
            ts = base_ts + i * 100
            local_ts = ts + 50 + (i % 100)
            side = "buy" if i % 2 == 0 else "sell"
            price = 50000.0 + (i % 100) * 0.5
            amount = 0.001 + (i % 10) * 0.001
            f.write(f"binance-futures,BTCUSDT,{ts},{local_ts},trade{i},{side},{price:.2f},{amount:.6f}\n")

    # Generate depth
    with open(depth_path, 'w') as f:
        f.write("exchange,symbol,timestamp,local_timestamp,is_snapshot,side,price,amount\n")

        # Snapshot
        snapshot_ts = base_ts
        snapshot_local_ts = snapshot_ts + 50
        for i in range(100):
            bid_price = 50000.0 - i * 0.5
            ask_price = 50000.5 + i * 0.5
            amount = 1.0 + i * 0.1
            f.write(f"binance-futures,BTCUSDT,{snapshot_ts},{snapshot_local_ts},true,bid,{bid_price:.2f},{amount:.3f}\n")
            f.write(f"binance-futures,BTCUSDT,{snapshot_ts},{snapshot_local_ts},true,ask,{ask_price:.2f},{amount:.3f}\n")

        # Incremental updates
        for i in range(50000):
            ts = base_ts + 1000 + i * 50
            local_ts = ts + 30 + (i % 50)
            side = "bid" if i % 2 == 0 else "ask"
            price = (50000.0 - (i % 50) * 0.5) if side == "bid" else (50000.5 + (i % 50) * 0.5)
            amount = (i % 10) * 0.5
            f.write(f"binance-futures,BTCUSDT,{ts},{local_ts},false,{side},{price:.2f},{amount:.3f}\n")

    return trades_path, depth_path


def count_event_types(data):
    """Count event types in data."""
    trade_count = 0
    depth_count = 0
    snapshot_count = 0
    clear_count = 0

    for event in data:
        ev_type = event['ev'] & 0xFF
        if ev_type == 2:
            trade_count += 1
        elif ev_type == 1:
            depth_count += 1
        elif ev_type == 4:
            snapshot_count += 1
        elif ev_type == 3:
            clear_count += 1

    return {
        'trades': trade_count,
        'depth': depth_count,
        'snapshot': snapshot_count,
        'clear': clear_count,
    }


def main():
    print("Generating sample data...")
    trades_path, depth_path = generate_sample_data()

    print(f"\nPython converter:")
    print("-" * 40)

    # Warmup run to trigger JIT compilation
    print("Warmup run (JIT compilation)...")
    warmup_start = time.time()
    _ = convert([trades_path, depth_path])
    warmup_elapsed = time.time() - warmup_start
    print(f"Warmup complete: {warmup_elapsed:.3f}s (includes JIT compilation)")

    # Actual benchmark run
    print("\nBenchmark run (after JIT)...")
    start = time.time()
    # Note: trades file should be listed first per the Python documentation
    py_data = convert([trades_path, depth_path])
    py_elapsed = time.time() - start

    print(f"Conversion complete: {len(py_data)} events in {py_elapsed:.3f}s")
    print(f"Speed: {len(py_data) / py_elapsed:.0f} events/sec")

    py_counts = count_event_types(py_data)
    print(f"\nEvent breakdown:")
    print(f"  Trades: {py_counts['trades']}")
    print(f"  Depth updates: {py_counts['depth']}")
    print(f"  Snapshot entries: {py_counts['snapshot']}")
    print(f"  Clear events: {py_counts['clear']}")

    # Save Python output
    py_output = "/tmp/sample_output_python.npz"
    np.savez_compressed(py_output, data=py_data)
    print(f"\nSaved to: {py_output}")

    # Load Rust output for comparison
    rust_output = "/tmp/sample_output.npz"
    try:
        rust_data = np.load(rust_output)['data']
        print(f"\nRust output loaded: {len(rust_data)} events")

        rust_counts = count_event_types(rust_data)
        print(f"  Trades: {rust_counts['trades']}")
        print(f"  Depth updates: {rust_counts['depth']}")
        print(f"  Snapshot entries: {rust_counts['snapshot']}")
        print(f"  Clear events: {rust_counts['clear']}")

        # Compare
        print(f"\n{'=' * 40}")
        print("Comparison:")
        print(f"{'=' * 40}")

        print(f"Total events: Python={len(py_data)}, Rust={len(rust_data)}")

        if len(py_data) == len(rust_data):
            print("✓ Event counts match")

            # Compare field by field
            all_match = True
            for field in ['ev', 'exch_ts', 'local_ts', 'px', 'qty']:
                py_vals = py_data[field]
                rust_vals = rust_data[field]

                if field in ['px', 'qty']:
                    # Float comparison with tolerance
                    matches = np.allclose(py_vals, rust_vals, rtol=1e-10, atol=1e-10, equal_nan=True)
                else:
                    matches = np.array_equal(py_vals, rust_vals)

                if matches:
                    print(f"✓ {field} values match")
                else:
                    print(f"✗ {field} values differ")
                    # Find first difference
                    diff_idx = np.where(py_vals != rust_vals)[0]
                    if len(diff_idx) > 0:
                        idx = diff_idx[0]
                        print(f"  First difference at index {idx}:")
                        print(f"    Python: {py_vals[idx]}")
                        print(f"    Rust:   {rust_vals[idx]}")
                    all_match = False

            if all_match:
                print("\n✓ All values match! Converters are consistent.")
            else:
                print("\n✗ Some values differ. Please investigate.")
        else:
            print(f"✗ Event counts differ by {abs(len(py_data) - len(rust_data))}")

    except FileNotFoundError:
        print(f"\nRust output not found at {rust_output}")
        print("Run: cargo run --release --example tardis_convert -- --generate-sample")


if __name__ == "__main__":
    main()
