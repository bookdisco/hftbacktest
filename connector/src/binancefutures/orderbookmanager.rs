use std::collections::{HashMap, VecDeque};

use tracing::{debug, info, warn};

use super::msg::{rest, stream};

/// Order book synchronization state for a single symbol
#[derive(Debug)]
pub enum SyncState {
    /// Not initialized, buffering events and waiting for snapshot
    Initializing,
    /// Synchronized and processing events normally
    Synchronized,
    /// Detected a gap, need to re-initialize
    NeedResync,
}

/// Manages order book synchronization for a single symbol following Binance documentation:
/// https://binance-docs.github.io/apidocs/futures/en/#how-to-manage-a-local-order-book-correctly
#[derive(Debug)]
pub struct SymbolOrderBook {
    pub symbol: String,
    /// Current synchronization state
    pub state: SyncState,
    /// Buffered depth events during initialization
    pub pending_events: VecDeque<stream::Depth>,
    /// Last processed update ID (u from the last event)
    pub last_update_id: Option<i64>,
    /// Snapshot's lastUpdateId for validation
    pub snapshot_last_update_id: Option<i64>,
    /// When true, we synced from snapshot only (no buffered events processed)
    /// The next event must be validated as a "first event" before using pu continuity
    pub awaiting_first_event: bool,
}

impl SymbolOrderBook {
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            state: SyncState::Initializing,
            pending_events: VecDeque::new(),
            last_update_id: None,
            snapshot_last_update_id: None,
            awaiting_first_event: false,
        }
    }

    /// Reset to initializing state (called on gaps or errors)
    pub fn reset(&mut self) {
        self.state = SyncState::Initializing;
        self.pending_events.clear();
        self.last_update_id = None;
        self.snapshot_last_update_id = None;
        self.awaiting_first_event = false;
        info!(symbol = %self.symbol, "Order book reset to initializing state");
    }
}

/// Manages order book synchronization for multiple symbols
pub struct OrderBookManager {
    /// Per-symbol order book state
    books: HashMap<String, SymbolOrderBook>,
}

impl OrderBookManager {
    pub fn new() -> Self {
        Self {
            books: HashMap::new(),
        }
    }

    /// Get or create order book state for a symbol
    fn get_or_create(&mut self, symbol: &str) -> &mut SymbolOrderBook {
        self.books
            .entry(symbol.to_string())
            .or_insert_with(|| SymbolOrderBook::new(symbol.to_string()))
    }

    /// Check if a symbol needs snapshot (initializing or needs resync)
    pub fn needs_snapshot(&self, symbol: &str) -> bool {
        match self.books.get(symbol) {
            None => true,
            Some(book) => matches!(book.state, SyncState::Initializing | SyncState::NeedResync),
        }
    }

    /// Check if a symbol is currently initializing (buffering events)
    pub fn is_initializing(&self, symbol: &str) -> bool {
        match self.books.get(symbol) {
            None => false,
            Some(book) => matches!(book.state, SyncState::Initializing),
        }
    }

    /// Start initialization for a symbol (called when subscribing)
    pub fn start_init(&mut self, symbol: &str) {
        let book = self.get_or_create(symbol);
        book.reset();
        info!(symbol = %symbol, "Started order book initialization");
    }

