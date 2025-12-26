#!/bin/bash
# Start Collector Script (Direct Exchange Connection)
#
# This script starts the collector to record market data directly from exchange.
# Data is saved in gzip-compressed format for later conversion and backtesting.
#
# Usage: ./start_collector.sh <exchange> <output_dir> <symbol1> [symbol2] ...
# Example: ./start_collector.sh binancefutures ./data btcusdt ethusdt

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

EXCHANGE=${1:-binancefutures}
OUTPUT_DIR=${2:-$SCRIPT_DIR/../data}
shift 2 2>/dev/null || true
SYMBOLS=${@:-btcusdt}

echo "========================================"
echo " HFTBacktest Data Collector"
echo "========================================"
echo "Exchange: $EXCHANGE"
echo "Output Dir: $OUTPUT_DIR"
echo "Symbols: $SYMBOLS"
echo "========================================"

# Create output directory
mkdir -p "$OUTPUT_DIR"

echo ""
echo "Starting data collection..."
echo "Data will be saved to: $OUTPUT_DIR"
echo "Press Ctrl+C to stop"
echo ""

cd "$ROOT_DIR"
RUST_LOG=info cargo run --release --package collector -- "$OUTPUT_DIR" "$EXCHANGE" $SYMBOLS
