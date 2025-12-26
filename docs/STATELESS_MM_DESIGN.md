# Stateless Market Making with Computation Graph

## 1. 当前问题分析

### 1.1 obi_mm 策略的状态依赖

```python
# 当前策略中的状态
imbalance_timeseries = np.full(30_000_000, np.nan)  # OBI 历史 (O(N) 空间)
change_timeseries = np.full(30_000_000, np.nan)     # 价格变化历史

# 滚动统计 - 每次计算 O(window)
m = np.nanmean(imbalance_timeseries[t-window:t+1])
s = np.nanstd(imbalance_timeseries[t-window:t+1])
volatility = np.nanstd(change_timeseries[t-60:t+1])
```

**问题：**
- 内存占用大 (30M * 8 bytes * 2 = 480MB)
- 滚动统计复杂度 O(window)
- 策略与因子计算耦合
- 回测/实盘代码不一致

### 1.2 目标

```
┌─────────────────────────────────────────────────────────────────┐
│                     Stateless MM Strategy                        │
│                                                                  │
│  Input:  { alpha, volatility, stop_loss_signal, ... }           │
│  Output: { bid_orders[], ask_orders[] }                          │
│                                                                  │
│  NO internal state - pure function of inputs                     │
└─────────────────────────────────────────────────────────────────┘
```

---

## 2. 架构设计

### 2.1 Overall Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                                                                          │
│   Market Data                    Computation Graph                       │
│   (Connector)                    (Factor Engine)                         │
│       │                               │                                  │
│       ▼                               ▼                                  │
│  ┌─────────┐    IPC          ┌──────────────────┐                       │
│  │  Book   │────────────────▶│  StreamingNode   │                       │
│  │  Trade  │                 │  ┌────────────┐  │                       │
│  └─────────┘                 │  │ OBI Node   │  │                       │
│                              │  └─────┬──────┘  │                       │
│                              │        ▼         │                       │
│                              │  ┌────────────┐  │    ┌───────────────┐  │
│                              │  │ ZScore     │──────▶│ FactorOutput  │  │
│                              │  │ Node       │  │    │ { alpha,      │  │
│                              │  └────────────┘  │    │   volatility, │  │
│                              │                  │    │   stop_loss } │  │
│                              │  ┌────────────┐  │    └───────┬───────┘  │
│                              │  │ Volatility │──┘            │          │
│                              │  │ Node       │               │ IPC      │
│                              │  └────────────┘               ▼          │
│                              │                        ┌─────────────┐   │
│                              │  ┌────────────┐        │  Stateless  │   │
│                              │  │ StopLoss   │───────▶│  MM Strategy│   │
│                              │  │ Node       │        └─────────────┘   │
│                              │  └────────────┘                          │
│                              └──────────────────┘                       │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

### 2.2 Computation Graph Nodes

每个 Node 维护 **O(1) 增量状态**，无需存储完整历史：

```rust
/// 计算图节点 trait
pub trait StreamingNode: Send + Sync {
    type Input;
    type Output;

    /// 增量更新，O(1) 时间复杂度
    fn update(&mut self, input: &Self::Input) -> Self::Output;

    /// 重置状态
    fn reset(&mut self);
}
```

---

## 3. O(1) 流式计算实现

### 3.1 Welford's Online Algorithm (滚动均值/方差)

```rust
/// O(1) 滚动统计 - Welford's Algorithm with Exponential Decay
pub struct ExponentialStats {
    alpha: f64,         // 衰减因子 (e.g., 2/(window+1))
    mean: f64,          // 指数加权均值
    var: f64,           // 指数加权方差
    initialized: bool,
}

impl ExponentialStats {
    pub fn new(window: usize) -> Self {
        Self {
            alpha: 2.0 / (window as f64 + 1.0),
            mean: 0.0,
            var: 0.0,
            initialized: false,
        }
    }

    /// O(1) 更新
    #[inline]
    pub fn update(&mut self, value: f64) -> (f64, f64) {
        if !self.initialized {
            self.mean = value;
            self.var = 0.0;
            self.initialized = true;
        } else {
            let delta = value - self.mean;
            self.mean += self.alpha * delta;
            self.var = (1.0 - self.alpha) * (self.var + self.alpha * delta * delta);
        }
        (self.mean, self.var.sqrt())
    }
}
```

### 3.2 固定窗口滚动统计 (精确版)

