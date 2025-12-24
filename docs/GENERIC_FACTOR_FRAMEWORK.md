# 通用高频因子计算框架设计

## 核心设计原则

1. **交易所无关**：因子只依赖归一化的内部数据结构
2. **回测/实盘统一**：同一套因子代码适用于回测和实盘
3. **流式计算**：增量更新，O(1) 时间复杂度
4. **状态可序列化**：支持冷/热启动

---

## 内部数据结构（已有）

### 1. Event（统一事件格式）

```rust
/// 64 字节对齐的事件结构
#[repr(C, align(64))]
pub struct Event {
    pub ev: u64,        // 事件类型标志
    pub exch_ts: i64,   // 交易所时间戳 (纳秒)
    pub local_ts: i64,  // 本地时间戳 (纳秒)
    pub px: f64,        // 价格
    pub qty: f64,       // 数量
    pub order_id: u64,  // 订单 ID (L3)
    pub ival: i64,      // 扩展整数字段
    pub fval: f64,      // 扩展浮点字段
}
```

### 事件类型

```rust
// 深度事件
DEPTH_EVENT          // 深度变化
DEPTH_SNAPSHOT_EVENT // 深度快照
DEPTH_BBO_EVENT      // BBO 更新
DEPTH_CLEAR_EVENT    // 清空深度

// 成交事件
TRADE_EVENT          // 成交

// 方向标志
BUY_EVENT            // 买方（bid/买入成交）
SELL_EVENT           // 卖方（ask/卖出成交）

// 处理器标志
LOCAL_EVENT          // 本地处理器事件
EXCH_EVENT           // 交易所处理器事件

// 组合示例
LOCAL_BID_DEPTH_EVENT = DEPTH_EVENT | BUY_EVENT | LOCAL_EVENT
LOCAL_BUY_TRADE_EVENT = TRADE_EVENT | BUY_EVENT | LOCAL_EVENT
```

### 2. MarketDepth（订单簿接口）

```rust
pub trait MarketDepth {
    fn best_bid(&self) -> f64;
    fn best_ask(&self) -> f64;
    fn best_bid_tick(&self) -> i64;
    fn best_ask_tick(&self) -> i64;
    fn best_bid_qty(&self) -> f64;
    fn best_ask_qty(&self) -> f64;
    fn tick_size(&self) -> f64;
    fn lot_size(&self) -> f64;
    fn bid_qty_at_tick(&self, price_tick: i64) -> f64;
    fn ask_qty_at_tick(&self, price_tick: i64) -> f64;
}

pub trait L2MarketDepth {
    fn update_bid_depth(&mut self, price: f64, qty: f64, timestamp: i64)
        -> (price_tick, prev_best, curr_best, prev_qty, curr_qty, ts);
    fn update_ask_depth(&mut self, price: f64, qty: f64, timestamp: i64)
        -> (price_tick, prev_best, curr_best, prev_qty, curr_qty, ts);
    fn clear_depth(&mut self, side: Side, clear_upto_price: f64);
}
```

### 3. HashMapMarketDepth（订单簿实现）

```rust
pub struct HashMapMarketDepth {
    pub tick_size: f64,
    pub lot_size: f64,
    pub timestamp: i64,
    pub bid_depth: HashMap<i64, f64>,  // price_tick -> qty
    pub ask_depth: HashMap<i64, f64>,
    pub best_bid_tick: i64,
    pub best_ask_tick: i64,
}
```

---

## 因子框架设计

### 核心 Trait

```rust
/// 流式因子计算接口
pub trait StreamingFactor: Send + Sync {
    /// 因子唯一 ID
    fn id(&self) -> u16;

    /// 因子名称
    fn name(&self) -> &str;

    /// 处理深度更新事件
    /// 返回：是否产生新的因子值
    fn on_depth_update(
        &mut self,
        side: Side,
        price_tick: i64,
        prev_qty: f64,
        curr_qty: f64,
        timestamp: i64,
        depth: &dyn MarketDepth,
    ) -> bool;

    /// 处理 BBO 变化
    fn on_bbo_change(
        &mut self,
        prev_best_bid: i64,
        curr_best_bid: i64,
        prev_best_ask: i64,
        curr_best_ask: i64,
        timestamp: i64,
        depth: &dyn MarketDepth,
    ) -> bool;

    /// 处理成交事件
    fn on_trade(
        &mut self,
        side: Side,
        price: f64,
        qty: f64,
        timestamp: i64,
    ) -> bool;

    /// 获取当前因子值
    fn value(&self) -> f64;

    /// 因子是否有效（预热完成）
    fn is_valid(&self) -> bool;

    /// 重置因子状态
    fn reset(&mut self);

    /// 序列化状态
    fn serialize_state(&self) -> Vec<u8>;

    /// 反序列化状态
    fn deserialize_state(&mut self, state: &[u8]) -> Result<(), FactorError>;
}
```