    /// Handle incoming WebSocket depth update
    /// Returns: (should_process, should_request_snapshot)
    pub fn on_depth_update(&mut self, data: &stream::Depth) -> (bool, bool) {
        let book = self.get_or_create(&data.symbol);

        match book.state {
            SyncState::Initializing => {
                // Buffer the event during initialization
                book.pending_events.push_back(stream::Depth {
                    transaction_time: data.transaction_time,
                    event_time: data.event_time,
                    symbol: data.symbol.clone(),
                    first_update_id: data.first_update_id,
                    last_update_id: data.last_update_id,
                    prev_update_id: data.prev_update_id,
                    bids: data.bids.clone(),
                    asks: data.asks.clone(),
                });
                debug!(
                    symbol = %data.symbol,
                    first_update_id = data.first_update_id,
                    last_update_id = data.last_update_id,
                    buffered_count = book.pending_events.len(),
                    "Buffered depth update during initialization"
                );
                // Don't process, might need snapshot if not already requested
                (false, book.pending_events.len() == 1)
            }
            SyncState::Synchronized => {
                // If we're awaiting the first event after snapshot-only sync,
                // validate it as a first event (U <= lastUpdateId AND u > lastUpdateId)
                if book.awaiting_first_event {
                    if let Some(snapshot_u) = book.snapshot_last_update_id {
                        if data.first_update_id <= snapshot_u && data.last_update_id > snapshot_u {
                            // Valid first event
                            info!(
                                symbol = %data.symbol,
                                first_update_id = data.first_update_id,
                                last_update_id = data.last_update_id,
                                "First valid event after snapshot"
                            );
                            book.awaiting_first_event = false;
                            book.last_update_id = Some(data.last_update_id);
                            return (true, false);
                        } else if data.last_update_id <= snapshot_u {
                            // Event is too old, skip it
                            debug!(
                                symbol = %data.symbol,
                                event_u = data.last_update_id,
                                snapshot_u = snapshot_u,
                                "Skipping old event while awaiting first valid event"
                            );
                            return (false, false);
                        } else {
                            // Gap: U > snapshot_u
                            warn!(
                                symbol = %data.symbol,
                                event_U = data.first_update_id,
                                snapshot_u = snapshot_u,
                                "Gap detected: first event U > snapshot lastUpdateId"
                            );
                            book.reset();
                            book.pending_events.push_back(stream::Depth {
                                transaction_time: data.transaction_time,
                                event_time: data.event_time,
                                symbol: data.symbol.clone(),
                                first_update_id: data.first_update_id,
                                last_update_id: data.last_update_id,
                                prev_update_id: data.prev_update_id,
                                bids: data.bids.clone(),
                                asks: data.asks.clone(),
                            });
                            return (false, true);
                        }
                    }
                }

                // Check continuity: pu should equal our last_update_id
                if let Some(last_u) = book.last_update_id {
                    if data.prev_update_id != last_u {
                        warn!(
                            symbol = %data.symbol,
                            expected_pu = last_u,
                            actual_pu = data.prev_update_id,
                            "Gap detected! prev_update_id mismatch"
                        );
                        book.reset();
                        // Buffer this event and request new snapshot
                        book.pending_events.push_back(stream::Depth {
                            transaction_time: data.transaction_time,
                            event_time: data.event_time,
                            symbol: data.symbol.clone(),
                            first_update_id: data.first_update_id,
                            last_update_id: data.last_update_id,
                            prev_update_id: data.prev_update_id,
                            bids: data.bids.clone(),
                            asks: data.asks.clone(),
                        });
                        return (false, true);
                    }
                }
                // Update last_update_id and process normally
                book.last_update_id = Some(data.last_update_id);
                (true, false)
            }
            SyncState::NeedResync => {
                // Buffer and wait for snapshot
                book.pending_events.push_back(stream::Depth {
                    transaction_time: data.transaction_time,
                    event_time: data.event_time,
                    symbol: data.symbol.clone(),
                    first_update_id: data.first_update_id,
                    last_update_id: data.last_update_id,
                    prev_update_id: data.prev_update_id,
                    bids: data.bids.clone(),
                    asks: data.asks.clone(),
                });
                (false, false)
            }
        }
    }