```rust
/// O(1) 固定窗口滚动统计 - 使用 Welford's 增量算法
pub struct RollingStats {
    window: usize,
    buffer: RingBuffer<f64>,
    sum: f64,
    sum_sq: f64,
    count: usize,
}

impl RollingStats {
    pub fn new(window: usize) -> Self {
        Self {
            window,
            buffer: RingBuffer::new(window),
            sum: 0.0,
            sum_sq: 0.0,
            count: 0,
        }
    }

    /// O(1) 更新
    #[inline]
    pub fn update(&mut self, value: f64) -> (f64, f64) {
        // 移除最旧的值
        if let Some(old) = self.buffer.push(value) {
            self.sum -= old;
            self.sum_sq -= old * old;
        } else {
            self.count += 1;
        }

        // 添加新值
        self.sum += value;
        self.sum_sq += value * value;

        // 计算均值和标准差
        let mean = self.sum / self.count as f64;
        let variance = (self.sum_sq / self.count as f64) - mean * mean;
        let std = variance.max(0.0).sqrt();

        (mean, std)
    }
}
```

### 3.3 RingBuffer 实现

```rust
pub struct RingBuffer<T> {
    data: Vec<T>,
    head: usize,
    len: usize,
    capacity: usize,
}

impl<T: Default + Clone> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![T::default(); capacity],
            head: 0,
            len: 0,
            capacity,
        }
    }

    /// O(1) push, 返回被移除的旧值
    #[inline]
    pub fn push(&mut self, value: T) -> Option<T> {
        let old = if self.len == self.capacity {
            Some(std::mem::replace(&mut self.data[self.head], value))
        } else {
            self.data[self.head] = value;
            self.len += 1;
            None
        };
        self.head = (self.head + 1) % self.capacity;
        old
    }
}
```

---

## 4. 计算图 Node 实现

### 4.1 OBI (Order Book Imbalance) Node

```rust
pub struct ObiNode {
    looking_depth: f64,  // e.g., 0.001 (0.1%)
}

impl StreamingNode for ObiNode {
    type Input = BookSnapshot;
    type Output = f64;

    #[inline]
    fn update(&mut self, book: &BookSnapshot) -> f64 {
        let mid = (book.best_bid + book.best_ask) / 2.0;
        let bid_boundary = mid * (1.0 - self.looking_depth);
        let ask_boundary = mid * (1.0 + self.looking_depth);

        let bid_qty: f64 = book.bids.iter()
            .take_while(|(px, _)| *px >= bid_boundary)
            .map(|(_, qty)| qty)
            .sum();

        let ask_qty: f64 = book.asks.iter()
            .take_while(|(px, _)| *px <= ask_boundary)
            .map(|(_, qty)| qty)
            .sum();

        bid_qty - ask_qty  // OBI
    }

    fn reset(&mut self) {}
}
```

### 4.2 ZScore Node

```rust
pub struct ZScoreNode {
    stats: RollingStats,  // O(1) 滚动统计
}

impl ZScoreNode {
    pub fn new(window: usize) -> Self {
        Self {
            stats: RollingStats::new(window),
        }
    }
}

impl StreamingNode for ZScoreNode {
    type Input = f64;
    type Output = f64;

    #[inline]
    fn update(&mut self, value: &f64) -> f64 {
        let (mean, std) = self.stats.update(*value);
        if std > 1e-10 {
            (value - mean) / std
        } else {
            0.0
        }
    }

    fn reset(&mut self) {
        self.stats = RollingStats::new(self.stats.window);
    }
}
```

### 4.3 Volatility Node

```rust
pub struct VolatilityNode {
    window: usize,
    stats: RollingStats,
    last_price: f64,
}

impl StreamingNode for VolatilityNode {
    type Input = f64;  // mid_price
    type Output = f64; // volatility

    #[inline]
    fn update(&mut self, mid_price: &f64) -> f64 {
        if self.last_price > 0.0 {
            let ret = mid_price / self.last_price - 1.0;
            let (_, std) = self.stats.update(ret);
            self.last_price = *mid_price;
            std
        } else {
            self.last_price = *mid_price;
            0.0
        }
    }

    fn reset(&mut self) {
        self.last_price = 0.0;
        self.stats = RollingStats::new(self.window);
    }
}
```

### 4.4 Stop Loss Node

```rust
pub struct StopLossNode {
    volatility_threshold: f64,
    change_threshold: f64,
    baseline_volatility: f64,
    baseline_change_std: f64,
}

impl StreamingNode for StopLossNode {
    type Input = (f64, f64);  // (volatility, price_change)
    type Output = StopLossSignal;

    #[inline]
    fn update(&mut self, input: &(f64, f64)) -> StopLossSignal {
        let (volatility, change) = *input;

        let vol_zscore = (volatility - self.baseline_volatility) / self.baseline_volatility;
        let change_zscore = change.abs() / self.baseline_change_std;

        if vol_zscore > self.volatility_threshold || change_zscore > self.change_threshold {
            StopLossSignal::Triggered
        } else {
            StopLossSignal::Normal
        }
    }

    fn reset(&mut self) {}
}

#[derive(Clone, Copy, Debug)]
pub enum StopLossSignal {
    Normal,
    Triggered,
}
```

