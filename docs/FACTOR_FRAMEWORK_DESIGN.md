# Factor Framework Design

## 1. 核心问题重新定义

### 1.1 数据流分析

```
                    Collector 已持久化
                          │
                          ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Market Data (.npz)                          │
│   Book Snapshots + Trades + Timestamps                          │
└─────────────────────────────────────────────────────────────────┘
                          │
                          │ 确定性函数
                          ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Factor Values                                │
│   alpha = f(book_history)                                        │
│   volatility = g(price_history)                                  │
│   → 可从原始数据完全重建                                          │
└─────────────────────────────────────────────────────────────────┘
```

### 1.2 是否需要数据库？

| 场景 | 需要数据库？ | 原因 |
|------|-------------|------|
| 因子状态恢复 | ❌ | 从原始数据重建 |
| 因子历史查询 | ❌ | 可从原始数据重算或预计算 |
| 订单/成交记录 | ⚠️ 可选 | 可用文件，数据库便于查询 |
| 实时监控 | ❌ | IPC 直接推送 |
| 策略回测 | ❌ | 直接用 .npz |

**结论：数据库是可选的，不是必须的。**

---

## 2. 通用 Factor Framework 设计

### 2.1 核心抽象

```rust
/// 计算图节点 - 通用接口
pub trait Node: Send + Sync {
    /// 节点 ID
    fn id(&self) -> &str;

    /// 输入依赖
    fn dependencies(&self) -> &[&str];

    /// 输出类型
    fn output_type(&self) -> TypeId;

    /// 状态大小（用于序列化）
    fn state_size(&self) -> usize;

    /// 序列化状态（用于 checkpoint）
    fn serialize_state(&self) -> Vec<u8>;

    /// 反序列化状态（用于恢复）
    fn deserialize_state(&mut self, data: &[u8]);
}

/// 流式计算节点
pub trait StreamNode<I, O>: Node {
    /// O(1) 更新
    fn update(&mut self, input: &I) -> O;

    /// 重置状态
    fn reset(&mut self);
}
```

### 2.2 Graph 结构

```rust
pub struct FactorGraph {
    nodes: HashMap<String, Box<dyn AnyNode>>,
    topology: Vec<String>,  // 拓扑排序后的执行顺序
    outputs: HashMap<String, Value>,  // 最新输出缓存
}

impl FactorGraph {
    /// 从配置构建图
    pub fn from_config(config: &GraphConfig) -> Self;

    /// 添加节点
    pub fn add_node<N: Node + 'static>(&mut self, node: N);

    /// 连接节点
    pub fn connect(&mut self, from: &str, to: &str);

    /// 编译（拓扑排序 + 优化）
    pub fn compile(&mut self) -> Result<(), GraphError>;

    /// O(1) 更新整个图
    pub fn update(&mut self, input: &MarketData) -> FactorOutput;

    /// Checkpoint 状态
    pub fn checkpoint(&self) -> GraphState;

    /// 从 Checkpoint 恢复
    pub fn restore(&mut self, state: &GraphState);

    /// 从历史数据重建状态
    pub fn rebuild_from_history(&mut self, data: &[MarketData]);
}
```

### 2.3 内置节点类型

```rust
// ============ Source Nodes ============
pub struct BookSnapshotNode;      // 订单簿快照
pub struct TradeNode;             // 成交数据
pub struct MidPriceNode;          // 中间价

// ============ Transform Nodes ============
pub struct RollingMeanNode;       // 滚动均值
pub struct RollingStdNode;        // 滚动标准差
pub struct EMANode;               // 指数移动平均
pub struct ZScoreNode;            // Z-Score 标准化
pub struct DiffNode;              // 差分
pub struct ReturnNode;            // 收益率

// ============ Factor Nodes ============
pub struct OBINode;               // Order Book Imbalance
pub struct VWAPNode;              // VWAP
pub struct TradeFlowNode;         // 交易流
pub struct BookPressureNode;      // 订单簿压力
pub struct SpreadNode;            // 价差

// ============ Signal Nodes ============
pub struct ThresholdNode;         // 阈值判断
pub struct CrossoverNode;         // 交叉信号
pub struct CompositeNode;         // 组合信号
```

---

## 3. 状态恢复策略

### 3.1 三种恢复方式

