# 高频因子计算框架设计方案

基于 HftBacktest 系统构建高频因子计算框架的设计文档。

## 目录

1. [现有基础设施分析](#现有基础设施分析)
2. [需求分析](#需求分析)
3. [架构设计](#架构设计)
4. [因子实现方案对比](#因子实现方案对比)
5. [数据结构设计](#数据结构设计)
6. [存储方案](#存储方案)
7. [IPC 因子同步机制](#ipc-因子同步机制)
8. [冷启动与热启动](#冷启动与热启动)
9. [实现路径建议](#实现路径建议)

---

## 现有基础设施分析

### Connector vs Collector

HftBacktest 提供两个独立的数据组件：

| 组件 | 用途 | 数据流向 | 输出格式 |
|------|------|---------|---------|
| **Connector** | 实盘交易 | 交易所 → IPC → Bot | IceOryx2 (LiveEvent) |
| **Collector** | 历史数据采集 | 交易所 → 文件 | gzip JSON 文本 |

```
                    ┌─────────────────────────────────────────────────┐
                    │                   交易所                         │
                    └──────────────┬──────────────────┬───────────────┘
                                   │                  │
              ┌────────────────────▼──────┐    ┌──────▼────────────────────┐
              │        Connector          │    │       Collector           │
              │  (实盘交易 + 行情分发)      │    │  (历史数据采集)            │
              │                           │    │                           │
              │  • IceOryx2 IPC 输出      │    │  • gzip 文件输出          │
              │  • 解析为结构化 LiveEvent  │    │  • 原始 JSON 保留         │
              │  • 支持订单路由           │    │  • 按日期自动轮转          │
              └───────────────────────────┘    └───────────────────────────┘
```

### Connector 支持的数据结构

**位置**: `connector/src/{exchange}/msg/stream.rs`

#### Binance Futures 数据类型

```rust
pub enum EventStream {
    // 深度更新
    DepthUpdate(Depth),      // L2 增量深度

    // 成交
    Trade(Trade),            // 公开成交
    TradeLite(TradeLite),    // 用户成交（轻量）

    // 订单更新
    OrderTradeUpdate(OrderTradeUpdate),

    // 账户更新
    AccountUpdate(AccountUpdate),  // 余额 + 持仓

    // 连接管理
    ListenKeyExpired(ListenKeyStream),
}

// 深度数据
pub struct Depth {
    pub transaction_time: i64,      // 交易所时间
    pub event_time: i64,            // 事件时间
    pub symbol: String,             // 交易对
    pub first_update_id: i64,       // 首个更新 ID
    pub last_update_id: i64,        // 末个更新 ID
    pub prev_update_id: i64,        // 前一更新 ID（用于连续性检查）
    pub bids: Vec<(String, String)>,  // [(price, qty), ...]
    pub asks: Vec<(String, String)>,
}

// 成交数据
pub struct Trade {
    pub transaction_time: i64,
    pub symbol: String,
    pub id: i64,                    // 成交 ID
    pub price: String,
    pub qty: String,
    pub type_: String,              // MARKET/LIMIT
    pub is_the_buyer_the_market_maker: bool,  // 方向判断
}

// 账户持仓
pub struct Position {
    pub symbol: String,
    pub position_amount: f64,       // 持仓数量
    pub entry_price: f64,           // 开仓均价
    pub unrealized_pnl: String,     // 未实现盈亏
    pub margin_type: String,        // 保证金模式
}
```

#### 订阅的 WebSocket 流

```rust
// Binance Futures (connector)
"$symbol@trade"          // 逐笔成交
"$symbol@depth@0ms"      // 深度增量（最快更新）
"$symbol@bookTicker"     // BBO（仅 Collector）

// Bybit
"orderbook.1.$symbol"    // L1 深度
"orderbook.50.$symbol"   // L2 深度 (50档)
"orderbook.500.$symbol"  // L2 深度 (500档)
"publicTrade.$symbol"    // 逐笔成交

// Hyperliquid
"trades"                 // 成交
"l2Book"                 // L2 深度
"bbo"                    // BBO
```

### Collector 存储格式

**位置**: `collector/src/file.rs`

```
{path}/{symbol}_YYYYMMDD.gz
```

**文件格式**:
```
{timestamp_nanos} {raw_json}\n
{timestamp_nanos} {raw_json}\n
...
```

**示例**:
```
1703145600000000000 {"e":"depthUpdate","E":1703145600001,"T":1703145600000,"s":"BTCUSDT",...}
1703145600001000000 {"e":"trade","E":1703145600001,"T":1703145600001,"s":"BTCUSDT",...}
```

### Collector 支持的交易所

| 交易所 | 命令参数 | 订阅流 |
|-------|---------|-------|
| Binance Spot | `binance` / `binancespot` | trade, bookTicker, depth@100ms |
| Binance USD-M Futures | `binancefutures` / `binancefuturesum` | trade, bookTicker, depth@0ms |
| Binance COIN-M Futures | `binancefuturescm` | trade, bookTicker, depth@0ms |
| Bybit Linear | `bybit` | orderbook.1/50/500, publicTrade |
| Hyperliquid | `hyperliquid` | trades, l2Book, bbo |

**使用示例**:
```bash
# 采集 Binance Futures 数据
./target/release/collector ./data binancefutures btcusdt ethusdt

# 采集 Bybit 数据
./target/release/collector ./data bybit BTCUSDT ETHUSDT

# 采集 Hyperliquid 数据
./target/release/collector ./data hyperliquid BTC ETH
```

---

## 需求分析

### 核心需求

1. **流式因子计算**：实时处理 tick 级别的行情数据，计算高频因子
2. **低延迟**：因子计算和传输需要微秒级延迟
3. **因子与策略解耦**：因子计算独立进程，策略异步接收因子更新
4. **状态持久化**：支持冷启动（从头计算）和热启动（恢复状态继续）

### 现有资源

- HftBacktest 的 IceOryx2 IPC 基础设施
- MarketDepth 数据结构和 Event 类型定义
- Python/Numba 流式计算方案（已有）
- Connector 的 WebSocket 行情接入

---

## 架构设计

### 整体架构

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              交易所 WebSocket                                │
└───────────────────────────────────┬─────────────────────────────────────────┘
                                    │
┌───────────────────────────────────▼─────────────────────────────────────────┐
│                           Connector 进程                                     │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────────┐  │
│  │ market_data_ws  │  │ OrderManager    │  │ FusedHashMapMarketDepth    │  │
│  └────────┬────────┘  └─────────────────┘  └─────────────────────────────┘  │
│           │                                                                  │
│           ▼                                                                  │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                    IceOryx2 Publisher                                │    │
│  │  • {name}/ToBot (LiveEvent)                                          │    │
│  │  • {name}/ToFactor (LiveEvent) ← 新增                                │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
                    │                               │
        IceOryx2 IPC│                               │IceOryx2 IPC
                    │                               │
┌───────────────────▼───────────────┐ ┌─────────────▼─────────────────────────┐
│        Factor Engine 进程          │ │           Strategy Bot 进程            │
│  ┌─────────────────────────────┐  │ │  ┌─────────────────────────────────┐  │
│  │   FactorComputer (Rust)     │  │ │  │  LiveBot<MD> + FactorReceiver   │  │
│  │   • 接收 LiveEvent          │  │ │  │  • 接收 LiveEvent               │  │
│  │   • 维护 MarketDepth 副本    │  │ │  │  • 接收 FactorEvent (异步)      │  │
│  │   • 流式计算因子             │  │ │  │  • 策略决策                     │  │
│  └──────────────┬──────────────┘  │ │  └─────────────────────────────────┘  │
│                 │                  │ │                 ▲                     │
│                 ▼                  │ │                 │                     │
│  ┌─────────────────────────────┐  │ │                 │                     │
│  │    IceOryx2 Publisher       │──┼─┼─────────────────┘                     │
│  │    {name}/Factor            │  │ │          IceOryx2 IPC                 │
│  └─────────────────────────────┘  │ │                                       │
│                 │                  │ └───────────────────────────────────────┘
│                 ▼                  │
│  ┌─────────────────────────────┐  │
│  │    State Persistence        │  │
│  │    • 因子状态快照            │  │
│  │    • 冷/热启动恢复           │  │
│  └─────────────────────────────┘  │
└────────────────────────────────────┘
```

### 进程职责

| 进程 | 职责 |
|------|------|
| **Connector** | 交易所连接、行情分发、订单路由 |
| **Factor Engine** | 接收行情、维护状态、计算因子、发布因子事件 |
| **Strategy Bot** | 接收行情+因子、策略决策、订单提交 |

---

## 因子实现方案对比

### 方案 A: 纯 Rust 实现

```rust
pub trait StreamingFactor: Send + Sync {
    /// 因子名称
    fn name(&self) -> &str;

    /// 处理深度更新事件
    fn on_depth_update(&mut self, event: &Event, depth: &impl MarketDepth) -> Option<f64>;

    /// 处理成交事件
    fn on_trade(&mut self, event: &Event) -> Option<f64>;

    /// 获取当前因子值
    fn value(&self) -> f64;

    /// 序列化状态用于持久化
    fn serialize_state(&self) -> Vec<u8>;

    /// 反序列化状态用于恢复
    fn deserialize_state(&mut self, state: &[u8]) -> Result<(), FactorError>;
}

// 示例：订单流不平衡因子
pub struct OrderFlowImbalance {
    window_ns: i64,           // 时间窗口（纳秒）
    buy_volume: VecDeque<(i64, f64)>,   // (timestamp, volume)
    sell_volume: VecDeque<(i64, f64)>,
    current_value: f64,
}

impl StreamingFactor for OrderFlowImbalance {
    fn on_trade(&mut self, event: &Event) -> Option<f64> {
        let ts = event.exch_ts;
        let qty = event.qty;

        // 添加新数据
        if event.is(BUY_EVENT) {
            self.buy_volume.push_back((ts, qty));
        } else {
            self.sell_volume.push_back((ts, qty));
        }

        // 移除过期数据
        let cutoff = ts - self.window_ns;
        while self.buy_volume.front().map(|x| x.0 < cutoff).unwrap_or(false) {
            self.buy_volume.pop_front();
        }
        while self.sell_volume.front().map(|x| x.0 < cutoff).unwrap_or(false) {
            self.sell_volume.pop_front();
        }

        // 计算不平衡
        let buy_sum: f64 = self.buy_volume.iter().map(|x| x.1).sum();
        let sell_sum: f64 = self.sell_volume.iter().map(|x| x.1).sum();
        let total = buy_sum + sell_sum;

        if total > 0.0 {
            self.current_value = (buy_sum - sell_sum) / total;
            Some(self.current_value)
        } else {
            None
        }
    }
    // ...
}
```

**优点**：
- 最高性能，零 GC
- 类型安全，编译期检查
- 与 HftBacktest 类型系统无缝集成
- 便于状态序列化

**缺点**：
- 开发速度较慢
- 需要重新编译才能修改因子逻辑

### 方案 B: Python/Numba 实现 + Rust IPC 桥接

```python
from numba import njit
from numba.typed import List
import numpy as np

@njit
def order_flow_imbalance(
    event_ts: int64,
    event_side: int64,
    event_qty: float64,
    buy_ts: List,      # 状态：买方时间戳
    buy_vol: List,     # 状态：买方成交量
    sell_ts: List,     # 状态：卖方时间戳
    sell_vol: List,    # 状态：卖方成交量
    window_ns: int64,
) -> float64:
    # 添加新数据
    if event_side == BUY:
        buy_ts.append(event_ts)
        buy_vol.append(event_qty)
    else:
        sell_ts.append(event_ts)
        sell_vol.append(event_qty)

    # 移除过期数据
    cutoff = event_ts - window_ns
    while len(buy_ts) > 0 and buy_ts[0] < cutoff:
        buy_ts.pop(0)
        buy_vol.pop(0)
    while len(sell_ts) > 0 and sell_ts[0] < cutoff:
        sell_ts.pop(0)
        sell_vol.pop(0)

    # 计算
    buy_sum = 0.0
    for v in buy_vol:
        buy_sum += v
    sell_sum = 0.0
    for v in sell_vol:
        sell_sum += v

    total = buy_sum + sell_sum
    if total > 0:
        return (buy_sum - sell_sum) / total
    return 0.0
```

**Rust IPC 桥接层**：

```rust
// Python Factor Engine 包装器
pub struct PyFactorBridge {
    py_runtime: PyO3Runtime,
    factor_module: PyObject,
    state: PyObject,
}

impl PyFactorBridge {
    pub fn compute(&mut self, event: &Event) -> Vec<(String, f64)> {
        // 调用 Python 因子计算
        Python::with_gil(|py| {
            let result = self.factor_module
                .call_method1(py, "compute", (event_to_py(event),))
                .unwrap();
            // 解析结果
        })
    }
}
```

**优点**：
- 开发迭代快
- 可复用现有 Numba 因子库
- 运行时修改因子（无需重新编译）

**缺点**：
- GIL 限制并行
- Python ↔ Rust 边界有开销
- 状态序列化复杂

### 方案 C: 混合架构（推荐）

```
┌─────────────────────────────────────────────────────────────────┐
│                     Factor Engine (Rust)                         │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  核心基础设施 (Rust)                                       │  │
│  │  • IceOryx2 IPC 收发                                       │  │
│  │  • MarketDepth 维护                                        │  │
│  │  • 状态持久化                                              │  │
│  │  • 因子事件发布                                            │  │
│  └───────────────────────────────────────────────────────────┘  │
│                              │                                   │
│                              ▼                                   │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  因子计算层                                                │  │
│  │  ┌─────────────────┐  ┌─────────────────────────────────┐ │  │
│  │  │ Rust 因子       │  │ 动态加载因子 (WASM/Lua/Python)  │ │  │
│  │  │ (高性能核心)    │  │ (快速迭代实验)                  │ │  │
│  │  └─────────────────┘  └─────────────────────────────────┘ │  │
│  └───────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

**策略**：
1. **核心因子用 Rust**：如 VWAP、OFI、微观结构因子
2. **实验因子用动态语言**：快速迭代，验证后移植到 Rust
3. **基础设施统一用 Rust**：IPC、状态管理、序列化

---

## 数据结构设计

### 因子事件定义

```rust
/// 因子更新事件
#[derive(Clone, Debug, Encode, Decode)]
pub struct FactorEvent {
    /// 事件时间戳（纳秒）
    pub timestamp: i64,
    /// 因子 ID（预定义映射）
    pub factor_id: u16,
    /// 因子值
    pub value: f64,
    /// 因子置信度（可选）
    pub confidence: f64,
    /// 关联的交易对
    pub symbol: String,
}

/// 因子快照（用于批量更新）
#[derive(Clone, Debug, Encode, Decode)]
pub struct FactorSnapshot {
    pub timestamp: i64,
    pub symbol: String,
    pub factors: Vec<(u16, f64)>,  // (factor_id, value)
}

/// 因子注册表（因子 ID → 因子名称映射）
#[derive(Clone, Debug, Encode, Decode)]
pub struct FactorRegistry {
    pub factors: HashMap<u16, FactorMeta>,
}

#[derive(Clone, Debug, Encode, Decode)]
pub struct FactorMeta {
    pub id: u16,
    pub name: String,
    pub description: String,
    pub update_frequency: UpdateFrequency,
}

#[derive(Clone, Copy, Debug, Encode, Decode)]
pub enum UpdateFrequency {
    OnTrade,        // 每笔成交更新
    OnDepthChange,  // 深度变化时更新
    OnBBOChange,    // BBO 变化时更新
    Periodic(u64),  // 定时更新（毫秒间隔）
}
```

### 因子计算器状态

```rust
/// 因子计算器状态（用于持久化）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FactorEngineState {
    /// 状态快照时间戳
    pub snapshot_ts: i64,

    /// 各因子的内部状态
    pub factor_states: HashMap<u16, Vec<u8>>,

    /// 市场深度快照
    pub depth_snapshot: DepthSnapshot,

    /// 最近 N 笔成交（用于恢复滑动窗口因子）
    pub recent_trades: Vec<Event>,

    /// 校验和（确保状态完整性）
    pub checksum: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DepthSnapshot {
    pub symbol: String,
    pub timestamp: i64,
    pub bids: Vec<(f64, f64)>,  // (price, qty)
    pub asks: Vec<(f64, f64)>,
}
```

### 现有数据结构复用

HftBacktest 已有的数据结构可以直接复用：

```rust
// 可直接使用的现有类型
use hftbacktest::{
    types::{Event, LiveEvent},           // 行情事件
    depth::{MarketDepth, L2MarketDepth}, // 深度接口
    prelude::*,                           // 常量定义
};

// Event 结构 (64 bytes, 已有)
pub struct Event {
    pub ev: u64,        // 事件类型标志
    pub exch_ts: i64,   // 交易所时间戳
    pub local_ts: i64,  // 本地时间戳
    pub px: f64,        // 价格
    pub qty: f64,       // 数量
    pub order_id: u64,  // 订单 ID
    pub ival: i64,      // 整数扩展字段
    pub fval: f64,      // 浮点扩展字段
}
```

---

## 存储方案

### 方案概述

```
┌─────────────────────────────────────────────────────────────────┐
│                        存储层架构                                │
│                                                                  │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐ │
│  │   热数据        │  │   温数据        │  │   冷数据        │ │
│  │   (内存)        │  │   (内存映射)    │  │   (磁盘)        │ │
│  │                 │  │                 │  │                 │ │
│  │  • 当前因子值   │  │  • 近期状态快照 │  │  • 历史快照     │ │
│  │  • 滑动窗口数据 │  │  • 索引文件     │  │  • 原始行情     │ │
│  │  • 深度副本     │  │                 │  │  • 因子序列     │ │
│  └─────────────────┘  └─────────────────┘  └─────────────────┘ │
│          │                    │                    │            │
│          ▼                    ▼                    ▼            │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │                    统一存储接口                              ││
│  │  trait FactorStorage {                                       ││
│  │      fn save_state(&self, state: &FactorEngineState);       ││
│  │      fn load_state(&self, ts: i64) -> FactorEngineState;    ││
│  │      fn list_snapshots(&self) -> Vec<SnapshotMeta>;         ││
│  │  }                                                           ││
│  └─────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
```

### 存储格式选择

| 数据类型 | 推荐格式 | 原因 |
|---------|---------|------|
| 因子状态快照 | bincode + lz4 | 快速序列化，压缩率高 |
| 深度快照 | bincode | 与 IPC 格式一致 |
| 历史因子序列 | Apache Parquet | 列式存储，高效查询 |
| 索引文件 | SQLite | 轻量，ACID，快速查找 |

### 快照策略

```rust
pub struct SnapshotConfig {
    /// 快照间隔（纳秒）
    pub interval_ns: i64,

    /// 保留的快照数量
    pub max_snapshots: usize,

    /// 快照存储路径
    pub snapshot_dir: PathBuf,

    /// 是否压缩
    pub compression: bool,
}

impl FactorEngine {
    pub fn maybe_snapshot(&mut self) {
        let now = self.current_timestamp();
        if now - self.last_snapshot_ts >= self.config.interval_ns {
            let state = self.create_state_snapshot();
            self.storage.save_state(&state);
            self.last_snapshot_ts = now;

            // 清理旧快照
            self.storage.cleanup_old_snapshots(self.config.max_snapshots);
        }
    }
}
```

---

## IPC 因子同步机制

### 通道设计

```rust
// 新增 IPC 服务
const FACTOR_SERVICE_NAME: &str = "{connector_name}/Factor";

/// 因子通道消息类型
#[derive(Clone, Debug, Encode, Decode)]
pub enum FactorMessage {
    /// 单因子更新
    Update(FactorEvent),

    /// 批量因子更新
    BatchUpdate(FactorSnapshot),

    /// 因子注册（启动时发送）
    Register(FactorRegistry),

    /// 心跳（检测 Factor Engine 存活）
    Heartbeat { timestamp: i64 },
}
```

### 发布端（Factor Engine）

```rust
pub struct FactorPublisher {
    sender: IceoryxSender<FactorMessage>,
    batch_buffer: Vec<FactorEvent>,
    batch_threshold: usize,
}

impl FactorPublisher {
    /// 发布单个因子更新
    pub fn publish(&self, event: FactorEvent) -> Result<(), ChannelError> {
        self.sender.send(TO_ALL, &FactorMessage::Update(event))
    }

    /// 批量发布（减少 IPC 开销）
    pub fn publish_batch(&mut self, events: Vec<FactorEvent>) -> Result<(), ChannelError> {
        if events.len() >= self.batch_threshold {
            let snapshot = FactorSnapshot {
                timestamp: events.last().unwrap().timestamp,
                symbol: events[0].symbol.clone(),
                factors: events.iter().map(|e| (e.factor_id, e.value)).collect(),
            };
            self.sender.send(TO_ALL, &FactorMessage::BatchUpdate(snapshot))
        } else {
            for event in events {
                self.publish(event)?;
            }
            Ok(())
        }
    }
}
```

### 订阅端（Strategy Bot）

```rust
pub struct FactorReceiver {
    receiver: IceoryxReceiver<FactorMessage>,
    factor_cache: HashMap<u16, f64>,  // 本地因子缓存
    last_update_ts: i64,
}

impl FactorReceiver {
    /// 非阻塞接收因子更新
    pub fn try_recv(&mut self) -> Option<FactorUpdate> {
        match self.receiver.receive() {
            Ok(Some((_, FactorMessage::Update(event)))) => {
                self.factor_cache.insert(event.factor_id, event.value);
                self.last_update_ts = event.timestamp;
                Some(FactorUpdate::Single(event))
            }
            Ok(Some((_, FactorMessage::BatchUpdate(snapshot)))) => {
                for (id, value) in &snapshot.factors {
                    self.factor_cache.insert(*id, *value);
                }
                self.last_update_ts = snapshot.timestamp;
                Some(FactorUpdate::Batch(snapshot))
            }
            _ => None,
        }
    }

    /// 获取因子当前值
    pub fn get(&self, factor_id: u16) -> Option<f64> {
        self.factor_cache.get(&factor_id).copied()
    }
}
```

### 与策略集成

```rust
pub struct FactorAwareBot<CH, MD> {
    bot: LiveBot<CH, MD>,
    factor_receiver: FactorReceiver,
}

impl<CH, MD> FactorAwareBot<CH, MD>
where
    CH: Channel,
    MD: MarketDepth + L2MarketDepth,
{
    /// 处理行情和因子事件
    pub fn elapse_with_factors(&mut self, duration: i64) -> Result<ElapseResult, BotError> {
        // 1. 先处理所有待处理的因子更新（非阻塞）
        while let Some(_update) = self.factor_receiver.try_recv() {
            // 因子已更新到 cache
        }

        // 2. 正常处理行情和订单事件
        self.bot.elapse(duration)
    }

    /// 获取因子值
    pub fn factor(&self, factor_id: u16) -> Option<f64> {
        self.factor_receiver.get(factor_id)
    }
}
```

---

## 冷启动与热启动

### 启动模式

```rust
pub enum StartupMode {
    /// 冷启动：从头开始，无历史状态
    Cold,

    /// 热启动：从最近快照恢复
    Hot {
        snapshot_ts: Option<i64>,  // 指定快照时间，None 表示最新
    },

    /// 回放启动：从历史数据回放到指定时间点
    Replay {
        start_ts: i64,
        end_ts: i64,
        data_source: DataSource,
    },
}

pub enum DataSource {
    LocalFile(PathBuf),
    S3 { bucket: String, prefix: String },
}
```

### 冷启动流程

```
┌─────────────────────────────────────────────────────────────────┐
│                         冷启动流程                               │
│                                                                  │
│  1. 初始化空的 MarketDepth                                       │
│                    │                                             │
│                    ▼                                             │
│  2. 初始化所有因子（空状态）                                      │
│                    │                                             │
│                    ▼                                             │
│  3. 连接 Connector，订阅行情                                      │
│                    │                                             │
│                    ▼                                             │
│  4. 接收深度快照（BatchStart/BatchEnd）                           │
│                    │                                             │
│                    ▼                                             │
│  5. 开始正常处理行情事件                                          │
│                    │                                             │
│                    ▼                                             │
│  6. 因子预热期（窗口因子需要积累足够数据）                          │
│                    │                                             │
│                    ▼                                             │
│  7. 开始发布因子事件                                              │
└─────────────────────────────────────────────────────────────────┘
```

```rust
impl FactorEngine {
    pub fn cold_start(&mut self) -> Result<(), FactorError> {
        // 1. 初始化空状态
        self.depth.clear_depth(Side::None, 0.0);
        for factor in &mut self.factors {
            factor.reset();
        }

        // 2. 标记预热期
        self.warmup_mode = true;
        self.warmup_start_ts = self.current_timestamp();

        // 3. 等待深度快照
        self.wait_for_depth_snapshot()?;

        Ok(())
    }

    fn process_event(&mut self, event: &LiveEvent) {
        // 更新深度和因子...

        // 检查预热期是否结束
        if self.warmup_mode {
            let elapsed = self.current_timestamp() - self.warmup_start_ts;
            if elapsed >= self.warmup_duration_ns {
                self.warmup_mode = false;
                info!("Warmup period completed, start publishing factors");
            }
        }

        // 预热期间不发布因子
        if !self.warmup_mode {
            self.publish_factors();
        }
    }
}
```

### 热启动流程

```
┌─────────────────────────────────────────────────────────────────┐
│                         热启动流程                               │
│                                                                  │
│  1. 加载最近的状态快照                                            │
│                    │                                             │
│                    ▼                                             │
│  2. 验证快照完整性（checksum）                                    │
│                    │                                             │
│                    ▼                                             │
│  3. 恢复 MarketDepth 状态                                         │
│                    │                                             │
│                    ▼                                             │
│  4. 恢复各因子内部状态                                            │
│                    │                                             │
│                    ▼                                             │
│  5. 连接 Connector，订阅行情                                      │
│                    │                                             │
│                    ▼                                             │
│  6. 接收增量更新，与快照合并                                       │
│     （处理快照时间 → 当前时间的 gap）                              │
│                    │                                             │
│                    ▼                                             │
│  7. 立即开始发布因子事件                                          │
└─────────────────────────────────────────────────────────────────┘
```

```rust
impl FactorEngine {
    pub fn hot_start(&mut self, snapshot_ts: Option<i64>) -> Result<(), FactorError> {
        // 1. 加载快照
        let state = match snapshot_ts {
            Some(ts) => self.storage.load_state(ts)?,
            None => self.storage.load_latest_state()?,
        };

        // 2. 验证完整性
        if !state.verify_checksum() {
            return Err(FactorError::CorruptedSnapshot);
        }

        // 3. 恢复深度
        self.depth.restore_from_snapshot(&state.depth_snapshot);

        // 4. 恢复因子状态
        for factor in &mut self.factors {
            if let Some(factor_state) = state.factor_states.get(&factor.id()) {
                factor.deserialize_state(factor_state)?;
            }
        }

        // 5. 回放最近成交（恢复滑动窗口）
        for trade in &state.recent_trades {
            for factor in &mut self.factors {
                factor.on_trade(trade);
            }
        }

        info!(
            snapshot_ts = state.snapshot_ts,
            "Hot start completed from snapshot"
        );

        // 6. 无需预热，直接开始
        self.warmup_mode = false;

        Ok(())
    }
}
```

### 状态快照实现

```rust
impl FactorEngine {
    fn create_state_snapshot(&self) -> FactorEngineState {
        let mut factor_states = HashMap::new();
        for factor in &self.factors {
            factor_states.insert(factor.id(), factor.serialize_state());
        }

        let depth_snapshot = DepthSnapshot {
            symbol: self.symbol.clone(),
            timestamp: self.current_timestamp(),
            bids: self.depth.bid_levels(),
            asks: self.depth.ask_levels(),
        };

        let mut state = FactorEngineState {
            snapshot_ts: self.current_timestamp(),
            factor_states,
            depth_snapshot,
            recent_trades: self.recent_trades.clone(),
            checksum: 0,
        };

        state.checksum = state.compute_checksum();
        state
    }
}
```

---

## 实现路径建议

### 阶段一：基础设施（1-2 周）

1. **扩展 IPC 层**
   - 新增 `{name}/Factor` IceOryx2 服务
   - 定义 `FactorEvent`, `FactorMessage` 类型
   - 实现 `FactorPublisher` 和 `FactorReceiver`

2. **因子引擎骨架**
   - 创建 `factor-engine` crate
   - 实现 `StreamingFactor` trait
   - 行情事件处理循环

### 阶段二：核心因子（2-3 周）

3. **实现基础因子**
   ```rust
   // 推荐先实现的因子
   - VWAP (成交量加权平均价)
   - OrderFlowImbalance (订单流不平衡)
   - MicroPrice (微观价格)
   - SpreadFactor (买卖价差)
   - DepthImbalance (深度不平衡)
   - TradeIntensity (成交强度)
   ```

4. **状态持久化**
   - 实现快照存储
   - 实现冷/热启动逻辑

### 阶段三：策略集成（1-2 周）

5. **Bot 集成**
   - 实现 `FactorAwareBot`
   - 因子缓存和查询接口

6. **Python 绑定**
   - 导出因子接口到 Python
   - 支持 Numba 调用

### 阶段四：优化与扩展（持续）

7. **性能优化**
   - 因子计算向量化 (SIMD)
   - 批量发布优化
   - 内存池复用

8. **动态因子支持**
   - WASM 或 Lua 动态加载
   - 因子热重载

### 代码组织建议

```
hftbacktest/
├── factor-engine/           # 新增 crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── traits.rs        # StreamingFactor trait
│       ├── factors/         # 因子实现
│       │   ├── mod.rs
│       │   ├── vwap.rs
│       │   ├── ofi.rs
│       │   └── ...
│       ├── engine.rs        # 因子引擎主循环
│       ├── storage.rs       # 状态持久化
│       ├── ipc.rs           # 因子 IPC 发布/订阅
│       └── startup.rs       # 冷/热启动逻辑
├── hftbacktest/
│   └── src/
│       ├── live/
│       │   ├── factor_receiver.rs  # 新增：策略端因子接收
│       │   └── ...
│       └── ...
└── py-hftbacktest/
    └── src/
        └── factor.rs        # 新增：Python 因子绑定
```

---

## 附录：性能预估

| 操作 | 预期延迟 |
|-----|---------|
| 因子计算（单因子） | 100-500 ns |
| IPC 发布（单消息） | 1-5 μs |
| 状态快照（100 因子） | 1-10 ms |
| 热启动恢复 | 10-100 ms |

关键性能指标：
- **因子更新到策略接收**：< 10 μs
- **每秒因子计算量**：> 1M 次/因子
- **状态快照间隔**：建议 1-5 分钟