### 因子更新频率分类

```rust
pub enum UpdateTrigger {
    /// 任何深度变化都更新
    OnAnyDepthChange,

    /// 仅 BBO 变化时更新
    OnBBOChange,

    /// 仅成交时更新
    OnTrade,

    /// 定时更新
    Periodic { interval_ns: i64 },

    /// 组合触发器
    Combined(Vec<UpdateTrigger>),
}
```

---

## 常用因子实现示例

### 1. Mid Price（中间价）

```rust
pub struct MidPrice {
    value: f64,
    valid: bool,
}

impl StreamingFactor for MidPrice {
    fn on_bbo_change(
        &mut self,
        _prev_bid: i64,
        curr_bid: i64,
        _prev_ask: i64,
        curr_ask: i64,
        _ts: i64,
        depth: &dyn MarketDepth,
    ) -> bool {
        if curr_bid != INVALID_MIN && curr_ask != INVALID_MAX {
            let bid = curr_bid as f64 * depth.tick_size();
            let ask = curr_ask as f64 * depth.tick_size();
            self.value = (bid + ask) / 2.0;
            self.valid = true;
            true
        } else {
            false
        }
    }

    fn value(&self) -> f64 { self.value }
    fn is_valid(&self) -> bool { self.valid }
    // ... 其他方法
}
```

### 2. Spread（买卖价差）

```rust
pub struct Spread {
    value: f64,           // 当前价差
    value_bps: f64,       // 价差 (基点)
    valid: bool,
}

impl StreamingFactor for Spread {
    fn on_bbo_change(
        &mut self,
        _prev_bid: i64,
        curr_bid: i64,
        _prev_ask: i64,
        curr_ask: i64,
        _ts: i64,
        depth: &dyn MarketDepth,
    ) -> bool {
        if curr_bid != INVALID_MIN && curr_ask != INVALID_MAX {
            let tick_size = depth.tick_size();
            let bid = curr_bid as f64 * tick_size;
            let ask = curr_ask as f64 * tick_size;
            self.value = ask - bid;
            self.value_bps = self.value / ((bid + ask) / 2.0) * 10000.0;
            self.valid = true;
            true
        } else {
            false
        }
    }
    // ...
}
```

### 3. Depth Imbalance（深度不平衡）

```rust
pub struct DepthImbalance {
    levels: usize,        // 计算深度档位数
    value: f64,           // [-1, 1] 范围
    valid: bool,
}

impl StreamingFactor for DepthImbalance {
    fn on_depth_update(
        &mut self,
        _side: Side,
        _price_tick: i64,
        _prev_qty: f64,
        _curr_qty: f64,
        _ts: i64,
        depth: &dyn MarketDepth,
    ) -> bool {
        let best_bid = depth.best_bid_tick();
        let best_ask = depth.best_ask_tick();

        if best_bid == INVALID_MIN || best_ask == INVALID_MAX {
            return false;
        }

        let mut bid_sum = 0.0;
        let mut ask_sum = 0.0;

        for i in 0..self.levels as i64 {
            bid_sum += depth.bid_qty_at_tick(best_bid - i);
            ask_sum += depth.ask_qty_at_tick(best_ask + i);
        }

        let total = bid_sum + ask_sum;
        if total > 0.0 {
            self.value = (bid_sum - ask_sum) / total;
            self.valid = true;
            true
        } else {
            false
        }
    }
    // ...
}
```

### 4. Order Flow Imbalance（订单流不平衡）

```rust
pub struct OrderFlowImbalance {
    window_ns: i64,
    buy_trades: VecDeque<(i64, f64)>,   // (timestamp, volume)
    sell_trades: VecDeque<(i64, f64)>,
    value: f64,
    valid: bool,
}

impl StreamingFactor for OrderFlowImbalance {
    fn on_trade(
        &mut self,
        side: Side,
        _price: f64,
        qty: f64,
        timestamp: i64,
    ) -> bool {
        // 添加新成交
        match side {
            Side::Buy => self.buy_trades.push_back((timestamp, qty)),
            Side::Sell => self.sell_trades.push_back((timestamp, qty)),
            _ => return false,
        }

        // 移除过期数据
        let cutoff = timestamp - self.window_ns;
        while self.buy_trades.front().map(|x| x.0 < cutoff).unwrap_or(false) {
            self.buy_trades.pop_front();
        }
        while self.sell_trades.front().map(|x| x.0 < cutoff).unwrap_or(false) {
            self.sell_trades.pop_front();
        }

        // 计算不平衡
        let buy_vol: f64 = self.buy_trades.iter().map(|x| x.1).sum();
        let sell_vol: f64 = self.sell_trades.iter().map(|x| x.1).sum();
        let total = buy_vol + sell_vol;

        if total > 0.0 {
            self.value = (buy_vol - sell_vol) / total;
            self.valid = self.buy_trades.len() + self.sell_trades.len() >= 10;
            true
        } else {
            false
        }
    }
    // ...
}
```

