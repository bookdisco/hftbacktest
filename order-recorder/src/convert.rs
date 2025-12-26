//! Conversion functions for order recording
//!
//! This module provides functions to convert hftbacktest Order types
//! to order-recorder event types.

use crate::events::{FillEvent, OrderStatus, OrderUpdate, Side};
use hftbacktest::types::{Order, Side as HftSide, Status as HftStatus};

/// Convert hftbacktest Side to order-recorder Side
pub fn convert_side(side: HftSide) -> Side {
    match side {
        HftSide::Buy => Side::Buy,
        HftSide::Sell => Side::Sell,
        HftSide::None | HftSide::Unsupported => Side::Buy, // Default
    }
}

/// Convert hftbacktest Status to order-recorder OrderStatus
pub fn convert_status(status: HftStatus) -> OrderStatus {
    match status {
        HftStatus::New => OrderStatus::New,
        HftStatus::PartiallyFilled => OrderStatus::PartiallyFilled,
        HftStatus::Filled => OrderStatus::Filled,
        HftStatus::Canceled => OrderStatus::Canceled,
        HftStatus::Rejected => OrderStatus::Rejected,
        HftStatus::Expired => OrderStatus::Expired,
        HftStatus::None | HftStatus::Replaced | HftStatus::Unsupported => OrderStatus::New,
    }
}

/// Create an OrderUpdate from hftbacktest Order
///
/// # Arguments
/// * `prev_order` - The previous order state
/// * `new_order` - The new order state
/// * `strategy_id` - The strategy identifier
/// * `symbol` - The trading symbol
/// * `timestamp` - Current timestamp in nanoseconds
/// * `tick_size` - Tick size for price conversion (order.tick_size may not be set correctly)
pub fn create_order_update(
    prev_order: &Order,
    new_order: &Order,
    strategy_id: &str,
    symbol: &str,
    timestamp: i64,
    tick_size: f64,
) -> OrderUpdate {
    let latency_ns = if new_order.exch_timestamp > 0 && new_order.local_timestamp > 0 {
        new_order.exch_timestamp - new_order.local_timestamp
    } else {
        0
    };

    OrderUpdate {
        timestamp,
        strategy_id: strategy_id.to_string(),
        order_id: new_order.order_id,
        symbol: symbol.to_string(),
        side: convert_side(new_order.side),
        price: new_order.price_tick as f64 * tick_size,
        qty: new_order.qty,
        status: convert_status(new_order.status),
        prev_status: convert_status(prev_order.status),
        leaves_qty: new_order.leaves_qty,
        exec_qty: new_order.exec_qty,
        exec_price: new_order.exec_price_tick as f64 * tick_size,
        exch_time: new_order.exch_timestamp,
        latency_ns,
    }
}

/// Check if a fill occurred between two order states
///
/// Returns the fill quantity if there was a fill, None otherwise
pub fn detect_fill(prev_order: &Order, new_order: &Order) -> Option<f64> {
    let fill_qty = new_order.exec_qty - prev_order.exec_qty;
    if fill_qty > 1e-10 {
        Some(fill_qty)
    } else {
        None
    }
}

/// Create a FillEvent from order state change
///
/// # Arguments
/// * `prev_order` - The previous order state
/// * `new_order` - The new order state
/// * `fill_qty` - The quantity filled
/// * `strategy_id` - The strategy identifier
/// * `symbol` - The trading symbol
/// * `timestamp` - Current timestamp in nanoseconds
/// * `mid_price` - Current mid price for slippage calculation
/// * `fill_id` - Unique fill ID
/// * `tick_size` - Tick size for price conversion
pub fn create_fill_event(
    _prev_order: &Order,
    new_order: &Order,
    fill_qty: f64,
    strategy_id: &str,
    symbol: &str,
    timestamp: i64,
    mid_price: f64,
    fill_id: u64,
    tick_size: f64,
) -> FillEvent {
    FillEvent {
        timestamp,
        strategy_id: strategy_id.to_string(),
        fill_id,
        order_id: new_order.order_id,
        symbol: symbol.to_string(),
        side: convert_side(new_order.side),
        price: new_order.exec_price_tick as f64 * tick_size,
        qty: fill_qty,
        fee: 0.0, // Fee is typically not available at this point
        is_maker: new_order.maker,
        exch_time: new_order.exch_timestamp,
        mid_price,
    }
}

/// Helper to get the round-trip latency from an order
///
/// Returns the latency in nanoseconds, or 0 if timestamps are not available
pub fn get_latency_ns(order: &Order) -> i64 {
    if order.exch_timestamp > 0 && order.local_timestamp > 0 {
        order.exch_timestamp - order.local_timestamp
    } else {
        0
    }
}