```
┌─────────────────────────────────────────────────────────────────┐
│                     State Recovery Options                       │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Option A: Full Rebuild (最简单)                                 │
│  ─────────────────────────────────                               │
│  启动时从头遍历历史数据，重建所有状态                             │
│  - 优点：无需额外存储，保证一致性                                 │
│  - 缺点：启动慢（取决于窗口大小）                                 │
│  - 适用：窗口较小（< 1小时）                                      │
│                                                                  │
│  Option B: Checkpoint (推荐)                                     │
│  ─────────────────────────────                                   │
│  定期保存增量状态到文件                                           │
│  - 优点：启动快，只需重放 checkpoint 后的数据                     │
│  - 缺点：需要额外存储空间                                         │
│  - 适用：大窗口因子                                               │
│                                                                  │
│  Option C: Hybrid (最佳)                                         │
│  ─────────────────────────                                       │
│  Checkpoint + 增量重放                                            │
│  - 每 N 分钟保存 checkpoint                                       │
│  - 启动时加载最近 checkpoint + 重放后续数据                       │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### 3.2 Checkpoint 实现

```rust
#[derive(Serialize, Deserialize)]
pub struct GraphCheckpoint {
    pub timestamp: i64,
    pub node_states: HashMap<String, Vec<u8>>,
    pub metadata: CheckpointMetadata,
}

impl FactorGraph {
    /// 创建 checkpoint
    pub fn checkpoint(&self) -> GraphCheckpoint {
        GraphCheckpoint {
            timestamp: self.last_timestamp,
            node_states: self.nodes.iter()
                .map(|(id, node)| (id.clone(), node.serialize_state()))
                .collect(),
            metadata: CheckpointMetadata {
                version: VERSION,
                node_count: self.nodes.len(),
            },
        }
    }

    /// 保存 checkpoint 到文件
    pub fn save_checkpoint(&self, path: &Path) -> io::Result<()> {
        let checkpoint = self.checkpoint();
        let data = bincode::serialize(&checkpoint)?;
        fs::write(path, data)
    }

    /// 从 checkpoint 恢复
    pub fn load_checkpoint(&mut self, path: &Path) -> io::Result<i64> {
        let data = fs::read(path)?;
        let checkpoint: GraphCheckpoint = bincode::deserialize(&data)?;

        for (id, state) in checkpoint.node_states {
            if let Some(node) = self.nodes.get_mut(&id) {
                node.deserialize_state(&state);
            }
        }

        Ok(checkpoint.timestamp)
    }

    /// 混合恢复：checkpoint + 增量重放
    pub fn recover(&mut self, checkpoint_dir: &Path, data_dir: &Path) -> Result<()> {
        // 1. 找到最近的 checkpoint
        let checkpoint_path = find_latest_checkpoint(checkpoint_dir)?;

        // 2. 加载 checkpoint
        let checkpoint_ts = self.load_checkpoint(&checkpoint_path)?;

        // 3. 重放 checkpoint 之后的数据
        let data_files = find_data_after(data_dir, checkpoint_ts)?;
        for file in data_files {
            let data = load_npz(&file)?;
            for event in data.iter() {
                self.update(event);
            }
        }

        Ok(())
    }
}
```

---

## 4. 回测/实盘统一架构

### 4.1 统一接口

```rust
/// 市场数据源 - 回测/实盘统一
pub trait MarketDataSource {
    fn next(&mut self) -> Option<MarketData>;
    fn timestamp(&self) -> i64;
}

/// 因子提供者 - 回测/实盘统一
pub trait FactorSource {
    fn next(&mut self) -> Option<FactorOutput>;
}

/// 订单执行器 - 回测/实盘统一
pub trait OrderExecutor {
    fn submit(&mut self, order: OrderRequest) -> Result<OrderId>;
    fn cancel(&mut self, order_id: OrderId) -> Result<()>;
    fn position(&self) -> f64;
    fn orders(&self) -> &HashMap<OrderId, Order>;
}
```

### 4.2 实现

```rust
// ============ 回测实现 ============
pub struct BacktestMarketSource {
    reader: DataReader,
}

pub struct BacktestFactorSource {
    market_source: BacktestMarketSource,
    graph: FactorGraph,
}

pub struct BacktestExecutor {
    // hftbacktest 内部
}

// ============ 实盘实现 ============
pub struct LiveMarketSource {
    ipc_rx: IpcReceiver<MarketData>,
}

