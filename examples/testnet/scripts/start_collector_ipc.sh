#!/bin/bash
# Start Collector Script (IPC Mode)
#
# This script starts the collector in IPC mode to record market data
# from a running connector. This is useful when you want to:
# - Record the same data that your live bot receives
# - Avoid duplicate WebSocket connections
# - Record data with same latency characteristics as live trading
#
# Usage: ./start_collector_ipc.sh <connector_name> <output_dir> <symbol1> [symbol2] ...
# Example: ./start_collector_ipc.sh bf ./data btcusdt ethusdt
#
# NOTE: The connector must be running before starting IPC collector

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

CONNECTOR_NAME=${1:-bf}
OUTPUT_DIR=${2:-$SCRIPT_DIR/../data}
shift 2 2>/dev/null || true
SYMBOLS=${@:-btcusdt}

echo "========================================"
echo " HFTBacktest Data Collector (IPC Mode)"
echo "========================================"
echo "Connector: $CONNECTOR_NAME"
echo "Output Dir: $OUTPUT_DIR"
echo "Symbols: $SYMBOLS"
echo "========================================"

# Create output directory
mkdir -p "$OUTPUT_DIR"

echo ""
echo "Starting IPC data collection..."
echo "Make sure connector '$CONNECTOR_NAME' is running!"
echo "Data will be saved to: $OUTPUT_DIR"
echo "Press Ctrl+C to stop"
echo ""

cd "$ROOT_DIR"
RUST_LOG=info cargo run --release --package collector -- "$OUTPUT_DIR" --ipc "$CONNECTOR_NAME" $SYMBOLS
