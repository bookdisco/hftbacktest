# HftBacktest 实盘交易架构文档

本文档详细介绍 HftBacktest 框架的实盘交易系统架构、关键组件和运行机制。

## 目录

1. [架构总览](#架构总览)
2. [核心组件](#核心组件)
3. [入口文件索引](#入口文件索引)
4. [WebSocket 连接稳定性机制](#websocket-连接稳定性机制)
5. [数据流协议](#数据流协议)
6. [运行实盘](#运行实盘)
7. [关键设计要点](#关键设计要点)
8. [IceOryx2 共享内存 IPC](#iceoryx2-共享内存-ipc)

---

## 架构总览

HftBacktest 实盘系统采用**双进程架构**：Bot 进程（用户策略）和 Connector 进程（交易所连接）通过 IPC 通信。

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           用户策略进程 (Bot)                              │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │  LiveBot<IceoryxUnifiedChannel, MD>                              │   │
│  │  • 实现 Bot trait                                                │   │
│  │  • 管理 Instrument (depth, orders, position)                     │   │
│  │  • 通过 Channel 发送 LiveRequest / 接收 LiveEvent                │   │
│  └───────────────────────────────┬─────────────────────────────────┘   │
└──────────────────────────────────┼──────────────────────────────────────┘
                                   │
                         IceOryx2 IPC (共享内存)
                         ├── {name}/ToBot   (Connector → Bot)
                         └── {name}/FromBot (Bot → Connector)
                                   │
┌──────────────────────────────────┼──────────────────────────────────────┐
│                           Connector 进程                                 │
│  ┌───────────────────────────────┴─────────────────────────────────┐   │
│  │  main.rs                                                         │   │
│  │  • run_receive_task(): 接收 Bot 请求 (订单提交/取消)              │   │
│  │  • run_publish_task(): 发布事件给所有 Bot                        │   │
│  └───────────────────────────────┬─────────────────────────────────┘   │
│                                  │                                      │
│  ┌───────────────────────────────┴─────────────────────────────────┐   │
│  │  Connector (BinanceFutures/Bybit/BinanceSpot)                    │   │
│  │  • market_data_stream: 订阅行情 WebSocket                        │   │
│  │  • user_data_stream: 订阅用户数据 WebSocket                      │   │
│  │  • REST Client: 下单/撤单/查询                                   │   │
│  │  • OrderManager: 管理订单状态                                    │   │
│  └─────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
                                   │
                              WebSocket/REST
                                   │
                    ┌──────────────┴──────────────┐
                    │         交易所              │
                    │  (Binance/Bybit/...)        │
                    └─────────────────────────────┘
```

---

## 核心组件

### 1. LiveBot (`hftbacktest/src/live/bot.rs`)

用户策略的核心接口，实现了 `Bot<MD>` trait。

```rust
pub struct LiveBot<CH, MD> {
    id: u64,                           // Bot 唯一标识
    channel: CH,                       // IPC 通道
    instruments: Vec<Instrument<MD>>,  // 交易品种列表
    error_handler: Option<ErrorHandler>,
    order_hook: Option<OrderRecvHook>,
}
```

**主要方法**：
- `elapse(duration)` - 等待事件或超时
- `submit_buy_order()` / `submit_sell_order()` - 提交订单
- `cancel()` - 取消订单
- `depth(asset_no)` - 获取市场深度
- `position(asset_no)` - 获取持仓
- `orders(asset_no)` - 获取订单列表

### 2. Instrument (`hftbacktest/src/live/mod.rs`)

表示一个交易品种的完整状态。

```rust
pub struct Instrument<MD> {
    connector_name: String,      // Connector 名称
    symbol: String,              // 交易对符号
    tick_size: f64,              // 最小价格变动
    lot_size: f64,               // 最小交易数量
    depth: MD,                   // 市场深度
    last_trades: Vec<Event>,     // 最近成交
    orders: HashMap<OrderId, Order>,  // 订单
    last_feed_latency: Option<(i64, i64)>,   // 行情延迟
    last_order_latency: Option<(i64, i64, i64)>,  // 订单延迟
    state: StateValues,          // 状态值
}
```

### 3. Connector Trait (`connector/src/connector.rs`)

交易所连接器的抽象接口。

```rust
pub trait Connector {
    /// 注册交易品种
    fn register(&mut self, symbol: String);

    /// 获取订单管理器
    fn order_manager(&self) -> Arc<Mutex<dyn GetOrders + Send + 'static>>;

    /// 启动连接器（非阻塞）
    fn run(&mut self, tx: UnboundedSender<PublishEvent>);

    /// 提交订单（非阻塞）
    fn submit(&self, symbol: String, order: Order, tx: UnboundedSender<PublishEvent>);

    /// 取消订单（非阻塞）
    fn cancel(&self, symbol: String, order: Order, tx: UnboundedSender<PublishEvent>);
}
```

### 4. IceoryxUnifiedChannel (`hftbacktest/src/live/ipc/iceoryx.rs`)

基于 iceoryx2 的 IPC 通道实现。

```rust
pub struct IceoryxUnifiedChannel {
    channel: Vec<Rc<IceoryxChannel<LiveRequest, LiveEvent>>>,
    unique_channel: Vec<Rc<IceoryxChannel<LiveRequest, LiveEvent>>>,
    ch_i: usize,
    node: Node<ipc::Service>,
}
```

**特点**：
- 零拷贝共享内存通信
- 支持多 Bot 连接同一 Connector
- 使用 bincode 序列化

---

## 入口文件索引

| 文件路径 | 作用 | 关键类/函数 |
|---------|------|------------|
| `hftbacktest/src/live/mod.rs` | Live 模块入口 | `Instrument`, `LiveBot`, `LiveBotBuilder` |
| `hftbacktest/src/live/bot.rs` | Bot 实现 | `LiveBot<CH, MD>`, `impl Bot<MD>` |
| `hftbacktest/src/live/ipc/iceoryx.rs` | IPC 通道 | `IceoryxUnifiedChannel`, `IceoryxSender`, `IceoryxReceiver` |
| `hftbacktest/src/live/ipc/config.rs` | IPC 配置 | `ChannelConfig`, `MAX_PAYLOAD_SIZE` |
| `connector/src/main.rs` | Connector 主入口 | `run_receive_task()`, `run_publish_task()` |
| `connector/src/connector.rs` | Connector trait | `Connector`, `ConnectorBuilder`, `PublishEvent` |
| `connector/src/binancefutures/mod.rs` | Binance Futures | `BinanceFutures`, `Config` |
| `connector/src/binancefutures/market_data_stream.rs` | 行情流 | `MarketDataStream::connect()` |
| `connector/src/binancefutures/user_data_stream.rs` | 用户数据流 | `UserDataStream`, Listen Key 管理 |
| `connector/src/binancefutures/rest.rs` | REST API | `BinanceFuturesClient` |
| `connector/src/binancefutures/ordermanager.rs` | 订单管理 | `OrderManager` |
| `connector/src/bybit/mod.rs` | Bybit 实现 | `Bybit` |
| `connector/src/binancespot/mod.rs` | Binance Spot | `BinanceSpot` |
| `connector/src/utils.rs` | 工具函数 | `ExponentialBackoff`, `Retry` |

---

## WebSocket 连接稳定性机制

### 1. 指数退避重试策略

**位置**: `connector/src/utils.rs:175-277`

```rust
pub struct ExponentialBackoff {
    last_attempt: Instant,
    factor: u32,                    // 倍数因子 (默认 2)
    last_delay: Option<Duration>,
    reset_interval: Option<Duration>,  // 重置间隔 (默认 300s)
    min_delay: Duration,            // 最小延迟 (默认 100ms)
    max_delay: Option<Duration>,    // 最大延迟 (默认 60s)
}
```

**行为**：
- 首次失败等待 100ms
- 每次失败后延迟翻倍：100ms → 200ms → 400ms → ... → 60s (最大)
- 连接稳定超过 5 分钟后，重置退避计数

**重试循环**：
```rust
pub async fn retry<F, Fut>(&mut self, func: F) -> Result<O, E> {
    loop {
        match func().await {
            Ok(o) => return Ok(o),
            Err(error) => {
                if let Some(error_handler) = self.error_handler.as_mut() {
                    error_handler(error)?;  // 通知错误（发送 LiveEvent::Error）
                }
                tokio::time::sleep(self.backoff.backoff()).await;  // 等待后重试
            }
        }
    }
}
```

### 2. Ping/Pong 心跳检测

**位置**: `connector/src/binancefutures/market_data_stream.rs:265-337`

```rust
pub async fn connect(&mut self, url: &str) -> Result<(), BinanceFuturesError> {
    let mut ping_checker = time::interval(Duration::from_secs(10));
    let mut last_ping = Instant::now();

    loop {
        select! {
            // 每 10 秒检查一次心跳
            _ = ping_checker.tick() => {
                if last_ping.elapsed() > Duration::from_secs(300) {
                    warn!("Ping timeout.");
                    return Err(BinanceFuturesError::ConnectionInterrupted);
                }
            }

            // 处理 WebSocket 消息
            message = read.next() => match message {
                // 收到 Ping，回复 Pong 并更新时间戳
                Some(Ok(Message::Ping(data))) => {
                    write.send(Message::Pong(data)).await?;
                    last_ping = Instant::now();
                }

                // 收到关闭帧
                Some(Ok(Message::Close(close_frame))) => {
                    return Err(BinanceFuturesError::ConnectionAbort(...));
                }

                // 连接中断
                None => {
                    return Err(BinanceFuturesError::ConnectionInterrupted);
                }

                // 正常消息处理...
            }
        }
    }
}
```

### 3. 自动重连机制

**位置**: `connector/src/binancefutures/mod.rs:111-187`

```rust
pub fn connect_market_data_stream(&mut self, ev_tx: UnboundedSender<PublishEvent>) {
    tokio::spawn(async move {
        // 使用 Retry 包装，实现无限重连
        let _ = Retry::new(ExponentialBackoff::default())
            .error_handler(|error: BinanceFuturesError| {
                // 1. 记录错误日志
                error!(?error, "An error occurred in the market data stream connection.");

                // 2. 发送错误事件通知 Bot
                ev_tx.send(PublishEvent::LiveEvent(LiveEvent::Error(
                    LiveError::with(ErrorKind::ConnectionInterrupted, error.into())
                ))).unwrap();

                Ok(())  // 返回 Ok 继续重试
            })
            .retry(|| async {
                // 3. 创建新的 WebSocket 连接
                let mut stream = MarketDataStream::new(
                    client.clone(),
                    ev_tx.clone(),
                    symbol_tx.subscribe(),
                );
                stream.connect(&base_url).await?;
                Ok(())
            })
            .await;
    });
}
```

### 4. 用户数据流 Listen Key 管理

Binance 用户数据流需要 Listen Key（有效期 60 分钟）：

```rust
pub fn connect_user_data_stream(&self, ev_tx: UnboundedSender<PublishEvent>) {
    tokio::spawn(async move {
        let _ = Retry::new(ExponentialBackoff::default())
            .retry(|| async {
                // 每次重连都重新获取 Listen Key
                let listen_key = stream.get_listen_key().await?;
                stream.connect(&format!("{base_url}/{listen_key}")).await?;
                Ok(())
            })
            .await;
    });
}
```

### 5. 错误类型

```rust
pub enum BinanceFuturesError {
    InstrumentNotFound,
    InvalidRequest,
    ListenKeyExpired,           // Listen Key 过期
    ConnectionInterrupted,      // 连接中断（心跳超时、None 消息）
    ConnectionAbort(String),    // 收到关闭帧
    ReqError(reqwest::Error),   // REST 请求错误
    OrderError { code, msg },   // 订单错误
    Tunstenite(tungstenite::Error),  // WebSocket 错误
    // ...
}
```

---

## 数据流协议

### Bot → Connector (LiveRequest)

```rust
pub enum LiveRequest {
    /// 注册交易品种，Connector 会订阅相关行情
    RegisterInstrument {
        symbol: String,
        tick_size: f64,
        lot_size: f64,
    },

    /// 订单请求
    /// - order.req = Status::New: 提交新订单
    /// - order.req = Status::Canceled: 取消订单
    Order {
        symbol: String,
        order: Order,
    },
}
```

### Connector → Bot (LiveEvent)

```rust
pub enum LiveEvent {
    /// 行情事件（深度更新、成交）
    Feed {
        symbol: String,
        event: Event,
    },

    /// 订单状态更新
    Order {
        symbol: String,
        order: Order,
    },

    /// 持仓更新
    Position {
        symbol: String,
        qty: f64,
        exch_ts: i64,
    },

    /// 错误事件
    Error(LiveError),

    /// 批量事件开始标记（用于原子更新）
    BatchStart,

    /// 批量事件结束标记
    BatchEnd,
}
```

### Event 标志位

```rust
// 深度事件
LOCAL_BID_DEPTH_EVENT  // 买方深度更新
LOCAL_ASK_DEPTH_EVENT  // 卖方深度更新
DEPTH_CLEAR_EVENT      // 清空深度

// 成交事件
LOCAL_BUY_TRADE_EVENT  // 主动买入成交
LOCAL_SELL_TRADE_EVENT // 主动卖出成交

// BBO 事件
DEPTH_BBO_EVENT        // 最优买卖价更新
```

---

## 运行实盘

### 1. 准备配置文件

**Binance Futures 配置 (config.toml)**:
```toml
stream_url = "wss://fstream.binance.com/ws"
api_url = "https://fapi.binance.com"
api_key = "your_api_key"
secret = "your_secret"
order_prefix = "hft"  # 订单 ID 前缀，用于区分不同策略
```

**Bybit 配置**:
```toml
stream_url = "wss://stream.bybit.com/v5/public/linear"
private_stream_url = "wss://stream.bybit.com/v5/private"
api_url = "https://api.bybit.com"
api_key = "your_api_key"
secret = "your_secret"
order_prefix = "hft"
```

### 2. 启动 Connector

```bash
# 编译
cargo build --release -p connector

# 运行
./target/release/connector <name> <connector_type> <config_path>

# 示例
./target/release/connector my_binance binancefutures ./config.toml
./target/release/connector my_bybit bybit ./bybit_config.toml
```

**参数说明**：
- `name`: Connector 名称，Bot 连接时使用
- `connector_type`: `binancefutures` | `bybit` | `binancespot`
- `config_path`: 配置文件路径

### 3. 运行策略 (Rust)

```rust
use hftbacktest::{
    live::{Instrument, LiveBot, LiveBotBuilder},
    prelude::*,
};

fn main() {
    let mut hbt = LiveBotBuilder::new()
        .register(Instrument::new(
            "my_binance",      // Connector 名称
            "btcusdt",         // 交易对（小写）
            0.1,               // tick_size
            0.001,             // lot_size
            HashMapMarketDepth::new(0.1, 0.001),
            1000,              // last_trades_capacity
        ))
        .error_handler(|error| {
            eprintln!("Error: {:?}", error);
            Ok(())
        })
        .build::<IceoryxUnifiedChannel>()
        .unwrap();

    // 使用与回测相同的策略代码
    gridtrading(&mut hbt, &mut recorder);
}
```

### 4. 运行策略 (Python)

```python
from numba import njit
from hftbacktest import (
    LiveInstrument,
    ROIVectorMarketDepthLiveBot,
    BUY, SELL, GTX, LIMIT
)

@njit
def my_strategy(hbt):
    while hbt.elapse(100_000_000) == 0:
        depth = hbt.depth(0)
        position = hbt.position(0)

        if position < 10:
            hbt.submit_buy_order(0, 1, depth.best_bid, 1.0, GTX, LIMIT, False)

# 创建实盘 Bot
instrument = LiveInstrument(
    "my_binance",  # Connector 名称
    "btcusdt",     # 交易对
    0.1,           # tick_size
    0.001          # lot_size
)
hbt = ROIVectorMarketDepthLiveBot([instrument])

# 运行策略
my_strategy(hbt)
```

---

## 关键设计要点

### 1. 进程分离架构

**优点**：
- Bot 崩溃不影响 Connector，可以快速重启策略
- 多个 Bot 可以共享同一个 Connector
- 连接管理与策略逻辑解耦

**通信方式**：
- IceOryx2 共享内存
- 零拷贝，微秒级延迟

### 2. 批量事件处理

`BatchStart` / `BatchEnd` 标记用于：
- Bot 初始化时获取完整状态快照（订单、持仓、深度）
- 深度更新等需要原子处理的场景

```rust
// Connector 发送批量事件
bot_tx.send(id, &LiveEvent::BatchStart)?;
for order in orders { bot_tx.send(id, &LiveEvent::Order { ... })?; }
for depth in depths { bot_tx.send(id, &LiveEvent::Feed { ... })?; }
bot_tx.send(id, &LiveEvent::BatchEnd)?;
```

```rust
// Bot 处理批量事件
match self.channel.recv_timeout(...) {
    Ok((_, LiveEvent::BatchStart)) => { batch_mode = true; }
    Ok((_, LiveEvent::BatchEnd)) => {
        batch_mode = false;
        if wait_resp_received { return Ok(ElapseResult::Ok); }
    }
    // 批量模式下不中断处理
}
```

### 3. OrderManager 设计

维护 `order_id` (u64) ↔ `client_order_id` (String) 的双向映射：

- 用户使用简单的 u64 order_id
- 交易所使用带前缀的字符串 client_order_id
- 支持订单前缀区分不同策略

### 4. FusedHashMapMarketDepth

融合多源深度数据的市场深度实现：

- 可以同时接收 L1 BBO 和 L2 深度数据
- 自动选择更精确/更新的数据
- 避免数据冲突和覆盖

### 5. 统一的 Bot Trait

回测和实盘使用相同的 `Bot<MD>` trait：

```rust
pub trait Bot<MD> {
    fn elapse(&mut self, duration: i64) -> Result<ElapseResult, Self::Error>;
    fn submit_buy_order(...) -> Result<ElapseResult, Self::Error>;
    fn submit_sell_order(...) -> Result<ElapseResult, Self::Error>;
    fn cancel(...) -> Result<ElapseResult, Self::Error>;
    fn depth(&self, asset_no: usize) -> &MD;
    fn position(&self, asset_no: usize) -> f64;
    fn orders(&self, asset_no: usize) -> &HashMap<OrderId, Order>;
    // ...
}
```

这使得**同一份策略代码可以无修改地用于回测和实盘**。

---

## IceOryx2 共享内存 IPC

### 什么是 IceOryx2

[IceOryx2](https://github.com/eclipse-iceoryx/iceoryx2) 是一个高性能的进程间通信（IPC）框架，采用**零拷贝共享内存**技术。它是 Eclipse IceOryx 项目的 Rust 重写版本，专为低延迟实时系统设计。

### 为什么选择 IceOryx2

| 特性 | IceOryx2 | TCP/Unix Socket | 其他共享内存 |
|------|----------|-----------------|-------------|
| 延迟 | 微秒级 | 毫秒级 | 微秒级 |
| 零拷贝 | ✅ | ❌ | 部分支持 |
| 类型安全 | ✅ (Rust) | ❌ | ❌ |
| 多订阅者 | ✅ | 需要额外实现 | 需要额外实现 |
| 发布-订阅模式 | ✅ 原生支持 | ❌ | ❌ |

### 核心概念

```
┌─────────────────────────────────────────────────────────────────┐
│                     共享内存区域                                  │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  Service: "my_connector/ToBot"                           │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐      │   │
│  │  │  Sample 1   │  │  Sample 2   │  │  Sample 3   │ ...  │   │
│  │  │ (LiveEvent) │  │ (LiveEvent) │  │ (LiveEvent) │      │   │
│  │  └─────────────┘  └─────────────┘  └─────────────┘      │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  Service: "my_connector/FromBot"                         │   │
│  │  ┌──────────────┐  ┌──────────────┐                     │   │
│  │  │   Sample 1   │  │   Sample 2   │ ...                 │   │
│  │  │(LiveRequest) │  │(LiveRequest) │                     │   │
│  │  └──────────────┘  └──────────────┘                     │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
         ▲                                          ▲
         │ 读取                                      │ 写入
    ┌────┴────┐                               ┌─────┴─────┐
    │   Bot   │                               │ Connector │
    │(Subscriber)                             │(Publisher)│
    └─────────┘                               └───────────┘
```

**核心组件**：
- **Node**: IPC 节点，管理服务生命周期
- **Service**: 通信服务，定义数据类型和通道配置
- **Publisher**: 发布者，向共享内存写入数据
- **Subscriber**: 订阅者，从共享内存读取数据
- **Sample**: 共享内存中的单个数据块

### HftBacktest 中的实现

**位置**: `hftbacktest/src/live/ipc/iceoryx.rs`

#### 服务命名规则

```rust
// Bot → Connector 通道
ServiceName: "{connector_name}/FromBot"

// Connector → Bot 通道
ServiceName: "{connector_name}/ToBot"
```

#### 发送者 (IceoryxSender)

```rust
pub struct IceoryxSender<T> {
    publisher: Publisher<ipc::Service, [u8], CustomHeader>,
    _t_marker: PhantomData<T>,
}

impl<T: Encode> IceoryxSender<T> {
    pub fn send(&self, id: u64, data: &T) -> Result<(), ChannelError> {
        // 1. 从共享内存"借用"一块空间
        let sample = self.publisher.loan_slice_uninit(MAX_PAYLOAD_SIZE)?;
        let mut sample = unsafe { sample.assume_init() };

        // 2. 使用 bincode 序列化数据到共享内存
        let payload = sample.payload_mut();
        let length = bincode::encode_into_slice(data, payload, config::standard())?;

        // 3. 设置自定义头部（目标 ID 和长度）
        sample.user_header_mut().id = id;
        sample.user_header_mut().len = length;

        // 4. 发送（实际上是标记数据可读）
        sample.send()?;
        Ok(())
    }
}
```

#### 接收者 (IceoryxReceiver)

```rust
pub struct IceoryxReceiver<T> {
    subscriber: Subscriber<ipc::Service, [u8], CustomHeader>,
    _t_marker: PhantomData<T>,
}

impl<T: Decode<()>> IceoryxReceiver<T> {
    pub fn receive(&self) -> Result<Option<(u64, T)>, ChannelError> {
        match self.subscriber.receive()? {
            None => Ok(None),
            Some(sample) => {
                // 1. 读取头部信息
                let id = sample.user_header().id;
                let len = sample.user_header().len;

                // 2. 从共享内存反序列化数据
                let bytes = &sample.payload()[0..len];
                let (decoded, _): (T, usize) =
                    bincode::decode_from_slice(bytes, config::standard())?;

                Ok(Some((id, decoded)))
            }
        }
    }
}
```

#### 自定义头部

```rust
#[repr(C)]
pub struct CustomHeader {
    pub id: u64,    // 目标 Bot ID (0 表示广播给所有)
    pub len: usize, // 消息长度
}
```

#### 配置参数

```rust
// hftbacktest/src/live/ipc/config.rs
pub const MAX_PAYLOAD_SIZE: usize = 65536;  // 64KB 单消息最大大小

pub struct ChannelConfig {
    pub buffer_size: usize,  // 缓冲区大小（消息数量）
    pub max_bots: usize,     // 最大 Bot 数量
}
```

### 零拷贝原理

传统 IPC 数据流：
```
Bot 内存 → 内核缓冲区 → 内核缓冲区 → Connector 内存
           (拷贝 1)       (拷贝 2)
```

IceOryx2 零拷贝数据流：
```
共享内存区域
┌───────────────────┐
│     数据块        │ ← Bot 直接写入
│                   │ ← Connector 直接读取
└───────────────────┘
         ↑
    无需拷贝，只传递指针
```

**关键**：
- 数据始终存在于共享内存中
- 发送方"借用"内存块写入数据
- 接收方直接读取同一内存块
- 只有指针/引用在进程间传递

### 性能特点

1. **微秒级延迟**：无系统调用开销，无内核参与
2. **确定性**：适用于实时系统，延迟可预测
3. **高吞吐**：无拷贝，适合大数据量传输
4. **多订阅者**：一次写入，多个 Bot 同时读取

### 与 Bincode 配合

HftBacktest 使用 [bincode](https://github.com/bincode-org/bincode) 进行序列化：

```rust
// 序列化到共享内存
bincode::encode_into_slice(&live_event, payload, config::standard())?;

// 从共享内存反序列化
let (live_event, _): (LiveEvent, usize) =
    bincode::decode_from_slice(bytes, config::standard())?;
```

**Bincode 特点**：
- 紧凑的二进制格式
- 极快的序列化/反序列化速度
- 与 Rust 类型系统紧密集成

---

## 附录：常见问题

### Q: 如何处理连接断开？

A: 框架自动处理重连，Bot 会收到 `LiveEvent::Error(ConnectionInterrupted)` 事件。可以通过 `error_handler` 记录日志或采取其他措施。

### Q: 订单状态如何更新？

A: 通过 `user_data_stream` WebSocket 接收订单更新，或通过 REST API 响应更新。Connector 内部的 `OrderManager` 维护订单状态。

### Q: 如何确保深度数据一致性？

A: 使用批量事件（BatchStart/BatchEnd）确保原子更新，使用 `FusedHashMapMarketDepth` 融合多源数据。

### Q: Python 和 Rust 策略有什么区别？

A: 接口完全相同，Python 通过 FFI 调用 Rust 代码。性能上 Rust 更快，但 Python 开发更便捷。
