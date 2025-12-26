#!/bin/bash
# Convert Collector Data Script
#
# This script converts raw collector data (gzip) to NPZ format
# that can be used for backtesting.
#
# Usage: ./convert_data.sh <input_file.gz> [output.npz]
# Example: ./convert_data.sh ./data/btcusdt_20241225.gz ./data/btcusdt_20241225.npz

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

INPUT_FILE=${1}
OUTPUT_FILE=${2}

if [ -z "$INPUT_FILE" ]; then
    echo "Usage: $0 <input_file.gz> [output.npz]"
    echo "Example: $0 ./data/btcusdt_20241225.gz ./data/btcusdt_20241225.npz"
    exit 1
fi

echo "========================================"
echo " HFTBacktest Data Converter"
echo "========================================"
echo "Input: $INPUT_FILE"
if [ -n "$OUTPUT_FILE" ]; then
    echo "Output: $OUTPUT_FILE"
fi
echo "========================================"

if [ ! -f "$INPUT_FILE" ]; then
    echo "ERROR: Input file not found: $INPUT_FILE"
    exit 1
fi

echo ""
echo "Converting data..."
echo ""

cd "$ROOT_DIR"
if [ -n "$OUTPUT_FILE" ]; then
    cargo run --release --example convert_collector_data -- "$INPUT_FILE" "$OUTPUT_FILE"
else
    cargo run --release --example convert_collector_data -- "$INPUT_FILE"
fi