### 5. Micro Price（微观价格）

```rust
pub struct MicroPrice {
    value: f64,
    valid: bool,
}

impl StreamingFactor for MicroPrice {
    fn on_bbo_change(
        &mut self,
        _prev_bid: i64,
        curr_bid: i64,
        _prev_ask: i64,
        curr_ask: i64,
        _ts: i64,
        depth: &dyn MarketDepth,
    ) -> bool {
        if curr_bid == INVALID_MIN || curr_ask == INVALID_MAX {
            return false;
        }

        let tick_size = depth.tick_size();
        let bid_px = curr_bid as f64 * tick_size;
        let ask_px = curr_ask as f64 * tick_size;
        let bid_qty = depth.best_bid_qty();
        let ask_qty = depth.best_ask_qty();

        let total_qty = bid_qty + ask_qty;
        if total_qty > 0.0 {
            // 微观价格 = 按挂单量加权的中间价
            // 买方挂单多 → 价格偏向卖方
            self.value = (bid_px * ask_qty + ask_px * bid_qty) / total_qty;
            self.valid = true;
            true
        } else {
            false
        }
    }
    // ...
}
```

### 6. VWAP（成交量加权平均价）

```rust
pub struct VWAP {
    window_ns: i64,
    trades: VecDeque<(i64, f64, f64)>,  // (ts, price, qty)
    value: f64,
    total_volume: f64,
    valid: bool,
}

impl StreamingFactor for VWAP {
    fn on_trade(
        &mut self,
        _side: Side,
        price: f64,
        qty: f64,
        timestamp: i64,
    ) -> bool {
        self.trades.push_back((timestamp, price, qty));

        // 移除过期
        let cutoff = timestamp - self.window_ns;
        while self.trades.front().map(|x| x.0 < cutoff).unwrap_or(false) {
            self.trades.pop_front();
        }

        // 计算 VWAP
        let mut pv_sum = 0.0;
        let mut v_sum = 0.0;
        for (_, px, vol) in &self.trades {
            pv_sum += px * vol;
            v_sum += vol;
        }

        if v_sum > 0.0 {
            self.value = pv_sum / v_sum;
            self.total_volume = v_sum;
            self.valid = self.trades.len() >= 5;
            true
        } else {
            false
        }
    }
    // ...
}
```

---

## 因子引擎

### 结构设计

```rust
pub struct FactorEngine<MD: MarketDepth + L2MarketDepth> {
    /// 市场深度（本地维护的副本）
    depth: MD,

    /// 注册的因子列表
    factors: Vec<Box<dyn StreamingFactor>>,

    /// 因子 ID → 索引映射
    factor_index: HashMap<u16, usize>,

    /// 当前时间戳
    current_ts: i64,

    /// 预热状态
    warmup_complete: bool,
    warmup_duration_ns: i64,
    warmup_start_ts: i64,
}
```

### 事件处理

```rust
impl<MD: MarketDepth + L2MarketDepth> FactorEngine<MD> {
    /// 处理原始 Event
    pub fn process_event(&mut self, event: &Event) -> Vec<FactorUpdate> {
        let mut updates = Vec::new();
        self.current_ts = event.local_ts;

        if event.is(DEPTH_EVENT | BUY_EVENT) {
            updates.extend(self.handle_bid_depth(event));
        } else if event.is(DEPTH_EVENT | SELL_EVENT) {
            updates.extend(self.handle_ask_depth(event));
        } else if event.is(TRADE_EVENT) {
            updates.extend(self.handle_trade(event));
        } else if event.is(DEPTH_BBO_EVENT) {
            updates.extend(self.handle_bbo(event));
        }

        updates
    }

    fn handle_bid_depth(&mut self, event: &Event) -> Vec<FactorUpdate> {
        let (price_tick, prev_best, curr_best, prev_qty, curr_qty, ts) =
            self.depth.update_bid_depth(event.px, event.qty, event.exch_ts);

        let mut updates = Vec::new();

        // 通知所有因子深度变化
        for factor in &mut self.factors {
            if factor.on_depth_update(
                Side::Buy, price_tick, prev_qty, curr_qty, ts, &self.depth
            ) {
                updates.push(FactorUpdate {
                    factor_id: factor.id(),
                    value: factor.value(),
                    timestamp: ts,
                    valid: factor.is_valid(),
                });
            }
        }

        // 如果 BBO 变化，额外通知
        if prev_best != curr_best {
            for factor in &mut self.factors {
                if factor.on_bbo_change(
                    prev_best, curr_best,
                    self.depth.best_ask_tick(), self.depth.best_ask_tick(),
                    ts, &self.depth
                ) {
                    updates.push(FactorUpdate {
                        factor_id: factor.id(),
                        value: factor.value(),
                        timestamp: ts,
                        valid: factor.is_valid(),
                    });
                }
            }
        }

        updates
    }

    fn handle_trade(&mut self, event: &Event) -> Vec<FactorUpdate> {
        let side = if event.is(BUY_EVENT) { Side::Buy } else { Side::Sell };
        let mut updates = Vec::new();

        for factor in &mut self.factors {
            if factor.on_trade(side, event.px, event.qty, event.exch_ts) {
                updates.push(FactorUpdate {
                    factor_id: factor.id(),
                    value: factor.value(),
                    timestamp: event.exch_ts,
                    valid: factor.is_valid(),
                });
            }
        }

        updates
    }
}
```

