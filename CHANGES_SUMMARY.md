# 改动总结

本文档总结了本次开发会话中的所有改动。

## 1. Binance Futures OrderBook 同步机制

### 背景
按照 Binance 官方文档实现正确的订单簿同步逻辑，包括：
- 事件缓冲（在获取快照期间）
- UpdateId 验证：`U <= lastUpdateId AND u > lastUpdateId`
- 连续性检查：`pu == previous_u`

### 新增文件

#### `connector/src/binancefutures/orderbookmanager.rs`
完整的订单簿同步管理器，包含：
- `SyncState` 枚举：`Initializing`, `WaitingForSnapshot`, `Synced`
- `SymbolOrderBook` 结构：管理单个交易对的同步状态
- `OrderBookManager` 结构：管理多个交易对
- `DepthEvent` 枚举：`Process`, `Skip`, `Buffer`
- 6 个单元测试验证同步逻辑

```rust
pub struct OrderBookManager {
    books: HashMap<String, SymbolOrderBook>,
}

impl OrderBookManager {
    pub fn start_init(&mut self, symbol: &str);
    pub fn on_depth_update(&mut self, depth: &stream::Depth) -> (bool, bool);
    pub fn on_snapshot(&mut self, symbol: &str, snapshot: &rest::Depth) -> Vec<DepthEvent>;
    pub fn trigger_resync(&mut self, symbol: &str);
}
```

#### `connector/src/lib.rs`
新增库入口文件，导出模块供外部使用：
```rust
#[cfg(feature = "binancefutures")]
pub mod binancefutures;
pub mod connector;
pub mod utils;
```

### 修改文件

#### `connector/src/binancefutures/mod.rs`
将内部模块改为公开：
```rust
pub mod msg;
pub mod orderbookmanager;
pub mod rest;
```

#### `connector/src/utils.rs`
添加通用解析错误类型（避免 feature 依赖问题）：
```rust
#[derive(Error, Debug)]
pub enum ParseError {
    #[error("Invalid price or quantity: {0}")]
    InvalidPxQty(#[from] ParseFloatError),
}

pub fn parse_depth(...) -> Result<(Vec<PxQty>, Vec<PxQty>), ParseError>
pub fn parse_px_qty_tup(...) -> Result<PxQty, ParseError>
```

#### `connector/src/bybit/mod.rs`
添加错误类型转换：
```rust
impl From<ParseError> for BybitError {
    fn from(err: ParseError) -> Self {
        match err {
            ParseError::InvalidPxQty(e) => BybitError::InvalidPxQty(e),
        }
    }
}
```

---

## 2. Collector 复用 Connector 代码

### 修改文件

#### `collector/Cargo.toml`
添加 connector 作为依赖：
```toml
connector = { path = "../connector", default-features = false, features = ["binancefutures"] }
```

#### `collector/src/binancefuturesum/mod.rs`
重构为使用 connector 的 OrderBookManager：

```rust
use connector::binancefutures::{
    msg::{rest, stream},
    orderbookmanager::OrderBookManager,
};

// 新增 SnapshotData 结构用于内部通道
struct SnapshotData {
    symbol: String,
    json: String,
}

// 修改 handle 函数签名，添加 snapshot_tx 参数
fn handle(
    order_book_manager: &mut OrderBookManager,
    writer_tx: &UnboundedSender<(DateTime<Utc>, String, String)>,
    snapshot_tx: &UnboundedSender<SnapshotData>,
    recv_time: DateTime<Utc>,
    data: Utf8Bytes,
    throttler: &Throttler,
) -> Result<(), ConnectorError>

// 修改 run_collection 使用 tokio::select! 处理 WebSocket 和快照
pub async fn run_collection(...) {
    loop {
        tokio::select! {
            ws_data = ws_rx.recv() => { ... }
            snapshot_data = snapshot_rx.recv() => {
                // 现在正确调用 handle_snapshot
                handle_snapshot(&mut order_book_manager, &snapshot.symbol, &snapshot.json)
            }
        }
    }
}
```

**修复的问题**：之前 `handle_snapshot` 函数定义了但从未被调用，快照数据没有经过 `OrderBookManager.on_snapshot()` 处理。

---

## 3. Rust Tardis 数据转换器

### 背景
用 Rust 重写 Python 的 `py-hftbacktest/hftbacktest/data/utils/tardis.py`，用于将 Tardis.dev 格式的 CSV 数据转换为 HftBacktest 使用的 Event 格式。

