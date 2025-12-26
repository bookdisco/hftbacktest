# 改动总结

本文档索引所有开发改动记录。详细改动请查看 `changelogs/` 目录。

## 改动历史

| 日期 | 文件 | 主要内容 |
|------|------|----------|
| 2025-12-26 | [changelogs/2025-12-26.md](changelogs/2025-12-26.md) | Factor Engine, OBI MM 策略, Order Recorder 模块 |
| 2025-12-25 | [changelogs/2025-12-25.md](changelogs/2025-12-25.md) | OrderBook 同步, Collector, PAPI Connector, 优雅关闭 |

## 最新改动 (2025-12-26)

### Order Recording Module (order-recorder)

新增订单录制模块，用于记录实盘交易活动到 ClickHouse：

- **IPC 通信**: iceoryx2 零拷贝进程间通信
- **ClickHouse 存储**: 异步批量写入
- **Order Hook**: `OrderHookContext` 集成到 `LiveBotBuilder`
- **延迟跟踪**: 记录订单往返延迟

```rust
use order_recorder::{StrategyState, hook::create_order_hook};

let hook_ctx = create_order_hook("hft_recording", "strategy_id".into(), "btcusdt".into(), 0.1);

if let Some(ctx) = hook_ctx.clone() {
    builder = builder.order_recv_hook(move |prev, new| {
        ctx.handle_order(prev, new);
        Ok(())
    });
}
```

### Factor Engine & OBI MM 策略

- 新增 `factor-engine` crate 用于因子计算
- OBI MM 策略从 Python 迁移到 Rust
- 集成 testnet 实盘交易

## 关键模块

| 模块 | 路径 | 描述 |
|------|------|------|
| order-recorder | `order-recorder/` | 订单录制和分析 |
| factor-engine | `factor-engine/` | 因子计算引擎 |
| obi-mm | `strategies/obi_mm_rust/` | OBI 做市策略 |
| connector | `connector/` | 交易所连接器 |
| collector | `collector/` | 市场数据收集 |