    /// Handle REST snapshot response
    /// Returns events that should be processed (snapshot + valid buffered events)
    pub fn on_snapshot(
        &mut self,
        symbol: &str,
        snapshot: &rest::Depth,
    ) -> Vec<DepthEvent> {
        let book = self.get_or_create(symbol);
        let last_update_id = snapshot.last_update_id;

        info!(
            symbol = %symbol,
            snapshot_last_update_id = last_update_id,
            buffered_events = book.pending_events.len(),
            "Processing snapshot"
        );

        let mut events_to_process = Vec::new();

        // First, add the snapshot as a full refresh
        events_to_process.push(DepthEvent::Snapshot {
            symbol: symbol.to_string(),
            transaction_time: snapshot.transaction_time,
            bids: snapshot.bids.clone(),
            asks: snapshot.asks.clone(),
            last_update_id,
        });

        // Process buffered events according to Binance documentation:
        // 1. Drop any event where u <= lastUpdateId
        // 2. The first processed event should have U <= lastUpdateId AND u >= lastUpdateId
        // 3. Each subsequent event's pu should equal previous u

        let mut found_first_valid = false;
        let mut current_u: Option<i64> = None;

        while let Some(event) = book.pending_events.pop_front() {
            // Rule: Drop any event where u <= lastUpdateId
            if event.last_update_id <= last_update_id {
                debug!(
                    symbol = %symbol,
                    event_u = event.last_update_id,
                    snapshot_last_update_id = last_update_id,
                    "Dropping event: u <= lastUpdateId"
                );
                continue;
            }

            if !found_first_valid {
                // Rule: The first processed event should have U <= lastUpdateId AND u >= lastUpdateId
                // Note: For Futures, the condition is slightly different:
                // U <= lastUpdateId + 1 AND u >= lastUpdateId + 1
                // But we use the more permissive check: U <= lastUpdateId AND u > lastUpdateId
                if event.first_update_id <= last_update_id && event.last_update_id > last_update_id {
                    found_first_valid = true;
                    current_u = Some(event.last_update_id);

                    info!(
                        symbol = %symbol,
                        first_update_id = event.first_update_id,
                        last_update_id = event.last_update_id,
                        "Found first valid event after snapshot"
                    );

                    events_to_process.push(DepthEvent::Update {
                        symbol: symbol.to_string(),
                        transaction_time: event.transaction_time,
                        bids: event.bids,
                        asks: event.asks,
                        first_update_id: event.first_update_id,
                        last_update_id: event.last_update_id,
                    });
                } else {
                    // This event doesn't satisfy the first event condition
                    // It might be too new (first_update_id > last_update_id)
                    // Keep buffering and wait for more events, or process if it's sequential
                    if event.first_update_id > last_update_id + 1 {
                        warn!(
                            symbol = %symbol,
                            event_first_update_id = event.first_update_id,
                            snapshot_last_update_id = last_update_id,
                            "Gap between snapshot and first event! Need to re-fetch snapshot"
                        );
                        // Put event back and mark for resync
                        book.pending_events.push_front(event);
                        book.state = SyncState::NeedResync;
                        return vec![]; // Return empty, caller should re-request snapshot
                    }
                    debug!(
                        symbol = %symbol,
                        event_U = event.first_update_id,
                        event_u = event.last_update_id,
                        snapshot_last_update_id = last_update_id,
                        "Skipping event: doesn't satisfy first event condition"
                    );
                }
            } else {
                // Subsequent events: check pu continuity
                if let Some(prev_u) = current_u {
                    if event.prev_update_id != prev_u {
                        warn!(
                            symbol = %symbol,
                            expected_pu = prev_u,
                            actual_pu = event.prev_update_id,
                            "Gap in buffered events! pu mismatch"
                        );
                        // Put remaining events back (including this one) and mark for resync
                        book.pending_events.push_front(event);
                        book.state = SyncState::NeedResync;
                        return events_to_process; // Return what we have so far
                    }
                }

                current_u = Some(event.last_update_id);
                events_to_process.push(DepthEvent::Update {
                    symbol: symbol.to_string(),
                    transaction_time: event.transaction_time,
                    bids: event.bids,
                    asks: event.asks,
                    first_update_id: event.first_update_id,
                    last_update_id: event.last_update_id,
                });
            }
        }

        // Successfully synchronized
        book.state = SyncState::Synchronized;
        book.snapshot_last_update_id = Some(last_update_id);

        if let Some(u) = current_u {
            // We processed some buffered events, use the last one's u
            book.last_update_id = Some(u);
            book.awaiting_first_event = false;
        } else {
            // No buffered events were processed, need to validate first incoming event
            book.last_update_id = Some(last_update_id);
            book.awaiting_first_event = true;
            info!(
                symbol = %symbol,
                last_update_id = last_update_id,
                "Synced from snapshot only, awaiting first valid WebSocket event"
            );
        }

        info!(
            symbol = %symbol,
            last_update_id = ?book.last_update_id,
            awaiting_first_event = book.awaiting_first_event,
            events_count = events_to_process.len(),
            "Order book synchronized successfully"
        );

        events_to_process
    }

    /// Mark a symbol as needing resync (e.g., on WebSocket reconnect)
    pub fn mark_resync(&mut self, symbol: &str) {
        if let Some(book) = self.books.get_mut(symbol) {
            book.reset();
        }
    }

