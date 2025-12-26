# HFT System Enhancement Requirements

## Overview

从高频策略角度分析，当前 HFTBacktest 框架缺少以下关键模块：

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           HFT System Architecture                            │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐                   │
│  │ Factor       │    │ Strategy     │    │ Order        │                   │
│  │ Module       │───▶│ Engine       │───▶│ Recording    │                   │
│  │ (Alpha)      │    │ (Core)       │    │ Module       │                   │
│  └──────────────┘    └──────────────┘    └──────────────┘                   │
│         │                   │                   │                            │
│         │                   │                   ▼                            │
│         │                   │           ┌──────────────┐                    │
│         │                   │           │ Persistence  │                    │
│         │                   │           │ (ClickHouse) │                    │
│         │                   │           └──────────────┘                    │
│         │                   │                   │                            │
│         │                   ▼                   ▼                            │
│         │           ┌──────────────┐    ┌──────────────┐                    │
│         │           │ Strategy     │    │ Visualization│                    │
│         └──────────▶│ Control      │    │ (Grafana)    │                    │
│                     └──────────────┘    └──────────────┘                    │
│                                                                              │
│  ═══════════════════════════════════════════════════════════════════════    │
│                        IceOryx2 IPC Layer                                   │
│  ═══════════════════════════════════════════════════════════════════════    │
│                                                                              │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐                   │
│  │ Connector    │    │ Collector    │    │ External     │                   │
│  │ (Exchange)   │    │ (Data)       │    │ Data Sources │                   │
│  └──────────────┘    └──────────────┘    └──────────────┘                   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 1. Factor Module (因子模块)

### 1.1 需求分析

| 需求 | 描述 | 优先级 |
|------|------|--------|
| Alpha 信号输入 | 外部预测信号 feed 到策略 | P0 |
| 历史订单簿缓存 | 最近 N 秒/N 档的 book 快照 | P0 |
| 历史成交缓存 | 最近 N 秒/N 条的 trade 数据 | P0 |
| 自定义数据源 | 其他预测信号（如情绪、资金流等） | P1 |
| 回测/实盘一致 | 同一套因子代码两种模式运行 | P0 |

### 1.2 当前限制

```rust
// 当前 Bot trait 只提供实时快照
fn depth(&self, asset_no: usize) -> &MD;           // 当前订单簿
fn last_trades(&self, asset_no: usize) -> &[Event]; // 最近成交（无时间窗口）
fn position(&self, asset_no: usize) -> f64;         // 当前仓位
```

**缺失：**
- 无时间窗口的历史数据访问
- 无外部信号输入接口
- 无因子计算框架

### 1.3 设计方案

#### 方案 A：内置 RingBuffer

```rust
pub struct FactorContext<MD> {
    // 历史订单簿快照 (时间索引)
    book_snapshots: RingBuffer<(i64, BookSnapshot)>,  // (timestamp, snapshot)

    // 历史成交
    trade_history: RingBuffer<Event>,

    // 外部 Alpha 信号
    alpha_signals: HashMap<String, RingBuffer<(i64, f64)>>,

    // 配置
    config: FactorConfig,
}

pub struct FactorConfig {
    book_snapshot_interval_ns: i64,  // 快照间隔
    book_history_duration_ns: i64,   // 保留时长
    trade_history_capacity: usize,   // 成交缓存条数
    alpha_buffer_capacity: usize,    // Alpha 缓存条数
}

impl<MD: MarketDepth> FactorContext<MD> {
    /// 获取 N 纳秒前的订单簿
    pub fn book_at(&self, timestamp: i64) -> Option<&BookSnapshot>;

    /// 获取时间窗口内的成交
    pub fn trades_in_window(&self, duration_ns: i64) -> &[Event];

    /// 获取 Alpha 信号
    pub fn alpha(&self, name: &str) -> Option<f64>;

    /// 计算 VWAP
    pub fn vwap(&self, duration_ns: i64) -> f64;

    /// 计算订单流不平衡
    pub fn order_flow_imbalance(&self, duration_ns: i64) -> f64;
}
```

#### 方案 B：独立 Factor Engine (通过 IPC)

