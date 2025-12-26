# HFTBacktest Strategy Development Guide

## Overview

This document explains the main loop trigger logic, available data, and best practices for both backtesting and live trading in HFTBacktest.

## Confirmation: Live Trading Has No Persistence

**Confirmed:** The live module (`hftbacktest/src/live/`) has **no persistence functionality**. Strategies can only access:

1. **Real-time order book** (MarketDepth)
2. **Current position** (single `f64` value from exchange)
3. **Active orders** (HashMap in memory, not persisted)
4. **Recent trades** (optional buffer, not persisted)

When the bot restarts:
- Order book is rebuilt from exchange feed
- Position is fetched from exchange API
- Order history is **lost** (only active orders are synced)
- No historical data access

---

## Bot Trait: Unified Interface

Both `Backtest` and `LiveBot` implement the `Bot<MD>` trait, allowing the same strategy code to run in both modes.

```rust
pub trait Bot<MD: MarketDepth> {
    // Time
    fn current_timestamp(&self) -> i64;

    // Market Data
    fn depth(&self, asset_no: usize) -> &MD;
    fn last_trades(&self, asset_no: usize) -> &[Event];

    // Position & State
    fn position(&self, asset_no: usize) -> f64;
    fn state_values(&self, asset_no: usize) -> &StateValues;

    // Orders
    fn orders(&self, asset_no: usize) -> &HashMap<OrderId, Order>;
    fn submit_buy_order(...) -> Result<ElapseResult, Self::Error>;
    fn submit_sell_order(...) -> Result<ElapseResult, Self::Error>;
    fn cancel(&mut self, asset_no: usize, order_id: OrderId, wait: bool);
    fn clear_inactive_orders(&mut self, asset_no: Option<usize>);

    // Time Control
    fn elapse(&mut self, duration: i64) -> Result<ElapseResult, Self::Error>;
    fn elapse_bt(&mut self, duration: i64) -> Result<ElapseResult, Self::Error>;
    fn wait_order_response(...) -> Result<ElapseResult, Self::Error>;
    fn wait_next_feed(...) -> Result<ElapseResult, Self::Error>;

    // Latency Info
    fn feed_latency(&self, asset_no: usize) -> Option<(i64, i64)>;
    fn order_latency(&self, asset_no: usize) -> Option<(i64, i64, i64)>;
}
```

---

## Main Loop Trigger Logic

### Backtest Mode

In backtesting, time is **simulated** based on historical data. The `elapse()` method advances the simulation.

```rust
// Backtest: elapse() processes events up to cur_ts + duration
while hbt.elapse(100_000_000).unwrap() == ElapseResult::Ok {
    // This runs every 100ms of simulated time
    // Events are processed in chronological order
}
```

**How it works internally:**
1. `elapse(duration)` advances `cur_ts` by `duration` nanoseconds
2. All market data events with `timestamp <= cur_ts + duration` are processed
3. Order latency is simulated (Local -> Exchange -> Local)
4. Returns `ElapseResult::EndOfData` when data is exhausted

### Live Mode

In live trading, time is **real**. The `elapse()` method waits for real time to pass while processing incoming events.

```rust
// Live: elapse() waits for duration, processing events as they arrive
while hbt.elapse(500_000_000).unwrap() == ElapseResult::Ok {
    // This runs every 500ms of real time
    // Events are processed as they arrive from connector
}
```

**How it works internally:**
1. `elapse(duration)` sets a timeout of `duration` nanoseconds
2. Waits for events from IPC channel (iceoryx2)
3. Processes `LiveEvent::Feed`, `LiveEvent::Order`, `LiveEvent::Position` as they arrive
4. Returns `ElapseResult::Ok` when timeout is reached
5. Returns `ElapseResult::EndOfData` on interrupt (Ctrl+C)

### Key Differences

| Aspect | Backtest | Live |
|--------|----------|------|
| Time source | Simulated from data | Real wall clock |
| Event processing | Batch (all events up to timestamp) | Streaming (as received) |
| `elapse()` behavior | Advances simulation time | Waits real time |
| `elapse_bt()` | Same as `elapse()` | No-op (returns Ok) |
| Latency | Simulated by LatencyModel | Real network latency |

---

## Available Data

### MarketDepth (Both Modes)

```rust
let depth = hbt.depth(0);

// Best prices
depth.best_bid()      // f64, NAN if no bid
depth.best_ask()      // f64, NAN if no ask
depth.best_bid_tick() // i64, INVALID_MIN if no bid
depth.best_ask_tick() // i64, INVALID_MAX if no ask

// Best quantities
depth.best_bid_qty()  // f64
depth.best_ask_qty()  // f64

// Depth at specific price
depth.bid_qty_at_tick(price_tick)  // f64
depth.ask_qty_at_tick(price_tick)  // f64

// Instrument info
depth.tick_size()  // f64
depth.lot_size()   // f64
```

