# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

HftBacktest is a high-frequency trading and market-making backtesting framework. The core is written in Rust with Python bindings via PyO3/Maturin. It focuses on accurate simulation by accounting for feed/order latencies and order queue positions using full order book tick data.

## Build Commands

### Rust
```bash
# Build all workspace members
cargo build

# Build with release optimizations
cargo build --release

# Run tests
cargo test

# Run a specific example (from hftbacktest directory)
cargo run --release --example gridtrading_backtest
```

### Python
```bash
# Install in development mode (requires maturin)
cd py-hftbacktest
maturin develop --release

# Install from PyPI
pip install hftbacktest

# Run Python tests
pytest py-hftbacktest/tests/
```

## Architecture

### Workspace Structure
- **hftbacktest/**: Core Rust library - backtesting engine, live trading bot, market depth implementations
- **hftbacktest-derive/**: Procedural macros (NpyDTyped derive)
- **py-hftbacktest/**: Python bindings (PyO3) and pure Python utilities
- **collector/**: Standalone binary for collecting market data from exchanges (Binance, Bybit, Hyperliquid)
- **connector/**: Standalone binary for live trading connections (Binance Futures/Spot, Bybit)

### Core Concepts

**Dual-Mode Architecture**: Same algorithm code runs in both backtesting and live trading via the `Bot` trait, which abstracts order submission, position tracking, and market depth access.

**Local/Exchange Processors**: Backtesting simulates two sides - `Local` (your trading system) and `Exchange` (the remote exchange). Orders flow between them with configurable latency models.

**Market Depth Implementations**:
- `HashMapMarketDepth`: General purpose, handles any price levels
- `ROIVectorMarketDepth`: Optimized for instruments with bounded price ranges
- `BTreeMarketDepth`: For L3 (Market-By-Order) data

**Data Format**: Uses numpy-compatible `.npz` files with `Event` struct (64 bytes: ev flags, timestamps, price, qty, order_id, etc.). The `EXCH_EVENT`/`LOCAL_EVENT` flags indicate which processor sees each event.

**IPC for Live Trading**: Bot and connector processes communicate via iceoryx2 shared memory. The connector handles exchange WebSocket/REST; the bot runs the strategy.

### Feature Flags (Rust)
- `backtest` (default): Backtesting functionality
- `live` (default): Live trading bot
- `s3`: Load data from S3

### Key Traits
- `Bot<MD>`: Core interface for both backtest and live - submit/cancel orders, access depth, elapse time
- `MarketDepth`/`L2MarketDepth`/`L3MarketDepth`: Order book interfaces
- `LatencyModel`: Models order round-trip latency
- `QueueModel`/`L3QueueModel`: Models queue position for fill simulation
- `FeeModel`: Trading fee calculation

### Python Integration
Python code uses Numba JIT compilation. The `@njit` decorated functions receive opaque pointers to Rust structures, calling through `cffi` bindings defined in `py-hftbacktest/hftbacktest/binding.py`.