---

## 与现有系统集成

### 回测模式

```rust
// 回测时，Event 来自历史数据文件
let data: Data<Event> = read_npz("btcusdt_20231201.npz")?;
let mut engine = FactorEngine::new(HashMapMarketDepth::new(0.1, 0.001));

for event in data.iter() {
    let updates = engine.process_event(event);
    // 使用因子值...
}
```

### 实盘模式

```rust
// 实盘时，Event 来自 LiveBot
impl<CH, MD> Bot<MD> for FactorAwareBot<CH, MD> {
    fn elapse(&mut self, duration: i64) -> Result<ElapseResult, BotError> {
        // LiveBot 内部已将交易所消息转换为 Event
        let result = self.bot.elapse(duration)?;

        // 处理因子
        for event in self.bot.last_events() {
            let updates = self.factor_engine.process_event(event);
            for update in updates {
                self.factor_cache.insert(update.factor_id, update.value);
            }
        }

        Ok(result)
    }
}
```

---

## 数据流转

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                                                                              │
│  ┌──────────────┐     ┌──────────────┐     ┌──────────────────────────────┐ │
│  │   Binance    │     │    Bybit     │     │       Hyperliquid           │ │
│  │   JSON       │     │    JSON      │     │          JSON               │ │
│  └──────┬───────┘     └──────┬───────┘     └──────────────┬───────────────┘ │
│         │                    │                            │                  │
│         └────────────────────┼────────────────────────────┘                  │
│                              │                                               │
│                              ▼                                               │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                    Connector / Collector                               │  │
│  │                    (交易所适配层)                                       │  │
│  │                                                                        │  │
│  │    解析 JSON → 生成 Event { ev, exch_ts, local_ts, px, qty, ... }     │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                              │                                               │
│                              ▼                                               │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                         Event (归一化)                                 │  │
│  │    64 bytes: ev | exch_ts | local_ts | px | qty | order_id | ...     │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                              │                                               │
│              ┌───────────────┴───────────────┐                               │
│              │                               │                               │
│              ▼                               ▼                               │
│  ┌──────────────────────┐       ┌──────────────────────────────────────┐   │
│  │   MarketDepth        │       │       FactorEngine                    │   │
│  │   (订单簿状态)        │◄──────│       (因子计算)                       │   │
│  │                      │       │                                       │   │
│  │   • best_bid/ask     │       │   • on_depth_update()                 │   │
│  │   • bid_qty_at_tick  │       │   • on_trade()                        │   │
│  │   • ask_qty_at_tick  │       │   • on_bbo_change()                   │   │
│  └──────────────────────┘       └───────────────────┬──────────────────┘   │
│                                                      │                       │
│                                                      ▼                       │
│                                         ┌────────────────────────┐          │
│                                         │    FactorUpdate        │          │
│                                         │    { id, value, ts }   │          │
│                                         └────────────────────────┘          │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 因子状态序列化