```
┌─────────────────┐     IceOryx2      ┌─────────────────┐
│                 │◀─────────────────▶│                 │
│  Strategy Bot   │   FactorRequest   │  Factor Engine  │
│                 │   FactorResponse  │                 │
└─────────────────┘                   └─────────────────┘
                                              │
                                              ▼
                                      ┌───────────────┐
                                      │ Data Sources  │
                                      │ - Book history│
                                      │ - Trade history
                                      │ - External API│
                                      └───────────────┘
```

#### 推荐：方案 A + 可选方案 B

- **核心因子** (book/trade 历史): 内置 RingBuffer，低延迟
- **复杂因子** (ML 模型等): 独立进程，通过 IPC 通信

### 1.4 实现优先级

```
Phase 1: 基础数据缓存
├── BookSnapshot RingBuffer
├── Trade RingBuffer
└── 回测/实盘统一接口

Phase 2: 内置因子
├── VWAP, TWAP
├── Order Flow Imbalance
├── Book Pressure
└── Trade Intensity

Phase 3: 外部信号
├── Alpha IPC Channel
├── Signal Registry
└── Factor Expression DSL
```

---

## 2. Order Recording Module (订单录制/分析模块)

### 2.1 需求分析

| 需求 | 描述 | 优先级 |
|------|------|--------|
| 订单生命周期记录 | 每个订单从创建到结束的完整轨迹 | P0 |
| 仓位变化记录 | 实时仓位快照 | P0 |
| PnL 计算 | 实现/未实现盈亏 | P0 |
| 持久化存储 | 写入时序数据库 | P0 |
| 实时可视化 | Grafana Dashboard | P1 |
| 成交分析 | Slippage, Fill Rate, Latency | P1 |

### 2.2 数据模型

```sql
-- ClickHouse Schema

-- 订单表
CREATE TABLE orders (
    order_id UInt64,
    symbol String,
    side Enum8('Buy' = 1, 'Sell' = -1),
    price Decimal64(8),
    qty Decimal64(8),
    status Enum8('New' = 1, 'PartiallyFilled' = 5, 'Filled' = 3, 'Canceled' = 4),
    create_time DateTime64(9),  -- 纳秒精度
    update_time DateTime64(9),
    exch_time DateTime64(9),
    leaves_qty Decimal64(8),
    exec_qty Decimal64(8),
    exec_price Decimal64(8),
    latency_ns Int64,           -- 订单延迟
    strategy_id String,
    INDEX idx_symbol (symbol) TYPE bloom_filter GRANULARITY 4
) ENGINE = MergeTree()
PARTITION BY toYYYYMMDD(create_time)
ORDER BY (symbol, create_time, order_id);

-- 仓位快照表
CREATE TABLE positions (
    timestamp DateTime64(9),
    symbol String,
    position Decimal64(8),
    avg_price Decimal64(8),
    unrealized_pnl Decimal64(8),
    realized_pnl Decimal64(8),
    strategy_id String
) ENGINE = MergeTree()
PARTITION BY toYYYYMMDD(timestamp)
ORDER BY (symbol, timestamp);

-- 成交表
CREATE TABLE fills (
    fill_id UInt64,
    order_id UInt64,
    symbol String,
    side Enum8('Buy' = 1, 'Sell' = -1),
    price Decimal64(8),
    qty Decimal64(8),
    fee Decimal64(8),
    is_maker Bool,
    exch_time DateTime64(9),
    local_time DateTime64(9),
    slippage_bps Decimal64(4),  -- 滑点 (basis points)
    strategy_id String
) ENGINE = MergeTree()
PARTITION BY toYYYYMMDD(exch_time)
ORDER BY (symbol, exch_time, fill_id);
```

### 2.3 架构设计

```
┌─────────────────────────────────────────────────────────────┐
│                        Strategy Bot                          │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  OrderRecorder (内置)                                │    │
│  │  - 订单状态变化回调                                   │    │
│  │  - 仓位变化回调                                       │    │
│  │  - 成交回调                                           │    │
│  └──────────────────────┬──────────────────────────────┘    │
│                         │ IceOryx2 Publish                   │
└─────────────────────────┼───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│                    Recording Service                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐          │
│  │ IPC         │  │ Aggregator  │  │ ClickHouse  │          │
│  │ Subscriber  │─▶│ & Enricher  │─▶│ Writer      │          │
│  └─────────────┘  └─────────────┘  └─────────────┘          │
│                                            │                 │
└────────────────────────────────────────────┼─────────────────┘
                                             │
                                             ▼
                                    ┌─────────────────┐
                                    │   ClickHouse    │
                                    └────────┬────────┘
                                             │
                                             ▼
                                    ┌─────────────────┐
                                    │    Grafana      │
                                    │   Dashboard     │
                                    └─────────────────┘
```