pub struct LiveFactorSource {
    /// Option A: 内置计算
    market_source: LiveMarketSource,
    graph: FactorGraph,

    /// Option B: 外部 Factor Engine
    // factor_rx: IpcReceiver<FactorOutput>,
}

pub struct LiveExecutor {
    ipc_tx: IpcSender<OrderRequest>,
    // ...
}
```

### 4.3 策略代码 - 完全统一

```rust
/// 策略运行器 - 回测/实盘通用
pub fn run_strategy<M, F, E>(
    market: &mut M,
    factors: &mut F,
    executor: &mut E,
    params: &StrategyParams,
) -> Result<()>
where
    M: MarketDataSource,
    F: FactorSource,
    E: OrderExecutor,
{
    while let Some(factor_output) = factors.next() {
        let input = StrategyInput {
            alpha: factor_output.alpha,
            volatility: factor_output.volatility,
            stop_loss: factor_output.stop_loss,
            best_bid: factor_output.best_bid,
            best_ask: factor_output.best_ask,
            position: executor.position(),
            active_orders: executor.orders().values().cloned().collect(),
            params: params.clone(),
        };

        let output = stateless_mm(&input);

        for order_id in output.cancel_orders {
            executor.cancel(order_id)?;
        }
        for order in output.new_bid_orders {
            executor.submit(order)?;
        }
        for order in output.new_ask_orders {
            executor.submit(order)?;
        }
    }
    Ok(())
}
```

---

## 5. 配置驱动的图定义

### 5.1 TOML 配置

```toml
[graph]
name = "obi_mm_factors"
version = "1.0"

# ============ 节点定义 ============

[[nodes]]
id = "mid_price"
type = "MidPrice"

[[nodes]]
id = "obi"
type = "OBI"
[nodes.config]
looking_depth = 0.001

[[nodes]]
id = "obi_zscore"
type = "ZScore"
inputs = ["obi"]
[nodes.config]
window = 100

[[nodes]]
id = "returns"
type = "Return"
inputs = ["mid_price"]

[[nodes]]
id = "volatility"
type = "RollingStd"
inputs = ["returns"]
[nodes.config]
window = 60

[[nodes]]
id = "stop_loss"
type = "Threshold"
inputs = ["volatility", "returns"]
[nodes.config]
volatility_threshold = 3.0
change_threshold = 3.0

# ============ 输出定义 ============

[outputs]
alpha = "obi_zscore"
volatility = "volatility"
stop_loss = "stop_loss"

# ============ Checkpoint 配置 ============

[checkpoint]
enabled = true
interval_seconds = 300
dir = "./checkpoints"
```

### 5.2 从配置构建图

```rust
impl FactorGraph {
    pub fn from_toml(config: &str) -> Result<Self> {
        let config: GraphConfig = toml::from_str(config)?;

        let mut graph = FactorGraph::new();

        // 添加节点
        for node_config in &config.nodes {
            let node = create_node(&node_config.type_, &node_config.config)?;
            graph.add_node(&node_config.id, node);
        }

        // 连接节点
        for node_config in &config.nodes {
            if let Some(inputs) = &node_config.inputs {
                for input in inputs {
                    graph.connect(input, &node_config.id);
                }
            }
        }

        // 设置输出
        for (name, node_id) in &config.outputs {
            graph.set_output(name, node_id);
        }

        graph.compile()?;
        Ok(graph)
    }
}
```

---

## 6. 性能优化

### 6.1 SIMD 加速

```rust
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

