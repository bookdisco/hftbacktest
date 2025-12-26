//! Order Recording Hook for live strategies
//!
//! This module provides a reusable order hook context that can be used
//! with any live trading strategy to record orders and fills.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hftbacktest::types::Order;
use tracing::{info, error};

use crate::events::{PositionSnapshot, StrategyState, StrategyStatus};
use crate::service::RecordingPublisher;
use crate::convert::{create_order_update, create_fill_event, detect_fill};

/// Get current timestamp in nanoseconds
fn current_timestamp_ns() -> i64 {
    chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
}

/// Order hook context for recording order updates and fills.
///
/// This context is designed to be used with `LiveBotBuilder::order_recv_hook()`.
/// It automatically records all order state changes and detects fills.
///
/// # Example
/// ```ignore
/// use std::sync::Arc;
/// use order_recorder::hook::OrderHookContext;
///
/// let hook_ctx = OrderHookContext::new(
///     publisher,
///     "my_strategy".to_string(),
///     "btcusdt".to_string(),
///     0.1,  // tick_size
/// );
///
/// let ctx = Arc::new(hook_ctx);
/// let ctx_clone = ctx.clone();
///
/// let bot = LiveBotBuilder::new()
///     .register(instrument)
///     .order_recv_hook(move |prev, new| {
///         ctx_clone.handle_order(prev, new);
///         Ok(())
///     })
///     .build()?;
/// ```
pub struct OrderHookContext {
    publisher: RecordingPublisher,
    strategy_id: String,
    symbol: String,
    tick_size: f64,
    fill_id_counter: AtomicU64,
}

impl OrderHookContext {
    /// Create a new order hook context.
    ///
    /// # Arguments
    /// * `publisher` - The recording publisher for IPC
    /// * `strategy_id` - Unique identifier for the strategy
    /// * `symbol` - Trading symbol (e.g., "btcusdt")
    /// * `tick_size` - Tick size for price conversion
    pub fn new(
        publisher: RecordingPublisher,
        strategy_id: String,
        symbol: String,
        tick_size: f64,
    ) -> Self {
        Self {
            publisher,
            strategy_id,
            symbol,
            tick_size,
            fill_id_counter: AtomicU64::new(1),
        }
    }

    /// Get the strategy ID
    pub fn strategy_id(&self) -> &str {
        &self.strategy_id
    }

    /// Get the symbol
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Get the tick size
    pub fn tick_size(&self) -> f64 {
        self.tick_size
    }

    /// Handle an order update from the order_recv_hook.
    ///
    /// This method should be called from the order_recv_hook closure.
    /// It will:
    /// 1. Create and publish an OrderUpdate event
    /// 2. Detect fills and publish FillEvent if applicable
    ///
    /// # Arguments
    /// * `prev_order` - The previous order state
    /// * `new_order` - The new order state
    pub fn handle_order(&self, prev_order: &Order, new_order: &Order) {
        let timestamp = current_timestamp_ns();

        // Create and publish order update
        let order_update = create_order_update(
            prev_order,
            new_order,
            &self.strategy_id,
            &self.symbol,
            timestamp,
            self.tick_size,
        );

        info!(
            "Order: id={} {:?}->{:?} price={:.2} qty={} latency={:.2}ms",
            order_update.order_id,
            order_update.prev_status,
            order_update.status,
            order_update.price,
            order_update.qty,
            order_update.latency_ns as f64 / 1_000_000.0
        );

        if let Err(e) = self.publisher.publish_order(order_update) {
            error!("Failed to publish order: {}", e);
        }

        // Check for fill and publish fill event
        if let Some(fill_qty) = detect_fill(prev_order, new_order) {
            let fill_id = self.fill_id_counter.fetch_add(1, Ordering::Relaxed);

            // Use exec_price as approximation for mid_price
            let mid_price = new_order.exec_price_tick as f64 * self.tick_size;

            let fill_event = create_fill_event(
                prev_order,
                new_order,
                fill_qty,
                &self.strategy_id,
                &self.symbol,
                timestamp,
                mid_price,
                fill_id,
                self.tick_size,
            );

            info!(
                "Fill: id={} order={} price={:.2} qty={} maker={}",
                fill_event.fill_id,
                fill_event.order_id,
                fill_event.price,
                fill_event.qty,
                fill_event.is_maker
            );

            if let Err(e) = self.publisher.publish_fill(fill_event) {
                error!("Failed to publish fill: {}", e);
            }
        }
    }

    /// Publish a strategy status event.
    ///
    /// # Arguments
    /// * `state` - The current strategy state
    /// * `message` - A message describing the status
    pub fn publish_status(&self, state: StrategyState, message: String) {
        let status = StrategyStatus {
            timestamp: current_timestamp_ns(),
            strategy_id: self.strategy_id.clone(),
            state,
            message,
        };
        if let Err(e) = self.publisher.publish_status(status) {
            error!("Failed to publish status: {}", e);
        }
    }

    /// Publish a position snapshot.
    ///
    /// # Arguments
    /// * `position` - Current position (positive = long, negative = short)
    /// * `avg_price` - Average entry price
    /// * `mid_price` - Current mid price
    /// * `unrealized_pnl` - Unrealized PnL
    /// * `realized_pnl` - Realized PnL
    pub fn publish_position(
        &self,
        position: f64,
        avg_price: f64,
        mid_price: f64,
        unrealized_pnl: f64,
        realized_pnl: f64,
    ) {
        let snapshot = PositionSnapshot {
            timestamp: current_timestamp_ns(),
            strategy_id: self.strategy_id.clone(),
            symbol: self.symbol.clone(),
            position,
            avg_price,
            mid_price,
            unrealized_pnl,
            realized_pnl,
        };
        if let Err(e) = self.publisher.publish_position(snapshot) {
            error!("Failed to publish position: {}", e);
        }
    }
}

/// Create an OrderHookContext wrapped in Arc, ready for use with order_recv_hook.
///
/// Returns None if the recording service is not available.
///
/// # Arguments
/// * `service_name` - IPC service name (e.g., "hft_recording")
/// * `strategy_id` - Unique identifier for the strategy
/// * `symbol` - Trading symbol
/// * `tick_size` - Tick size for price conversion
pub fn create_order_hook(
    service_name: &str,
    strategy_id: String,
    symbol: String,
    tick_size: f64,
) -> Option<Arc<OrderHookContext>> {
    match RecordingPublisher::new(service_name) {
        Ok(publisher) => {
            info!("Connected to recording service: {}", service_name);
            Some(Arc::new(OrderHookContext::new(
                publisher,
                strategy_id,
                symbol,
                tick_size,
            )))
        }
        Err(e) => {
            tracing::warn!(
                "Failed to connect to recording service '{}': {}. Order recording disabled.",
                service_name,
                e
            );
            None
        }
    }
}