### Position (Both Modes)

```rust
let position = hbt.position(0);  // f64, positive = long, negative = short
```

### StateValues

```rust
let state = hbt.state_values(0);
state.position       // f64 - Current position (both modes)
state.balance        // f64 - Backtest only
state.fee            // f64 - Backtest only, cumulative
state.num_trades     // i64 - Backtest only
state.trading_volume // f64 - Backtest only
state.trading_value  // f64 - Backtest only
```

**Note:** In live mode, only `position` is valid. Other fields are not populated.

### Orders (Both Modes)

```rust
let orders = hbt.orders(0);  // &HashMap<OrderId, Order>

for (order_id, order) in orders.iter() {
    order.order_id        // u64
    order.price_tick      // i64
    order.price()         // f64 = price_tick * tick_size
    order.qty             // f64 - Original quantity
    order.leaves_qty      // f64 - Remaining quantity
    order.exec_qty        // f64 - Executed quantity
    order.exec_price()    // f64 - Last execution price
    order.side            // Side::Buy or Side::Sell
    order.status          // Status::New, PartiallyFilled, Filled, Canceled, etc.
    order.time_in_force   // TimeInForce::GTC, GTX, FOK, IOC
    order.order_type      // OrdType::Limit, Market
    order.exch_timestamp  // i64 - Exchange timestamp
    order.local_timestamp // i64 - Local timestamp
    order.cancellable()   // bool - Can be canceled
    order.active()        // bool - Still active in market
    order.pending()       // bool - Has pending request
}
```

### Last Trades (Optional, Both Modes)

```rust
// Requires last_trades_capacity > 0 in setup
let trades = hbt.last_trades(0);  // &[Event]

for trade in trades {
    trade.px       // f64 - Trade price
    trade.qty      // f64 - Trade quantity
    trade.exch_ts  // i64 - Exchange timestamp
    trade.local_ts // i64 - Local timestamp
    trade.ev       // u64 - Event flags (contains BUY_EVENT or SELL_EVENT)
}

// Clear after processing
hbt.clear_last_trades(Some(0));
```

### Latency Information (Both Modes)

```rust
// Feed latency: (exchange_ts, local_ts)
if let Some((exch_ts, local_ts)) = hbt.feed_latency(0) {
    let feed_latency_ns = local_ts - exch_ts;
}

// Order latency: (request_ts, exchange_ts, response_ts)
if let Some((req_ts, exch_ts, resp_ts)) = hbt.order_latency(0) {
    let round_trip_ns = resp_ts - req_ts;
}
```

---

## Best Practices

### 1. Main Loop Pattern

```rust
fn run_strategy<MD: MarketDepth, B: Bot<MD>>(hbt: &mut B) -> Result<(), B::Error> {
    let asset_no = 0;
    let tick_size = hbt.depth(asset_no).tick_size();

    // Main loop with appropriate interval
    // Backtest: 100ms is common
    // Live: 500ms+ recommended to avoid rate limits
    while hbt.elapse(100_000_000).unwrap() == ElapseResult::Ok {
        // 1. Clean up inactive orders
        hbt.clear_inactive_orders(Some(asset_no));

        // 2. Check market depth validity
        let depth = hbt.depth(asset_no);
        if depth.best_bid_tick() == INVALID_MIN || depth.best_ask_tick() == INVALID_MAX {
            continue;  // Wait for valid depth
        }

        // 3. Get current state
        let position = hbt.position(asset_no);
        let mid_price = (depth.best_bid() + depth.best_ask()) / 2.0;

        // 4. Calculate target orders
        // ...

        // 5. Cancel/submit orders as needed
        // ...
    }
    Ok(())
}
```

### 2. Order ID Management

Use price tick as order ID to ensure uniqueness:

```rust
// One order per price level
let order_id = (price / tick_size).round() as u64;

// Or combine with side
let order_id = ((price / tick_size).round() as u64) | (side_flag << 63);
```

### 3. Position Limits

```rust
let position = hbt.position(0);
let max_position = 0.1;

// Only submit buy orders if below max
if position < max_position {
    hbt.submit_buy_order(...)?;
}

// Only submit sell orders if above -max
if position > -max_position {
    hbt.submit_sell_order(...)?;
}
```

### 4. Waiting for Order Response

```rust
// Option 1: Don't wait (async)
hbt.submit_buy_order(0, order_id, price, qty, GTX, LIMIT, false)?;

// Option 2: Wait for response (blocking)
hbt.submit_buy_order(0, order_id, price, qty, GTX, LIMIT, true)?;

// Option 3: Manual wait with timeout
hbt.submit_buy_order(0, order_id, price, qty, GTX, LIMIT, false)?;
hbt.wait_order_response(0, order_id, 5_000_000_000)?;  // 5s timeout
```

### 5. Live Trading Considerations

