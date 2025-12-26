# Changes Summary

## Order Recording Module (order-recorder)

A new crate for recording live trading activity to ClickHouse for analysis and monitoring.

### Features

- **IPC Communication**: Uses iceoryx2 for zero-copy inter-process communication
- **ClickHouse Storage**: Async batched writes to ClickHouse for time-series data
- **Order Hook Integration**: `OrderHookContext` integrates with `LiveBotBuilder::order_recv_hook()`
- **Latency Tracking**: Records order round-trip latency (local_timestamp -> exch_timestamp)
- **Fill Detection**: Automatically detects fills from order state changes
- **Position Snapshots**: Periodic position and PnL snapshots

### Architecture

```
Strategy (Bot)  --IPC-->  order-recorder  --HTTP-->  ClickHouse
     |                         |
     v                         v
OrderHookContext          RecordingService
     |                         |
     v                         v
RecordingPublisher        ClickHouseWriter
```

### Files

- `order-recorder/src/events.rs` - IPC event types (OrderUpdate, FillEvent, PositionSnapshot, StrategyStatus)
- `order-recorder/src/service.rs` - Recording service (IPC subscriber) and RecordingPublisher
- `order-recorder/src/writer.rs` - Async ClickHouse writer with batching
- `order-recorder/src/convert.rs` - Conversion from hftbacktest::Order to recording events
- `order-recorder/src/hook.rs` - Public OrderHookContext for strategy integration
- `order-recorder/src/main.rs` - Standalone recording service binary

### Usage

```rust
use order_recorder::{StrategyState, hook::create_order_hook};

// Create hook context (returns None if service unavailable)
let hook_ctx = create_order_hook(
    "hft_recording",           // IPC service name
    "my_strategy".to_string(), // Strategy ID
    "btcusdt".to_string(),     // Symbol
    0.1,                       // tick_size
);

// Use with LiveBotBuilder
if let Some(ctx) = hook_ctx.clone() {
    builder = builder.order_recv_hook(move |prev, new| {
        ctx.handle_order(prev, new);
        Ok(())
    });
}

// Publish status/position anytime
if let Some(ref ctx) = hook_ctx {
    ctx.publish_status(StrategyState::Running, "Running".to_string());
    ctx.publish_position(position, avg_price, mid_price, unrealized_pnl, realized_pnl);
}
```

### ClickHouse Tables

```sql
-- Orders table
CREATE TABLE hft.orders (
    order_id UInt64, symbol String, side Enum8, price Float64, qty Float64,
    status Enum8, create_time DateTime64(9), update_time DateTime64(9),
    exch_time DateTime64(9), leaves_qty Float64, exec_qty Float64,
    exec_price Float64, latency_ns Int64, strategy_id String
) ENGINE = MergeTree() ORDER BY (strategy_id, symbol, update_time);

-- Fills table
CREATE TABLE hft.fills (
    fill_id UInt64, order_id UInt64, symbol String, side Enum8,
    price Float64, qty Float64, fee Float64, is_maker UInt8,
    exch_time DateTime64(9), local_time DateTime64(9),
    slippage_bps Float64, strategy_id String
) ENGINE = MergeTree() ORDER BY (strategy_id, symbol, local_time);

-- Positions table
CREATE TABLE hft.positions (
    timestamp DateTime64(9), symbol String, position Float64,
    avg_price Float64, unrealized_pnl Float64, realized_pnl Float64,
    strategy_id String
) ENGINE = MergeTree() ORDER BY (strategy_id, symbol, timestamp);

-- Strategy status table
CREATE TABLE hft.strategy_status (
    timestamp DateTime64(9), strategy_id String, status UInt8, message String
) ENGINE = MergeTree() ORDER BY (strategy_id, timestamp);
```

### Latency Query Examples

```sql
-- Latency statistics
SELECT
    avg(latency_ns)/1000000 as avg_ms,
    min(latency_ns)/1000000 as min_ms,
    quantile(0.5)(latency_ns)/1000000 as p50_ms,
    quantile(0.95)(latency_ns)/1000000 as p95_ms
FROM hft.orders WHERE latency_ns > 0 AND latency_ns < 1000000000;

-- Orders by status
SELECT status, count() FROM hft.orders GROUP BY status;
```

## OBI MM Strategy Updates

### Testnet Binary

- `strategies/obi_mm_rust/src/testnet.rs` - New testnet trading binary with order recording
- Removed `strategies/obi_mm_rust/src/live.rs` (replaced by testnet.rs)

### Configuration Files

- `examples/testnet/config/obi_mm_testnet.toml` - OBI MM testnet configuration
- `examples/testnet/config/obi_mm_btcusdt.toml` - BTCUSDT specific configuration
- `examples/testnet/scripts/start_obi_mm.sh` - Launch script for OBI MM strategy

## Workspace Updates

- Added `order-recorder` to workspace members in root `Cargo.toml`
- Updated `strategies/obi_mm_rust/Cargo.toml` to depend on `order-recorder`