```rust
/// 因子引擎状态快照
#[derive(Serialize, Deserialize)]
pub struct FactorEngineSnapshot {
    pub timestamp: i64,
    pub depth_snapshot: DepthSnapshot,
    pub factor_states: HashMap<u16, Vec<u8>>,
    pub checksum: u64,
}

#[derive(Serialize, Deserialize)]
pub struct DepthSnapshot {
    pub tick_size: f64,
    pub lot_size: f64,
    pub best_bid_tick: i64,
    pub best_ask_tick: i64,
    pub bid_depth: Vec<(i64, f64)>,
    pub ask_depth: Vec<(i64, f64)>,
}

impl<MD> FactorEngine<MD> {
    pub fn snapshot(&self) -> FactorEngineSnapshot {
        let mut factor_states = HashMap::new();
        for factor in &self.factors {
            factor_states.insert(factor.id(), factor.serialize_state());
        }

        FactorEngineSnapshot {
            timestamp: self.current_ts,
            depth_snapshot: self.depth.to_snapshot(),
            factor_states,
            checksum: 0, // 计算校验和
        }
    }

    pub fn restore(&mut self, snapshot: FactorEngineSnapshot) -> Result<(), FactorError> {
        self.depth.restore_from_snapshot(&snapshot.depth_snapshot);

        for factor in &mut self.factors {
            if let Some(state) = snapshot.factor_states.get(&factor.id()) {
                factor.deserialize_state(state)?;
            }
        }

        self.current_ts = snapshot.timestamp;
        Ok(())
    }
}
```

---

## 多资产因子支持

### 单资产 vs 多资产因子

```rust
/// 多资产因子引擎
pub struct MultiAssetFactorEngine<MD: MarketDepth + L2MarketDepth> {
    /// 每个资产的深度
    depths: Vec<MD>,

    /// 单资产因子 (per asset)
    single_asset_factors: Vec<Vec<Box<dyn StreamingFactor>>>,

    /// 跨资产因子
    cross_asset_factors: Vec<Box<dyn CrossAssetFactor>>,

    /// 资产名称映射
    asset_index: HashMap<String, usize>,
}

/// 跨资产因子接口
pub trait CrossAssetFactor: Send + Sync {
    fn id(&self) -> u16;
    fn name(&self) -> &str;

    /// 某资产的因子更新时触发
    fn on_factor_update(
        &mut self,
        asset_no: usize,
        factor_id: u16,
        value: f64,
        timestamp: i64,
        all_depths: &[&dyn MarketDepth],
    ) -> bool;

    fn value(&self) -> f64;
    fn is_valid(&self) -> bool;
}
```

### 跨资产因子示例：价差因子

```rust
/// BTC-ETH 价格相关性
pub struct PairSpread {
    asset_a: usize,          // e.g., BTCUSDT
    asset_b: usize,          // e.g., ETHUSDT
    ratio: f64,              // 标准比例 (e.g., 15.0 for BTC/ETH)
    mid_a: f64,
    mid_b: f64,
    value: f64,              // 当前价差偏离
    valid: bool,
}

impl CrossAssetFactor for PairSpread {
    fn on_factor_update(
        &mut self,
        asset_no: usize,
        factor_id: u16,
        value: f64,
        _ts: i64,
        _depths: &[&dyn MarketDepth],
    ) -> bool {
        // 假设 factor_id = 1 是 MidPrice
        if factor_id != 1 {
            return false;
        }

        if asset_no == self.asset_a {
            self.mid_a = value;
        } else if asset_no == self.asset_b {
            self.mid_b = value;
        } else {
            return false;
        }

        if self.mid_a > 0.0 && self.mid_b > 0.0 {
            let expected_ratio = self.mid_a / self.mid_b;
            self.value = (expected_ratio - self.ratio) / self.ratio;
            self.valid = true;
            true
        } else {
            false
        }
    }

    fn value(&self) -> f64 { self.value }
    fn is_valid(&self) -> bool { self.valid }
}
```

---

## Python 集成

### FFI 导出