```rust
// Use GTX (Post-only) for maker orders
hbt.submit_buy_order(0, id, price, qty, TimeInForce::GTX, OrdType::Limit, false)?;

// Check minimum notional (e.g., Binance requires $100+)
let notional = price * qty;
if notional < 100.0 {
    return;  // Skip order
}

// Rate limiting: use longer intervals
while hbt.elapse(500_000_000).unwrap() == ElapseResult::Ok {  // 500ms
    // ...
}

// Handle errors gracefully
match hbt.submit_buy_order(...) {
    Ok(_) => {},
    Err(BotError::OrderIdExist) => { /* handle duplicate */ },
    Err(e) => { error!(?e, "Order failed"); }
}
```

### 6. Simulating Processing Time (Backtest)

```rust
// elapse_bt() only affects backtest, ignored in live
hbt.elapse_bt(1_000_000)?;  // Simulate 1ms processing time
```

---

## Data Availability Comparison

| Data | Backtest | Live |
|------|----------|------|
| Market Depth | Full L2/L3 | Full L2/L3 |
| Best Bid/Ask | Yes | Yes |
| Position | Yes | Yes |
| Active Orders | Yes | Yes |
| Balance | Yes | **No** |
| Fee (cumulative) | Yes | **No** |
| Trade Count | Yes | **No** |
| Trading Volume | Yes | **No** |
| Trading Value | Yes | **No** |
| Historical Data | Via data files | **No** |
| Order History | All orders | Active only |
| Feed Latency | Simulated | Real |
| Order Latency | Simulated | Real |

---

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                        Strategy Code                             │
│                     (implements Bot<MD>)                         │
└───────────────────────────┬─────────────────────────────────────┘
                            │
            ┌───────────────┴───────────────┐
            │                               │
            ▼                               ▼
┌───────────────────────┐       ┌───────────────────────┐
│      Backtest         │       │      LiveBot          │
│  ┌─────────────────┐  │       │  ┌─────────────────┐  │
│  │  Local Process  │  │       │  │   IPC Channel   │  │
│  │  (your system)  │  │       │  │   (iceoryx2)    │  │
│  └────────┬────────┘  │       │  └────────┬────────┘  │
│           │           │       │           │           │
│  ┌────────▼────────┐  │       └───────────┼───────────┘
│  │  Exch Process   │  │                   │
│  │  (simulation)   │  │                   ▼
│  └─────────────────┘  │       ┌───────────────────────┐
│                       │       │      Connector        │
│  Data: .npz files     │       │  (Binance/Bybit/...)  │
└───────────────────────┘       └───────────────────────┘
```

---

## Example: Minimal Grid Trading

```rust
use hftbacktest::prelude::*;

fn grid_trading<MD: MarketDepth, B: Bot<MD>>(
    hbt: &mut B,
    half_spread: f64,
    grid_num: usize,
    order_qty: f64,
    max_position: f64,
) -> Result<(), B::Error> {
    let asset_no = 0;
    let tick_size = hbt.depth(asset_no).tick_size();

    while hbt.elapse(100_000_000)? == ElapseResult::Ok {
        hbt.clear_inactive_orders(Some(asset_no));

        let depth = hbt.depth(asset_no);
        if depth.best_bid_tick() == INVALID_MIN {
            continue;
        }

        let mid = (depth.best_bid() + depth.best_ask()) / 2.0;
        let position = hbt.position(asset_no);

        // Calculate grid prices
        let bid_base = mid * (1.0 - half_spread);
        let ask_base = mid * (1.0 + half_spread);

        // Submit orders on grid
        let orders = hbt.orders(asset_no);

        for i in 0..grid_num {
            let bid_price = bid_base - (i as f64) * tick_size;
            let bid_id = (bid_price / tick_size).round() as u64;

            if position < max_position && !orders.contains_key(&bid_id) {
                hbt.submit_buy_order(
                    asset_no, bid_id, bid_price, order_qty,
                    TimeInForce::GTX, OrdType::Limit, false
                )?;
            }

            let ask_price = ask_base + (i as f64) * tick_size;
            let ask_id = (ask_price / tick_size).round() as u64;

            if position > -max_position && !orders.contains_key(&ask_id) {
                hbt.submit_sell_order(
                    asset_no, ask_id, ask_price, order_qty,
                    TimeInForce::GTX, OrdType::Limit, false
                )?;
            }
        }
    }
    Ok(())
}
```

---

## References

- `hftbacktest/src/types.rs` - Core types (Bot trait, Order, Event, etc.)
- `hftbacktest/src/depth/mod.rs` - MarketDepth trait
- `hftbacktest/src/backtest/mod.rs` - Backtest implementation
- `hftbacktest/src/live/bot.rs` - LiveBot implementation
- `hftbacktest/examples/algo.rs` - Grid trading example
- `examples/example.py` - Python example with Numba