---

## 5. 计算图组装

### 5.1 Graph Builder

```rust
pub struct FactorGraph {
    obi_node: ObiNode,
    zscore_node: ZScoreNode,
    volatility_node: VolatilityNode,
    stop_loss_node: StopLossNode,
}

#[derive(Clone, Debug)]
pub struct FactorOutput {
    pub alpha: f64,           // Normalized OBI
    pub volatility: f64,      // Rolling volatility
    pub stop_loss: StopLossSignal,
    pub timestamp: i64,
}

impl FactorGraph {
    pub fn new(config: FactorConfig) -> Self {
        Self {
            obi_node: ObiNode { looking_depth: config.looking_depth },
            zscore_node: ZScoreNode::new(config.obi_window),
            volatility_node: VolatilityNode::new(config.volatility_window),
            stop_loss_node: StopLossNode::new(config.stop_loss_config),
        }
    }

    /// O(1) 更新整个计算图
    #[inline]
    pub fn update(&mut self, book: &BookSnapshot, timestamp: i64) -> FactorOutput {
        // Step 1: OBI 计算
        let obi = self.obi_node.update(book);

        // Step 2: ZScore 标准化
        let alpha = self.zscore_node.update(&obi);

        // Step 3: 波动率计算
        let mid_price = (book.best_bid + book.best_ask) / 2.0;
        let volatility = self.volatility_node.update(&mid_price);

        // Step 4: 止损信号
        let price_change = if self.volatility_node.last_price > 0.0 {
            mid_price / self.volatility_node.last_price - 1.0
        } else {
            0.0
        };
        let stop_loss = self.stop_loss_node.update(&(volatility, price_change));

        FactorOutput {
            alpha,
            volatility,
            stop_loss,
            timestamp,
        }
    }
}
```

---

## 6. 无状态 MM 策略

### 6.1 策略输入/输出定义

```rust
/// 策略输入 - 完全外部提供
#[derive(Clone, Debug)]
pub struct StrategyInput {
    // 因子
    pub alpha: f64,
    pub volatility: f64,
    pub stop_loss: StopLossSignal,

    // 市场数据
    pub best_bid: f64,
    pub best_ask: f64,
    pub mid_price: f64,

    // 状态
    pub position: f64,
    pub active_orders: Vec<OrderInfo>,

    // 参数
    pub params: StrategyParams,
}

/// 策略输出 - 订单指令
#[derive(Clone, Debug)]
pub struct StrategyOutput {
    pub cancel_orders: Vec<u64>,      // 要取消的订单 ID
    pub new_bid_orders: Vec<OrderRequest>,
    pub new_ask_orders: Vec<OrderRequest>,
}

#[derive(Clone, Debug)]
pub struct OrderRequest {
    pub price: f64,
    pub qty: f64,
}
```

### 6.2 Pure Function 策略实现

```rust
/// 无状态 MM 策略 - 纯函数
pub fn stateless_mm(input: &StrategyInput) -> StrategyOutput {
    let p = &input.params;

    // 止损检查
    if matches!(input.stop_loss, StopLossSignal::Triggered) {
        return StrategyOutput {
            cancel_orders: input.active_orders.iter().map(|o| o.order_id).collect(),
            new_bid_orders: vec![],
            new_ask_orders: if input.position.abs() > 1e-9 {
                // 市价平仓
                vec![OrderRequest {
                    price: if input.position > 0.0 {
                        input.best_bid - 100.0 * p.tick_size
                    } else {
                        input.best_ask + 100.0 * p.tick_size
                    },
                    qty: input.position.abs(),
                }]
            } else {
                vec![]
            },
        };
    }

    // 计算报价
    let volatility_ratio = (input.volatility / p.baseline_volatility).powf(p.power);
    let half_spread = p.half_spread * volatility_ratio;

    let fair_price = input.mid_price + p.c1 * input.alpha * input.mid_price;
    let normalized_position = input.position / p.order_qty;
    let reservation_price = fair_price - p.skew * normalized_position * input.mid_price;

    let bid_price = (reservation_price - half_spread * input.mid_price).min(input.best_bid);
    let ask_price = (reservation_price + half_spread * input.mid_price).max(input.best_ask);

    let bid_price = (bid_price / p.tick_size).floor() * p.tick_size;
    let ask_price = (ask_price / p.tick_size).ceil() * p.tick_size;

    // 生成订单网格
    let mut new_bids = vec![];
    let mut new_asks = vec![];

    if input.position < p.max_position {
        for i in 0..p.grid_num {
            new_bids.push(OrderRequest {
                price: bid_price - (i as f64) * p.grid_interval,
                qty: p.order_qty,
            });
        }
    }

    if input.position > -p.max_position {
        for i in 0..p.grid_num {
            new_asks.push(OrderRequest {
                price: ask_price + (i as f64) * p.grid_interval,
                qty: p.order_qty,
            });
        }
    }

    // 确定要取消的订单
    let cancel_orders = compute_orders_to_cancel(&input.active_orders, &new_bids, &new_asks, p);

    StrategyOutput {
        cancel_orders,
        new_bid_orders: new_bids,
        new_ask_orders: new_asks,
    }
}
```