```rust
// py-hftbacktest/src/factor.rs

use hftbacktest::factor::{FactorEngine, StreamingFactor, MidPrice, Spread, DepthImbalance};

/// 创建因子引擎
#[unsafe(no_mangle)]
pub extern "C" fn factor_engine_new(
    tick_size: f64,
    lot_size: f64,
) -> *mut FactorEngine<HashMapMarketDepth> {
    let depth = HashMapMarketDepth::new(tick_size, lot_size);
    let engine = FactorEngine::new(depth);
    Box::into_raw(Box::new(engine))
}

/// 注册因子
#[unsafe(no_mangle)]
pub extern "C" fn factor_engine_register(
    engine_ptr: *mut FactorEngine<HashMapMarketDepth>,
    factor_type: u16,
    config_json: *const c_char,
) -> i64 {
    let engine = unsafe { &mut *engine_ptr };
    let config = unsafe { CStr::from_ptr(config_json).to_str().unwrap() };

    let factor: Box<dyn StreamingFactor> = match factor_type {
        1 => Box::new(MidPrice::new()),
        2 => Box::new(Spread::new()),
        3 => {
            let cfg: DepthImbalanceConfig = serde_json::from_str(config).unwrap();
            Box::new(DepthImbalance::new(cfg.levels))
        }
        // ...
        _ => return -1,
    };

    engine.register(factor);
    0
}

/// 处理事件
#[unsafe(no_mangle)]
pub extern "C" fn factor_engine_process(
    engine_ptr: *mut FactorEngine<HashMapMarketDepth>,
    event_ptr: *const Event,
    updates_out: *mut FactorUpdateFFI,
    max_updates: usize,
) -> i64 {
    let engine = unsafe { &mut *engine_ptr };
    let event = unsafe { &*event_ptr };

    let updates = engine.process_event(event);
    let count = updates.len().min(max_updates);

    for (i, update) in updates.iter().take(count).enumerate() {
        unsafe {
            (*updates_out.add(i)) = FactorUpdateFFI {
                factor_id: update.factor_id,
                value: update.value,
                timestamp: update.timestamp,
                valid: update.valid as u8,
            };
        }
    }

    count as i64
}

/// 获取因子值
#[unsafe(no_mangle)]
pub extern "C" fn factor_engine_get_value(
    engine_ptr: *mut FactorEngine<HashMapMarketDepth>,
    factor_id: u16,
) -> f64 {
    let engine = unsafe { &*engine_ptr };
    engine.get_factor(factor_id).map(|f| f.value()).unwrap_or(f64::NAN)
}

/// 获取所有因子值（批量）
#[unsafe(no_mangle)]
pub extern "C" fn factor_engine_get_all_values(
    engine_ptr: *mut FactorEngine<HashMapMarketDepth>,
    values_out: *mut f64,
    count: usize,
) -> i64 {
    let engine = unsafe { &*engine_ptr };
    let factors = engine.factors();

    for (i, factor) in factors.iter().take(count).enumerate() {
        unsafe {
            *values_out.add(i) = if factor.is_valid() {
                factor.value()
            } else {
                f64::NAN
            };
        }
    }

    factors.len().min(count) as i64
}
```

### Python 绑定

```python
# hftbacktest/factor.py

import numpy as np
from numba import njit, carray
from numba.types import float64, int64, uint16
import ctypes

# FFI 函数声明
_LIB = ctypes.CDLL("libhftbacktest.so")

_factor_engine_new = _LIB.factor_engine_new
_factor_engine_new.argtypes = [ctypes.c_double, ctypes.c_double]
_factor_engine_new.restype = ctypes.c_void_p

_factor_engine_register = _LIB.factor_engine_register
_factor_engine_register.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_char_p]
_factor_engine_register.restype = ctypes.c_int64

_factor_engine_process = _LIB.factor_engine_process
_factor_engine_process.argtypes = [
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.c_size_t
]
_factor_engine_process.restype = ctypes.c_int64

_factor_engine_get_value = _LIB.factor_engine_get_value
_factor_engine_get_value.argtypes = [ctypes.c_void_p, ctypes.c_uint16]
_factor_engine_get_value.restype = ctypes.c_double

_factor_engine_get_all_values = _LIB.factor_engine_get_all_values
_factor_engine_get_all_values.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t]
_factor_engine_get_all_values.restype = ctypes.c_int64


class FactorEngine:
    """因子引擎 Python 封装"""

    # 因子类型常量
    MID_PRICE = 1
    SPREAD = 2
    DEPTH_IMBALANCE = 3
    ORDER_FLOW_IMBALANCE = 4
    MICRO_PRICE = 5
    VWAP = 6

    def __init__(self, tick_size: float, lot_size: float):
        self._ptr = _factor_engine_new(tick_size, lot_size)
        self._factor_ids = []
        self._values_buffer = np.zeros(64, dtype=np.float64)

    def register(self, factor_type: int, config: dict = None) -> int:
        """注册因子"""
        config_json = json.dumps(config or {}).encode()
        result = _factor_engine_register(self._ptr, factor_type, config_json)
        if result >= 0:
            self._factor_ids.append(factor_type)
        return result

    def get_value(self, factor_id: int) -> float:
        """获取单个因子值"""
        return _factor_engine_get_value(self._ptr, factor_id)

    def get_all_values(self) -> np.ndarray:
        """获取所有因子值"""
        count = _factor_engine_get_all_values(
            self._ptr,
            self._values_buffer.ctypes.data,
            len(self._values_buffer)
        )
        return self._values_buffer[:count].copy()

    @property
    def ptr(self) -> int:
        """返回引擎指针（用于 njit 函数）"""
        return self._ptr


# Numba 兼容的 FFI 调用（用于 njit 策略内部）
@njit
def get_factor_value(engine_ptr: int, factor_id: int) -> float64:
    """在 njit 函数内获取因子值"""
    # 通过 cffi 调用
    return _cffi_get_factor_value(engine_ptr, factor_id)
```

### 与策略集成示例