### 2.4 IPC 消息定义

```rust
#[derive(Clone, Debug, Encode, Decode)]
pub enum RecordingEvent {
    /// 订单状态更新
    OrderUpdate {
        timestamp: i64,
        strategy_id: String,
        order: Order,
        prev_status: Status,
    },

    /// 成交
    Fill {
        timestamp: i64,
        strategy_id: String,
        order_id: u64,
        symbol: String,
        side: Side,
        price: f64,
        qty: f64,
        fee: f64,
        is_maker: bool,
        mid_price: f64,  // 用于计算滑点
    },

    /// 仓位快照
    PositionSnapshot {
        timestamp: i64,
        strategy_id: String,
        symbol: String,
        position: f64,
        avg_price: f64,
        mid_price: f64,
        unrealized_pnl: f64,
        realized_pnl: f64,
    },

    /// 策略状态
    StrategyStatus {
        timestamp: i64,
        strategy_id: String,
        status: StrategyState,
        message: String,
    },
}

#[derive(Clone, Debug, Encode, Decode)]
pub enum StrategyState {
    Starting,
    Running,
    Paused,
    Stopping,
    Stopped,
    Error,
}
```

### 2.5 Grafana Dashboard 指标

```
┌────────────────────────────────────────────────────────────┐
│                    Strategy Dashboard                       │
├────────────────────────────────────────────────────────────┤
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │ Position     │  │ PnL (Real)   │  │ PnL (Unreal) │      │
│  │   0.05 BTC   │  │   $1,234.56  │  │   $-45.67    │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │                  PnL Time Series                     │   │
│  │  ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄  │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
│  ┌──────────────────────┐  ┌──────────────────────┐        │
│  │   Order Fill Rate    │  │   Avg Latency (ms)   │        │
│  │       87.5%          │  │        12.3          │        │
│  └──────────────────────┘  └──────────────────────┘        │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │              Recent Orders                           │   │
│  │  ID      Side   Price      Qty    Status   Latency  │   │
│  │  12345   BUY    98000.50   0.01   Filled   8.2ms    │   │
│  │  12346   SELL   98010.20   0.01   Open     -        │   │
│  └─────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────┘
```

---

## 3. Strategy Control Module (策略控制模块)

### 3.1 需求分析

| 需求 | 描述 | 优先级 |
|------|------|--------|
| 暂停/恢复 | 暂停下单但保持市场数据更新 | P0 |
| 参数热更新 | 运行时修改策略参数 | P0 |
| 紧急停止 | 撤销所有订单并停止策略 | P0 |
| 仓位限制 | 动态调整最大仓位 | P1 |
| 风控规则 | 动态风控参数 | P1 |

### 3.2 控制接口设计

```rust
#[derive(Clone, Debug, Encode, Decode)]
pub enum ControlCommand {
    /// 暂停策略（保持市场数据更新，停止下单）
    Pause,

    /// 恢复策略
    Resume,

    /// 紧急停止（撤销所有订单，平仓，停止策略）
    EmergencyStop {
        cancel_orders: bool,
        close_position: bool,
    },

    /// 更新参数
    UpdateParams {
        params: HashMap<String, Value>,
    },

    /// 更新风控限制
    UpdateRiskLimits {
        max_position: Option<f64>,
        max_order_qty: Option<f64>,
        max_notional: Option<f64>,
    },

    /// 查询状态
    QueryStatus,
}

#[derive(Clone, Debug, Encode, Decode)]
pub struct ControlResponse {
    pub success: bool,
    pub message: String,
    pub strategy_state: StrategyState,
    pub current_params: HashMap<String, Value>,
}
```

### 3.3 实现方式

#### 方式 A：IPC Control Channel

```
┌─────────────────┐     IceOryx2      ┌─────────────────┐
│                 │◀─────────────────▶│                 │
│  Control CLI    │  ControlCommand   │  Strategy Bot   │
│  / Web API      │  ControlResponse  │                 │
└─────────────────┘                   └─────────────────┘
```