### 新增文件

#### `hftbacktest/src/data/mod.rs`
模块入口：
```rust
pub use tardis::{convert, convert_tardis, TardisConvertConfig, SnapshotMode};
#[cfg(feature = "backtest")]
pub use tardis::{write_npz_file, convert_and_save};
pub use validation::{
    correct_local_timestamp, correct_event_order, validate_event_order,
    argsort_by_exch_ts, argsort_by_local_ts,
};
```

#### `hftbacktest/src/data/validation.rs`
事件顺序验证和校正：
```rust
/// 校正负延迟
pub fn correct_local_timestamp(data: &mut [Event], base_latency: i64) -> i64;

/// 校正事件顺序，添加 EXCH_EVENT/LOCAL_EVENT 标志
pub fn correct_event_order(
    data: &[Event],
    sorted_exch_indices: &[usize],
    sorted_local_indices: &[usize],
) -> Vec<Event>;

/// 验证事件顺序正确性
pub fn validate_event_order(data: &[Event]) -> Result<(), String>;

/// 按交易所时间戳排序的索引
pub fn argsort_by_exch_ts(data: &[Event]) -> Vec<usize>;

/// 按本地时间戳排序的索引
pub fn argsort_by_local_ts(data: &[Event]) -> Vec<usize>;
```

#### `hftbacktest/src/data/tardis.rs`
Tardis CSV 转换器：
```rust
pub enum SnapshotMode {
    Process,    // 处理所有快照
    IgnoreSod,  // 忽略 Start-of-Day 快照
    Ignore,     // 忽略所有快照
}

pub struct TardisConvertConfig {
    pub buffer_size: usize,      // 默认 100_000_000
    pub ss_buffer_size: usize,   // 默认 1_000_000
    pub base_latency: i64,       // 默认 0
    pub snapshot_mode: SnapshotMode,
}

/// 转换 Tardis CSV 文件
pub fn convert_tardis<P: AsRef<Path>>(
    input_files: &[P],
    config: &TardisConvertConfig,
) -> Result<Vec<Event>, String>;

/// 转换并保存为 NPZ
#[cfg(feature = "backtest")]
pub fn convert_and_save<P: AsRef<Path>>(
    input_files: &[P],
    output_path: P,
    config: &TardisConvertConfig,
) -> Result<Vec<Event>, String>;

/// 写入 NPZ 文件
#[cfg(feature = "backtest")]
pub fn write_npz_file<P: AsRef<Path>>(events: &[Event], output_path: P) -> Result<(), String>;
```

支持的文件类型：
- `trades.csv[.gz]` - 交易数据
- `incremental_book_L2.csv[.gz]` - 深度更新数据

#### `hftbacktest/examples/tardis_convert.rs`
CLI 示例：
```bash
# 转换文件
cargo run --release --example tardis_convert -- trades.csv.gz depth.csv.gz -o output.npz

# 生成测试数据并转换
cargo run --release --example tardis_convert -- --generate-sample
```

#### `hftbacktest/examples/compare_tardis_converters.py`
Python 和 Rust 转换器对比脚本，验证输出一致性和性能。

### 修改文件

#### `hftbacktest/src/lib.rs`
添加 data 模块：
```rust
/// Provides data conversion utilities.
pub mod data;
```

#### `hftbacktest/Cargo.toml`
添加 flate2 依赖（用于 gzip 解压）：
```toml
flate2 = "1.1"
```

---

## 4. 测试验证

### Connector 测试
```bash
cargo test -p connector
# 11 passed: orderbookmanager (6) + utils (5)
```

### Hftbacktest 数据模块测试
```bash
cargo test -p hftbacktest data::
# 12 passed: tardis (7) + validation (5)
```

### 一致性验证
```bash
# 先运行 Rust
cargo run --release --example tardis_convert -- --generate-sample

# 再运行 Python 对比
python hftbacktest/examples/compare_tardis_converters.py
```

结果：
```
✓ Event counts match (73,602 events)
✓ ev values match
✓ exch_ts values match
✓ local_ts values match
✓ px values match
✓ qty values match
✓ All values match! Converters are consistent.
```

---

## 5. 性能对比

| 转换器 | 首次运行 | JIT/热缓存后 | 速度 |
|--------|----------|--------------|------|
| Python | 1.838s | 0.011s | 6.97M events/sec |
| Rust | 0.033s | 0.033s | 2.22M events/sec |