```python
from numba import njit
import numpy as np
from hftbacktest import BacktestAsset, HashMapMarketDepthBacktest
from hftbacktest.factor import FactorEngine

@njit
def strategy(hbt, factor_values):
    """
    带因子的交易策略

    factor_values: numpy array [mid_price, spread, depth_imbalance, ...]
    """
    mid_price = factor_values[0]
    spread = factor_values[1]
    depth_imbalance = factor_values[2]

    # 基于因子的信号
    if depth_imbalance > 0.3 and spread < 0.001:
        # 买入信号
        pass

    return True


def run_backtest():
    # 创建回测实例
    hbt = HashMapMarketDepthBacktest([
        BacktestAsset(
            data="btcusdt_20231201.npz",
            tick_size=0.1,
            lot_size=0.001,
            # ...
        )
    ])

    # 创建因子引擎
    factor_engine = FactorEngine(tick_size=0.1, lot_size=0.001)
    factor_engine.register(FactorEngine.MID_PRICE)
    factor_engine.register(FactorEngine.SPREAD)
    factor_engine.register(FactorEngine.DEPTH_IMBALANCE, {"levels": 5})

    # 运行回测
    while hbt.elapse(1_000_000) == 0:  # 1ms
        # 获取因子值
        factor_values = factor_engine.get_all_values()

        # 运行策略
        strategy(hbt.ptr, factor_values)
```

---

## 性能优化

### 1. 因子计算批处理

```rust
impl<MD: MarketDepth + L2MarketDepth> FactorEngine<MD> {
    /// 批量处理事件（减少函数调用开销）
    pub fn process_events_batch(
        &mut self,
        events: &[Event],
        updates: &mut Vec<FactorUpdate>,
    ) {
        updates.clear();

        for event in events {
            self.current_ts = event.local_ts;

            // 直接内联处理，避免临时 Vec
            if event.is(DEPTH_EVENT) {
                self.handle_depth_inline(event, updates);
            } else if event.is(TRADE_EVENT) {
                self.handle_trade_inline(event, updates);
            }
        }
    }
}
```

### 2. 因子更新条件过滤

```rust
/// 因子更新条件
pub struct FactorFilter {
    /// 最小更新间隔 (纳秒)
    min_update_interval: i64,

    /// 最小变化阈值
    min_change_threshold: f64,

    /// 上次更新时间
    last_update_ts: i64,

    /// 上次值
    last_value: f64,
}

impl<MD: MarketDepth + L2MarketDepth> FactorEngine<MD> {
    /// 带过滤的因子更新
    fn maybe_emit_update(
        &self,
        factor: &dyn StreamingFactor,
        filter: &mut FactorFilter,
        timestamp: i64,
    ) -> Option<FactorUpdate> {
        let value = factor.value();

        // 时间过滤
        if timestamp - filter.last_update_ts < filter.min_update_interval {
            return None;
        }

        // 变化量过滤
        let change = (value - filter.last_value).abs();
        if change < filter.min_change_threshold {
            return None;
        }

        filter.last_update_ts = timestamp;
        filter.last_value = value;

        Some(FactorUpdate {
            factor_id: factor.id(),
            value,
            timestamp,
            valid: factor.is_valid(),
        })
    }
}
```

### 3. 内存预分配

```rust
pub struct FactorEngine<MD: MarketDepth + L2MarketDepth> {
    // ...

    /// 预分配的更新缓冲区
    update_buffer: Vec<FactorUpdate>,

    /// 预分配的临时计算缓冲区
    calc_buffer: Vec<f64>,
}

impl<MD: MarketDepth + L2MarketDepth> FactorEngine<MD> {
    pub fn new_with_capacity(depth: MD, factor_capacity: usize) -> Self {
        Self {
            depth,
            factors: Vec::with_capacity(factor_capacity),
            factor_index: HashMap::with_capacity(factor_capacity),
            update_buffer: Vec::with_capacity(factor_capacity * 2),
            calc_buffer: Vec::with_capacity(1024),
            // ...
        }
    }
}
```

### 4. SIMD 优化（深度遍历）

```rust
use std::simd::{f64x4, SimdFloat};

impl DepthImbalance {
    /// SIMD 加速的深度求和
    fn sum_depth_simd(&self, depth: &dyn MarketDepth, start_tick: i64, levels: usize) -> f64 {
        let mut sum = f64x4::splat(0.0);
        let chunks = levels / 4;

        for i in 0..chunks {
            let offset = (i * 4) as i64;
            let qtys = f64x4::from_array([
                depth.bid_qty_at_tick(start_tick - offset),
                depth.bid_qty_at_tick(start_tick - offset - 1),
                depth.bid_qty_at_tick(start_tick - offset - 2),
                depth.bid_qty_at_tick(start_tick - offset - 3),
            ]);
            sum += qtys;
        }

        // 处理余数
        let mut remainder = 0.0;
        for i in (chunks * 4)..levels {
            remainder += depth.bid_qty_at_tick(start_tick - i as i64);
        }

        sum.reduce_sum() + remainder
    }
}
```