    /// Mark all symbols as needing resync (e.g., on WebSocket reconnect)
    pub fn mark_all_resync(&mut self) {
        for book in self.books.values_mut() {
            book.reset();
        }
        info!("All order books marked for resync");
    }

    /// Get all symbols that need snapshot
    pub fn symbols_needing_snapshot(&self) -> Vec<String> {
        self.books
            .iter()
            .filter(|(_, book)| matches!(book.state, SyncState::Initializing | SyncState::NeedResync))
            .map(|(symbol, _)| symbol.clone())
            .collect()
    }
}

impl Default for OrderBookManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Depth event to be processed by the data stream
#[derive(Debug, Clone)]
pub enum DepthEvent {
    /// Full snapshot (clear and rebuild)
    Snapshot {
        symbol: String,
        transaction_time: i64,
        bids: Vec<(String, String)>,
        asks: Vec<(String, String)>,
        last_update_id: i64,
    },
    /// Incremental update
    Update {
        symbol: String,
        transaction_time: i64,
        bids: Vec<(String, String)>,
        asks: Vec<(String, String)>,
        first_update_id: i64,
        last_update_id: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_depth_event(
        symbol: &str,
        first_u: i64,
        last_u: i64,
        prev_u: i64,
    ) -> stream::Depth {
        stream::Depth {
            transaction_time: 1000,
            event_time: 1000,
            symbol: symbol.to_string(),
            first_update_id: first_u,
            last_update_id: last_u,
            prev_update_id: prev_u,
            bids: vec![("100.0".to_string(), "1.0".to_string())],
            asks: vec![("101.0".to_string(), "1.0".to_string())],
        }
    }

    fn make_snapshot(last_update_id: i64) -> rest::Depth {
        rest::Depth {
            last_update_id,
            event_time: 1000,
            transaction_time: 1000,
            bids: vec![("100.0".to_string(), "10.0".to_string())],
            asks: vec![("101.0".to_string(), "10.0".to_string())],
        }
    }

    #[test]
    fn test_basic_sync_flow() {
        let mut manager = OrderBookManager::new();

        // Start initialization
        manager.start_init("btcusdt");
        assert!(manager.is_initializing("btcusdt"));
        assert!(manager.needs_snapshot("btcusdt"));

        // Receive some depth updates (should be buffered)
        let event1 = make_depth_event("btcusdt", 100, 105, 99);
        let (should_process, should_request) = manager.on_depth_update(&event1);
        assert!(!should_process);
        assert!(should_request); // First event triggers snapshot request

        let event2 = make_depth_event("btcusdt", 106, 110, 105);
        let (should_process, should_request) = manager.on_depth_update(&event2);
        assert!(!should_process);
        assert!(!should_request); // Already requested

        // Receive snapshot with lastUpdateId = 102
        let snapshot = make_snapshot(102);
        let events = manager.on_snapshot("btcusdt", &snapshot);

        // Should have: snapshot + event1 (U=100 <= 102, u=105 > 102) + event2 (pu=105 matches)
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], DepthEvent::Snapshot { .. }));
        assert!(matches!(events[1], DepthEvent::Update { first_update_id: 100, .. }));
        assert!(matches!(events[2], DepthEvent::Update { first_update_id: 106, .. }));

        // Should now be synchronized
        assert!(!manager.is_initializing("btcusdt"));
        assert!(!manager.needs_snapshot("btcusdt"));
    }

    #[test]
    fn test_drop_old_events() {
        let mut manager = OrderBookManager::new();
        manager.start_init("btcusdt");

        // Buffer events with u <= snapshot's lastUpdateId
        let event1 = make_depth_event("btcusdt", 90, 95, 89);  // Will be dropped
        let event2 = make_depth_event("btcusdt", 96, 100, 95); // Will be dropped (u == lastUpdateId)
        let event3 = make_depth_event("btcusdt", 98, 105, 100); // Valid: U <= 100, u > 100

        manager.on_depth_update(&event1);
        manager.on_depth_update(&event2);
        manager.on_depth_update(&event3);

        let snapshot = make_snapshot(100);
        let events = manager.on_snapshot("btcusdt", &snapshot);

        // Only snapshot + event3 should be processed
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], DepthEvent::Snapshot { .. }));
        assert!(matches!(events[1], DepthEvent::Update { first_update_id: 98, last_update_id: 105, .. }));
    }

    #[test]
    fn test_continuity_check() {
        let mut manager = OrderBookManager::new();
        manager.start_init("btcusdt");

        // Buffer events
        let event1 = make_depth_event("btcusdt", 100, 105, 99);
        manager.on_depth_update(&event1);

        // Snapshot
        let snapshot = make_snapshot(102);
        manager.on_snapshot("btcusdt", &snapshot);

        // Now synchronized, new event with correct pu
        let event2 = make_depth_event("btcusdt", 106, 110, 105);
        let (should_process, should_request) = manager.on_depth_update(&event2);
        assert!(should_process);
        assert!(!should_request);

        // Event with wrong pu (gap!)
        let event3 = make_depth_event("btcusdt", 115, 120, 112); // pu should be 110
        let (should_process, should_request) = manager.on_depth_update(&event3);
        assert!(!should_process);
        assert!(should_request); // Need to resync
        assert!(manager.needs_snapshot("btcusdt"));
    }

    #[test]
    fn test_gap_in_buffered_events() {
        let mut manager = OrderBookManager::new();
        manager.start_init("btcusdt");

        // Buffer events with a gap
        let event1 = make_depth_event("btcusdt", 100, 105, 99);
        let event2 = make_depth_event("btcusdt", 110, 115, 109); // Gap! pu should be 105

        manager.on_depth_update(&event1);
        manager.on_depth_update(&event2);

        let snapshot = make_snapshot(102);
        let events = manager.on_snapshot("btcusdt", &snapshot);

        // Should process snapshot + event1, then detect gap
        assert_eq!(events.len(), 2);
        assert!(manager.needs_snapshot("btcusdt")); // Marked for resync
    }

    #[test]
    fn test_resync_on_reconnect() {
        let mut manager = OrderBookManager::new();

        // Initialize and sync
        manager.start_init("btcusdt");
        let event1 = make_depth_event("btcusdt", 100, 105, 99);
        manager.on_depth_update(&event1);
        let snapshot = make_snapshot(102);
        manager.on_snapshot("btcusdt", &snapshot);

        assert!(!manager.needs_snapshot("btcusdt"));

        // Simulate reconnect
        manager.mark_all_resync();

        assert!(manager.needs_snapshot("btcusdt"));
        assert!(manager.is_initializing("btcusdt"));
    }

    #[test]
    fn test_snapshot_only_sync() {
        // Test case: snapshot received, but all buffered events are too old
        // The next WebSocket event should be validated as a "first event"
        let mut manager = OrderBookManager::new();
        manager.start_init("btcusdt");

        // Buffer one event that will be dropped (u <= snapshot's lastUpdateId)
        let event1 = make_depth_event("btcusdt", 90, 95, 89);
        manager.on_depth_update(&event1);

        // Snapshot with a higher lastUpdateId
        let snapshot = make_snapshot(100);
        let events = manager.on_snapshot("btcusdt", &snapshot);

        // Only snapshot should be processed (event1 was dropped)
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], DepthEvent::Snapshot { .. }));

        // Should be synchronized but awaiting first valid event
        assert!(!manager.needs_snapshot("btcusdt"));

        // Next event with u <= snapshot should be skipped
        let event2 = make_depth_event("btcusdt", 98, 99, 97);
        let (should_process, should_request) = manager.on_depth_update(&event2);
        assert!(!should_process); // Skipped: u=99 <= snapshot=100
        assert!(!should_request);

        // Event that spans the snapshot (U <= 100, u > 100) is valid as first event
        let event3 = make_depth_event("btcusdt", 98, 105, 99);
        let (should_process, should_request) = manager.on_depth_update(&event3);
        assert!(should_process); // Valid first event
        assert!(!should_request);

        // Now continuation should use pu check
        let event4 = make_depth_event("btcusdt", 106, 110, 105);
        let (should_process, should_request) = manager.on_depth_update(&event4);
        assert!(should_process); // pu=105 matches last u=105
        assert!(!should_request);

        // Gap detection should still work
        let event5 = make_depth_event("btcusdt", 120, 125, 118); // Wrong pu
        let (should_process, should_request) = manager.on_depth_update(&event5);
        assert!(!should_process);
        assert!(should_request); // Gap detected
    }
}