**注意**：Python 使用 Polars（底层 Rust）+ Numba JIT，在 JIT 编译后性能优于当前 Rust 实现。Rust 实现可通过使用 `csv` 或 `polars-rs` crate 进一步优化。

---

## 6. 文件变更清单

### 新增文件
```
connector/src/lib.rs
connector/src/binancefutures/orderbookmanager.rs
connector/examples/binancefutures_orderbook_test.rs
hftbacktest/src/data/mod.rs
hftbacktest/src/data/tardis.rs
hftbacktest/src/data/validation.rs
hftbacktest/examples/tardis_convert.rs
hftbacktest/examples/compare_tardis_converters.py
```

### 修改文件
```
connector/Cargo.toml
connector/src/binancefutures/mod.rs
connector/src/binancefutures/market_data_stream.rs
connector/src/utils.rs
connector/src/bybit/mod.rs
connector/src/main.rs
collector/Cargo.toml
collector/src/binancefuturesum/mod.rs
hftbacktest/Cargo.toml
hftbacktest/src/lib.rs
```

---

## 7. Collector 数据转换器 (新增)

### 背景
实现 Collector JSON 输出到 HftBacktest Event 格式的完整转换流程，跳过 Tardis CSV 中间格式的手动处理。

### 新增文件

#### `hftbacktest/src/data/collector.rs`
Collector JSON 转换器：
```rust
pub struct CollectorConvertConfig {
    pub include_snapshots: bool,  // 是否包含快照
    pub exchange: String,         // 交易所名称，默认 "binance-futures"
}

pub struct ConversionStats {
    pub trades: usize,
    pub depth_updates: usize,
    pub snapshot_entries: usize,
    pub book_ticker_count: usize,
    pub errors: usize,
}

/// 将 Collector JSON 转换为 Tardis CSV 格式
pub fn convert_to_tardis_csv<P1, P2, P3>(
    input_files: &[P1],
    trades_output: P2,
    depth_output: P3,
    config: &CollectorConvertConfig,
) -> Result<ConversionStats, String>;

/// 将 Collector JSON 直接转换为 Event 格式
#[cfg(feature = "backtest")]
pub fn convert_collector_to_events<P: AsRef<Path>>(
    input_files: &[P],
    output_path: Option<P>,
    config: &CollectorConvertConfig,
) -> Result<(Vec<Event>, ConversionStats), String>;
```

支持的数据类型：
- `@trade` - 交易数据
- `@depth@0ms` - 深度更新
- REST 快照 - 订单簿快照

#### `hftbacktest/examples/convert_collector_data.rs`
简单的数据转换和验证工具：
```bash
cargo run --release --example convert_collector_data -- <input.gz> [output.npz]
```

#### `hftbacktest/examples/collector_e2e_test.rs`
完整的端到端测试脚本，包含：
- 自动启动 collector 收集数据
- 转换为 Tardis CSV 和 Event NPZ
- 数据验证（时间戳、价格、订单簿一致性）

### 修改文件

#### `hftbacktest/src/data/mod.rs`
添加 collector 模块导出：
```rust
pub use collector::{CollectorConvertConfig, ConversionStats, convert_to_tardis_csv};
#[cfg(feature = "backtest")]
pub use collector::convert_collector_to_events;
```

#### `hftbacktest/Cargo.toml`
添加依赖：
```toml
serde_json = "1.0"
serde = { version = "1.0.228", features = ["derive"] }
tempfile = { version = "3.20", optional = true }  # backtest feature

[dev-dependencies]
chrono = "0.4"
```

---

## 8. 使用示例

### 8.1 数据收集

```bash
# 编译 collector
cargo build --release -p collector

# 收集 Binance Futures BTCUSDT 数据
# 参数: <输出目录> <交易所> <交易对>
./target/release/collector ./data binancefuturesum btcusdt

# 使用 Ctrl+C 停止收集
# 输出文件: ./data/btcusdt_YYYYMMDD.gz
```

### 8.2 数据转换

```bash
# 编译转换工具
cargo build --release --example convert_collector_data

# 转换并验证
cargo run --release --example convert_collector_data -- \
    ./data/btcusdt_20251225.gz \
    ./data/events.npz
```