---

## 因子测试框架

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 因子测试辅助
    struct FactorTester<F: StreamingFactor> {
        factor: F,
        depth: HashMapMarketDepth,
    }

    impl<F: StreamingFactor> FactorTester<F> {
        fn new(factor: F, tick_size: f64) -> Self {
            Self {
                factor,
                depth: HashMapMarketDepth::new(tick_size, 0.001),
            }
        }

        /// 设置订单簿状态
        fn set_book(&mut self, bids: &[(f64, f64)], asks: &[(f64, f64)]) {
            self.depth.clear();
            for (px, qty) in bids {
                self.depth.update_bid_depth(*px, *qty, 0);
            }
            for (px, qty) in asks {
                self.depth.update_ask_depth(*px, *qty, 0);
            }
        }

        /// 触发 BBO 更新
        fn trigger_bbo(&mut self) -> bool {
            self.factor.on_bbo_change(
                self.depth.best_bid_tick() - 1,
                self.depth.best_bid_tick(),
                self.depth.best_ask_tick() + 1,
                self.depth.best_ask_tick(),
                0,
                &self.depth,
            )
        }

        fn value(&self) -> f64 {
            self.factor.value()
        }
    }

    #[test]
    fn test_mid_price() {
        let mut tester = FactorTester::new(MidPrice::new(), 0.1);

        // 设置订单簿: bid=100.0, ask=100.2
        tester.set_book(
            &[(100.0, 10.0)],
            &[(100.2, 10.0)],
        );

        tester.trigger_bbo();

        // mid = (100.0 + 100.2) / 2 = 100.1
        assert!((tester.value() - 100.1).abs() < 1e-9);
    }

    #[test]
    fn test_depth_imbalance() {
        let mut tester = FactorTester::new(DepthImbalance::new(3), 0.1);

        // bid 总量 = 30, ask 总量 = 10
        tester.set_book(
            &[(100.0, 10.0), (99.9, 10.0), (99.8, 10.0)],
            &[(100.1, 5.0), (100.2, 3.0), (100.3, 2.0)],
        );

        tester.factor.on_depth_update(
            Side::Buy, 1000, 0.0, 10.0, 0, &tester.depth
        );

        // imbalance = (30 - 10) / (30 + 10) = 0.5
        assert!((tester.value() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_order_flow_imbalance() {
        let mut factor = OrderFlowImbalance::new(1_000_000_000); // 1s window

        // 买入成交 100
        factor.on_trade(Side::Buy, 100.0, 100.0, 1_000_000);
        // 卖出成交 50
        factor.on_trade(Side::Sell, 99.9, 50.0, 2_000_000);

        // OFI = (100 - 50) / (100 + 50) = 0.333...
        assert!((factor.value() - 0.333333).abs() < 0.001);
    }
}
```

---

## 下一步实现计划

1. **Phase 1: 核心框架**
   - [ ] 在 `hftbacktest/src/` 下创建 `factor/` 模块
   - [ ] 实现 `StreamingFactor` trait
   - [ ] 实现基础因子 (MidPrice, Spread, DepthImbalance)
   - [ ] 实现 `FactorEngine`

2. **Phase 2: Python 集成**
   - [ ] 在 `py-hftbacktest/src/` 添加 FFI 导出
   - [ ] 创建 `hftbacktest/factor.py` Python 封装
   - [ ] 编写使用示例

3. **Phase 3: 高级因子**
   - [ ] OrderFlowImbalance
   - [ ] MicroPrice
   - [ ] VWAP
   - [ ] 自定义因子宏/DSL

4. **Phase 4: 优化与测试**
   - [ ] 性能基准测试
   - [ ] SIMD 优化
   - [ ] 完整测试覆盖

---

## 总结

| 层级 | 职责 | 数据格式 |
|------|------|---------|
| **交易所层** | 原始数据接收 | JSON (各交易所格式) |
| **适配层** (Connector/Collector) | 归一化 | Event (64 bytes) |
| **订单簿层** | 状态维护 | MarketDepth trait |
| **因子层** | 因子计算 | StreamingFactor trait |
| **策略层** | 决策 | factor.value() |

**关键点**：
1. 因子只依赖 `Event` 和 `MarketDepth`，与交易所无关
2. 同一份因子代码适用于回测和实盘
3. 每个因子负责自己的状态管理和序列化
4. Python 通过 FFI 调用 Rust 因子引擎，保持高性能
5. 支持单资产和跨资产因子