impl RollingStats {
    #[inline]
    #[cfg(target_arch = "x86_64")]
    pub fn update_simd(&mut self, values: &[f64; 4]) -> [(f64, f64); 4] {
        unsafe {
            let v = _mm256_loadu_pd(values.as_ptr());
            // SIMD operations...
        }
    }
}
```

### 6.2 批量更新

```rust
impl FactorGraph {
    /// 批量更新多个事件（减少函数调用开销）
    pub fn update_batch(&mut self, events: &[MarketData]) -> Vec<FactorOutput> {
        events.iter().map(|e| self.update(e)).collect()
    }
}
```

### 6.3 延迟计算

```rust
impl FactorGraph {
    /// 只计算被订阅的输出
    pub fn update_lazy(&mut self, input: &MarketData, needed: &[&str]) -> FactorOutput {
        // 只遍历需要的节点路径
        let active_nodes = self.get_active_nodes(needed);
        for node_id in active_nodes {
            self.update_node(node_id, input);
        }
        self.collect_outputs(needed)
    }
}
```

---

## 7. 完整数据流

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           Complete Data Flow                             │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│   ┌──────────────┐                                                       │
│   │  Exchange    │                                                       │
│   │  WebSocket   │                                                       │
│   └──────┬───────┘                                                       │
│          │                                                               │
│          ▼                                                               │
│   ┌──────────────┐    ┌──────────────┐                                  │
│   │  Connector   │───▶│  Collector   │───▶ .npz files (持久化)          │
│   └──────┬───────┘    └──────────────┘                                  │
│          │                                                               │
│          │ IceOryx2                                                      │
│          ▼                                                               │
│   ┌──────────────────────────────────────┐                              │
│   │         Factor Graph                  │                              │
│   │  ┌────────────────────────────────┐  │                              │
│   │  │ Checkpoint (可选)              │  │                              │
│   │  │ - 每 5 分钟保存状态            │  │                              │
│   │  │ - 启动时恢复                   │  │                              │
│   │  └────────────────────────────────┘  │                              │
│   │                                       │                              │
│   │  Book ──▶ OBI ──▶ ZScore ──▶ alpha   │                              │
│   │                      │               │                              │
│   │  Price ──▶ Return ──▶ Vol ──▶ vol    │                              │
│   │                      │               │                              │
│   │                 StopLoss ──▶ signal  │                              │
│   └──────────────────────┬───────────────┘                              │
│                          │                                               │
│                          │ FactorOutput                                  │
│                          ▼                                               │
│   ┌──────────────────────────────────────┐                              │
│   │       Stateless MM Strategy          │                              │
│   │  f(alpha, vol, signal, pos, params)  │                              │
│   │            │                          │                              │
│   │            ▼                          │                              │
│   │  OrderOutput { cancel[], bid[], ask[] │                              │
│   └──────────────────────┬───────────────┘                              │
│                          │                                               │
│                          ▼                                               │
│   ┌──────────────────────────────────────┐                              │
│   │         Order Executor               │                              │
│   │  - 回测: Backtest Engine             │                              │
│   │  - 实盘: Connector IPC               │                              │
│   └──────────────────────────────────────┘                              │
│                                                                          │
│   ════════════════════════════════════════════════════════════════      │
│                     Optional: Recording Service                          │
│   ════════════════════════════════════════════════════════════════      │
│                          │                                               │
│                          ▼                                               │
│   ┌──────────────┐  ┌──────────────┐  ┌──────────────┐                  │
│   │ Order Log    │  │ Trade Log    │  │ PnL Log      │                  │
│   │ (.jsonl)     │  │ (.jsonl)     │  │ (.jsonl)     │                  │
│   └──────────────┘  └──────────────┘  └──────────────┘                  │
│          │                 │                 │                           │
│          └─────────────────┼─────────────────┘                           │
│                            ▼                                             │
│                    ┌──────────────┐                                      │
│                    │  可选: DB    │                                      │
│                    │  ClickHouse  │                                      │
│                    └──────────────┘                                      │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 8. 总结

### 回答你的问题

**Q1: 能扩展成通用框架吗？**

✅ **可以**。通过：
- `Node` trait 抽象任意计算节点
- `FactorGraph` 管理节点拓扑和执行
- 配置驱动的图定义 (TOML)
- 回测/实盘统一的 trait 接口

**Q2: 是否需要数据库？**

❌ **不需要**（对于因子计算）。因为：
- Collector 已经持久化了原始数据 (.npz)
- 因子是原始数据的确定性函数
- 可通过 Checkpoint + 增量重放恢复状态

⚠️ **可选**（对于订单/交易记录）：
- 简单场景：.jsonl 文件足够
- 需要查询分析：ClickHouse
- 实时监控：IPC 直接推送

### 最终架构

| 组件 | 存储方式 | 原因 |
|------|----------|------|
| 市场数据 | .npz (Collector) | 已有 |
| 因子状态 | Checkpoint 文件 | 可从原始数据重建 |
| 订单记录 | .jsonl 或 DB | 可选，便于分析 |
| 实时数据 | IPC (内存) | 低延迟 |