输出示例：
```
=== Conversion Stats ===
Trades: 114
Depth updates: 12,489
Snapshot entries: 2,000

=== Event Stats ===
Total events: 16,627
Trade events: 118
Depth events: 12,513
Snapshot events: 4,000

=== Price Range (trades) ===
Min: 87,654.60
Max: 87,918.90

=== Latency Stats ===
Min: 11.1 ms
Max: 89.5 ms
Avg: 31.0 ms

=== Orderbook Validation ===
Consistency errors: 0
✓ Data validation passed!
```

### 8.3 端到端测试

```bash
# 自动收集 + 转换 + 验证 (30秒数据)
cargo run --release --example collector_e2e_test -- \
    --duration 30 \
    --output ./test_data \
    --symbol btcusdt

# 使用已有数据（跳过收集）
cargo run --release --example collector_e2e_test -- \
    --skip-collect \
    --output ./test_data
```

### 8.4 在 Python 中使用转换后的数据

```python
import numpy as np

# 加载 NPZ 文件
data = np.load('./data/events.npz')
events = data['data']

print(f"Total events: {len(events)}")
print(f"Dtype: {events.dtype}")
# [('ev', '<u8'), ('exch_ts', '<i8'), ('local_ts', '<i8'),
#  ('px', '<f8'), ('qty', '<f8'), ('order_id', '<u8'),
#  ('ival', '<i8'), ('fval', '<f8')]

# 用于回测
from hftbacktest import BacktestAsset, HashMapMarketDepthBacktest

asset = (
    BacktestAsset()
        .data([events])
        .tick_size(0.1)
        .lot_size(0.001)
        ...
)
```

### 8.5 Tardis CSV 转换 (已有 Tardis 数据)

```bash
# 转换 Tardis.dev 下载的数据
cargo run --release --example tardis_convert -- \
    trades.csv.gz \
    incremental_book_L2.csv.gz \
    -o output.npz

# 生成示例数据并测试
cargo run --release --example tardis_convert -- --generate-sample
```

---

## 9. 完整数据流

```
┌─────────────────────────────────────────────────────────────────┐
│                     Binance Futures WebSocket                    │
│         @trade / @depth@0ms / @bookTicker + REST snapshot        │
└───────────────────────────────┬─────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                          Collector                               │
│  - OrderBookManager 同步订单簿                                    │
│  - 按日期轮转 gzip 压缩存储                                       │
│  - 输出格式: {timestamp_nanos} {json}                            │
└───────────────────────────────┬─────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Collector Converter                           │
│  collector.rs: JSON → Tardis CSV → Event[]                       │
│  - 解析 trade, depth, snapshot                                   │
│  - 校正延迟和事件顺序                                             │
│  - 输出 .npz 文件                                                │
└───────────────────────────────┬─────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                      HftBacktest                                 │
│  - BacktestAsset.data([events])                                  │
│  - 支持回测和策略开发                                             │
└─────────────────────────────────────────────────────────────────┘
```

---

## 10. 文件变更清单 (更新)

### 新增文件
```
connector/src/lib.rs
connector/src/binancefutures/orderbookmanager.rs
connector/examples/binancefutures_orderbook_test.rs
hftbacktest/src/data/mod.rs
hftbacktest/src/data/tardis.rs
hftbacktest/src/data/validation.rs
hftbacktest/src/data/collector.rs                    # 新增
hftbacktest/examples/tardis_convert.rs
hftbacktest/examples/compare_tardis_converters.py
hftbacktest/examples/convert_collector_data.rs       # 新增
hftbacktest/examples/collector_e2e_test.rs           # 新增
```

### 修改文件
```
connector/Cargo.toml
connector/src/binancefutures/mod.rs
connector/src/binancefutures/market_data_stream.rs
connector/src/utils.rs
connector/src/bybit/mod.rs
connector/src/main.rs
collector/Cargo.toml
collector/src/binancefuturesum/mod.rs
hftbacktest/Cargo.toml                               # 更新
hftbacktest/src/lib.rs
hftbacktest/src/data/mod.rs                          # 更新
```

---

## 11. 后续工作

1. ~~**Collector JSON → Event 转换器**~~：✓ 已完成

2. **Rust 性能优化**：使用 `csv` 或 `polars-rs` crate 提升 CSV 解析性能。

3. ~~**集成测试**~~：✓ 已完成，使用真实 Binance 数据验证了完整流程。

4. **长时间数据收集测试**：验证 collector 在长时间运行下的稳定性和数据完整性。

5. **多交易对支持**：测试同时收集多个交易对的数据。