#### 方式 B：REST API (通过独立服务)

```
┌─────────────────┐      HTTP         ┌─────────────────┐
│                 │◀─────────────────▶│  Control        │
│  Web UI         │                   │  Service        │
│  / CLI          │                   │                 │
└─────────────────┘                   └────────┬────────┘
                                               │ IceOryx2
                                               ▼
                                      ┌─────────────────┐
                                      │  Strategy Bot   │
                                      └─────────────────┘
```

### 3.4 策略端集成

```rust
pub struct StrategyController {
    state: Arc<RwLock<StrategyState>>,
    params: Arc<RwLock<StrategyParams>>,
    risk_limits: Arc<RwLock<RiskLimits>>,
    control_rx: IpcReceiver<ControlCommand>,
    control_tx: IpcSender<ControlResponse>,
}

impl StrategyController {
    /// 检查是否应该暂停（每个循环开始时调用）
    pub fn should_pause(&self) -> bool;

    /// 获取当前参数
    pub fn params(&self) -> StrategyParams;

    /// 获取风控限制
    pub fn risk_limits(&self) -> RiskLimits;

    /// 处理控制命令（非阻塞）
    pub fn poll_commands(&self) -> Option<ControlCommand>;
}

// 策略主循环集成
while hbt.elapse(100_000_000)? == ElapseResult::Ok {
    // 1. 处理控制命令
    if let Some(cmd) = controller.poll_commands() {
        handle_command(cmd)?;
    }

    // 2. 检查是否暂停
    if controller.should_pause() {
        continue;  // 跳过交易逻辑，但继续处理市场数据
    }

    // 3. 获取最新参数
    let params = controller.params();
    let limits = controller.risk_limits();

    // 4. 执行策略逻辑...
}
```

---

## 4. Implementation Roadmap

### Phase 1: Core Infrastructure (2-3 weeks)

```
Week 1-2:
├── Factor Module - 基础数据缓存
│   ├── RingBuffer 实现
│   ├── BookSnapshot 结构
│   └── 集成到 Bot trait
│
└── Recording Module - 基础版本
    ├── IPC Event 定义
    ├── ClickHouse schema
    └── 基础 Writer

Week 3:
├── Strategy Control - 基础版本
│   ├── Pause/Resume
│   └── Emergency Stop
│
└── 集成测试
```

### Phase 2: Enhanced Features (2-3 weeks)

```
Week 4-5:
├── Factor Module - 内置因子
│   ├── VWAP, TWAP
│   ├── Order Flow Imbalance
│   └── Book Pressure
│
└── Recording Module - 分析功能
    ├── Slippage 计算
    ├── Latency 分析
    └── Grafana Dashboard

Week 6:
├── Strategy Control - 热更新
│   ├── 参数热更新
│   └── 风控限制更新
│
└── CLI 工具
```

### Phase 3: Advanced Features (2-3 weeks)

```
Week 7-8:
├── Factor Module - 外部信号
│   ├── Alpha IPC Channel
│   ├── Signal Registry
│   └── ML Model 集成
│
└── Recording Module - 高级分析
    ├── 成交分析报告
    ├── 策略性能归因
    └── 实时告警

Week 9:
├── Web UI
│   ├── 策略监控
│   ├── 参数配置
│   └── 历史查询
│
└── 文档 & 示例
```

---

## 5. Technology Stack

| 组件 | 技术选型 | 理由 |
|------|----------|------|
| IPC | IceOryx2 | 已在用，低延迟 |
| 时序数据库 | ClickHouse | 高性能，列式存储 |
| 可视化 | Grafana | 成熟，ClickHouse 插件 |
| 控制 API | gRPC 或 REST | 标准化，易于集成 |
| 配置管理 | TOML + 热更新 | Rust 生态标准 |

---

## 6. Open Questions

1. **因子计算延迟要求？**
   - 内置因子目标延迟 < 1μs
   - 外部因子可接受延迟？

2. **历史数据保留时长？**
   - Book snapshots: 建议 1-5 分钟
   - Trades: 建议 1000-10000 条
   - 可配置

3. **多策略支持？**
   - 是否需要多策略共享 connector？
   - 策略间隔离 vs 共享数据？

4. **回测数据复用？**
   - 因子数据是否需要预计算并存储？
   - 回测时是否需要精确复现实盘计算？
