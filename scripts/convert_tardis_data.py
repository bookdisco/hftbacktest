#!/usr/bin/env python3
"""
Convert Tardis data to HftBacktest npz format.

Usage:
    python convert_tardis_data.py
"""

import os
from pathlib import Path
from hftbacktest.data.utils.tardis import convert

# Data paths
L2_DIR = Path("/mnt/data/samba/users/Data/tardis/datasets/binance-futures/incremental_book_L2/BTCUSDT")
TRADES_DIR = Path("/mnt/data/samba/users/Data/tardis/datasets/binance-futures/trades/BTCUSDT")
OUTPUT_DIR = Path("/home/b0qi/bookdisco/hftbacktest/test_data/btcusdt")

# Date range: 2024-11-13 to 2024-11-19 (1 week)
DATES = [
    "2024-11-13",
    "2024-11-14",
    "2024-11-15",
    "2024-11-16",
    "2024-11-17",
    "2024-11-18",
    "2024-11-19",
]


def main():
    # Create output directory
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

    for date in DATES:
        trades_file = TRADES_DIR / f"{date}.csv.gz"
        l2_file = L2_DIR / f"{date}.csv.gz"
        output_file = OUTPUT_DIR / f"{date}.npz"

        # Check if files exist
        if not trades_file.exists():
            print(f"Trades file not found: {trades_file}")
            continue
        if not l2_file.exists():
            print(f"L2 file not found: {l2_file}")
            continue

        # Skip if already converted
        if output_file.exists():
            print(f"Already converted: {output_file}")
            continue

        print(f"\n{'='*60}")
        print(f"Converting {date}")
        print(f"{'='*60}")

        # Note: trades file must come before depth file
        # to prevent double-counting of traded quantity
        input_files = [
            str(trades_file),
            str(l2_file),
        ]

        try:
            convert(
                input_files=input_files,
                output_filename=str(output_file),
                buffer_size=200_000_000,  # Increase buffer for BTC data
                ss_buffer_size=2_000_000,
                base_latency=0,
                snapshot_mode='ignore_sod',  # Ignore start-of-day snapshot
            )
            print(f"Saved: {output_file}")
        except Exception as e:
            print(f"Error converting {date}: {e}")
            raise

    print(f"\n{'='*60}")
    print("Conversion complete!")
    print(f"Output directory: {OUTPUT_DIR}")
    print(f"{'='*60}")


if __name__ == "__main__":
    main()