---

## 7. 集成架构

### 7.1 独立进程模式 (推荐)

```
┌─────────────┐    ┌─────────────────┐    ┌─────────────┐
│  Connector  │───▶│  Factor Engine  │───▶│  Strategy   │
│  (Market    │    │  (Computation   │    │  (Stateless │
│   Data)     │    │   Graph)        │    │   MM)       │
└─────────────┘    └─────────────────┘    └─────────────┘
       │                   │                     │
       └───────────────────┴─────────────────────┘
                     IceOryx2 IPC
```

### 7.2 IPC 消息定义

```rust
// Factor Engine -> Strategy
#[derive(Clone, Encode, Decode)]
pub struct FactorUpdate {
    pub timestamp: i64,
    pub symbol: String,
    pub alpha: f64,
    pub volatility: f64,
    pub stop_loss: StopLossSignal,
    pub best_bid: f64,
    pub best_ask: f64,
}

// 策略订阅 Factor Engine 的输出
impl Strategy {
    fn run(&mut self) {
        while let Ok(factors) = self.factor_rx.recv_timeout(Duration::from_secs(1)) {
            let position = self.get_position();
            let orders = self.get_active_orders();

            let input = StrategyInput {
                alpha: factors.alpha,
                volatility: factors.volatility,
                stop_loss: factors.stop_loss,
                best_bid: factors.best_bid,
                best_ask: factors.best_ask,
                mid_price: (factors.best_bid + factors.best_ask) / 2.0,
                position,
                active_orders: orders,
                params: self.params.clone(),
            };

            let output = stateless_mm(&input);
            self.execute_orders(output);
        }
    }
}
```

---

## 8. 时间复杂度分析

| 操作 | 当前实现 | 优化后 |
|------|----------|--------|
| OBI 计算 | O(depth_levels) | O(depth_levels) |
| OBI ZScore | O(window) | **O(1)** |
| Volatility | O(window) | **O(1)** |
| Stop Loss Check | O(1) | O(1) |
| 策略逻辑 | O(grid_num) | O(grid_num) |
| **总计** | O(window) | **O(1)** |

| 资源 | 当前实现 | 优化后 |
|------|----------|--------|
| 内存 (OBI history) | 30M * 8B = 240MB | window * 8B ≈ 8KB |
| 内存 (Change history) | 30M * 8B = 240MB | window * 8B ≈ 480B |

---

## 9. 回测/实盘统一

### 9.1 Trait 抽象

```rust
pub trait FactorProvider {
    fn get_factors(&mut self) -> Option<FactorOutput>;
}

// 回测实现
pub struct BacktestFactorProvider {
    graph: FactorGraph,
    data_reader: DataReader,
}

impl FactorProvider for BacktestFactorProvider {
    fn get_factors(&mut self) -> Option<FactorOutput> {
        let book = self.data_reader.next_book()?;
        Some(self.graph.update(&book, book.timestamp))
    }
}

// 实盘实现
pub struct LiveFactorProvider {
    factor_rx: IpcReceiver<FactorOutput>,
}

impl FactorProvider for LiveFactorProvider {
    fn get_factors(&mut self) -> Option<FactorOutput> {
        self.factor_rx.try_recv().ok()
    }
}

// 策略代码统一
fn run_strategy<F: FactorProvider>(factor_provider: &mut F, params: StrategyParams) {
    while let Some(factors) = factor_provider.get_factors() {
        let output = stateless_mm(&build_input(factors, params));
        // execute orders...
    }
}
```

---

## 10. 总结

### 优势

1. **O(1) 计算复杂度** - 使用增量统计算法
2. **低内存占用** - 只保留窗口大小的数据
3. **无状态策略** - 易于测试、调试、回滚
4. **回测/实盘一致** - 统一的 FactorProvider 接口
5. **模块化** - 因子计算与策略逻辑分离
6. **可扩展** - 新增因子只需添加 Node

### 实现步骤

```
Step 1: 实现 RingBuffer 和 RollingStats
Step 2: 实现各个 Node (OBI, ZScore, Volatility, StopLoss)
Step 3: 组装 FactorGraph
Step 4: 实现无状态 stateless_mm 函数
Step 5: 实现 FactorProvider trait
Step 6: IPC 集成
Step 7: 回测验证
```
