#!/bin/bash
# Start Connector Script
#
# This script starts the connector process that handles:
# - REST API communication with exchange
# - WebSocket market data stream
# - WebSocket user data stream (orders, positions)
# - IPC communication with trading bots
#
# Usage: ./start_connector.sh <connector_name> <exchange> <config_file>
# Example: ./start_connector.sh bf binancefutures config/binancefutures_testnet.toml

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

CONNECTOR_NAME=${1:-bf}
EXCHANGE=${2:-binancefutures}
CONFIG_FILE=${3:-$SCRIPT_DIR/../config/binancefutures_testnet.toml}

echo "========================================"
echo " HFTBacktest Connector Startup"
echo "========================================"
echo "Connector Name: $CONNECTOR_NAME"
echo "Exchange: $EXCHANGE"
echo "Config File: $CONFIG_FILE"
echo "========================================"

if [ ! -f "$CONFIG_FILE" ]; then
    echo "ERROR: Config file not found: $CONFIG_FILE"
    exit 1
fi

# Check if API keys are configured
if grep -q 'api_key = ""' "$CONFIG_FILE"; then
    echo ""
    echo "WARNING: API key not configured in $CONFIG_FILE"
    echo "Please edit the config file and add your testnet API keys."
    echo ""
fi

echo ""
echo "Starting connector..."
echo "Press Ctrl+C to stop"
echo ""

cd "$ROOT_DIR"
RUST_LOG=info cargo run --release --package connector -- "$CONNECTOR_NAME" "$EXCHANGE" "$CONFIG_FILE"
