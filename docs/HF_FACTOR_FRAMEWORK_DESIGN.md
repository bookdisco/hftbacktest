# 高频因子计算框架设计方案

基于 HftBacktest 系统构建高频因子计算框架的设计文档。

## 目录

1. [设计目标](#设计目标)
2. [核心架构](#核心架构)
3. [计算图设计](#计算图设计)
4. [流式计算节点](#流式计算节点)
5. [多因子加权组合](#多因子加权组合)
6. [研究环境](#研究环境)
7. [因子分析与优化](#因子分析与优化)
8. [生产部署](#生产部署)
9. [实现示例](#实现示例)

---

## 设计目标

### 核心需求

1. **流式计算**：O(1) 时间复杂度的增量更新
2. **多因子组合**：支持多因子计算并线性加权
3. **研究环境**：支持因子开发、参数优化、历史回测

### 设计原则

```
┌─────────────────────────────────────────────────────────────────┐
│                        设计原则                                  │
│                                                                  │
│  1. O(1) 增量计算 - 每次事件更新的时间复杂度为常数               │
│  2. 状态可序列化 - 所有计算状态可持久化和恢复                    │
│  3. DAG 依赖图   - 因子之间的依赖关系用有向无环图表示            │
│  4. 研发一体化   - 同一套因子代码用于研究和生产                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## 核心架构

### 整体架构

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              数据源                                          │
│   ┌─────────────────────┐   ┌─────────────────────────────────────────────┐ │
│   │  实盘: Connector    │   │  回测: Collector 历史数据                    │ │
│   │  (IceOryx2 IPC)     │   │  (.npz / .gz 文件)                          │ │
│   └──────────┬──────────┘   └──────────────────┬──────────────────────────┘ │
└──────────────┼─────────────────────────────────┼────────────────────────────┘
               │                                  │
               ▼                                  ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                           Event Adapter                                      │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  统一事件接口: EventSource trait                                    │   │
│   │  • LiveEventSource(IPC)  • FileEventSource(.npz)  • JsonSource(.gz) │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
└───────────────────────────────────┬─────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Factor Computation Graph                             │
│                                                                              │
│   ┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌────────────┐   │
│   │SourceNode   │───►│TransformNode│───►│FactorNode  │───►│CombineNode │   │
│   │(Depth/Trade)│    │(RollingStats)    │(OBI/VWAP)   │    │(加权组合)   │   │
│   └─────────────┘    └─────────────┘    └─────────────┘    └────────────┘   │
│                                                                              │
└───────────────────────────────────┬─────────────────────────────────────────┘
                                    │
               ┌────────────────────┼────────────────────┐
               ▼                    ▼                    ▼
┌──────────────────────┐ ┌──────────────────┐ ┌──────────────────────────────┐
│    Strategy Bot      │ │   Research Env   │ │     Factor Store             │
│    (实盘交易)         │ │   (参数优化)      │ │     (因子分析)               │
└──────────────────────┘ └──────────────────┘ └──────────────────────────────┘
```

---

## 计算图设计

### 节点类型层次

```python
# 节点类型定义 (Python + Numba)
from abc import ABC, abstractmethod
from numba import njit
from numba.experimental import jitclass
from numba import types
import numpy as np

class Node(ABC):
    """计算图节点基类"""

    @abstractmethod
    def update(self, event) -> bool:
        """处理事件，返回是否有输出更新"""
        pass

    @abstractmethod
    def value(self) -> float:
        """获取当前输出值"""
        pass

    @abstractmethod
    def serialize_state(self) -> bytes:
        """序列化内部状态"""
        pass

    @abstractmethod
    def deserialize_state(self, state: bytes):
        """反序列化恢复状态"""
        pass


class SourceNode(Node):
    """数据源节点 - 从原始事件提取数据"""
    pass


class TransformNode(Node):
    """变换节点 - 对输入进行统计变换"""
    pass


class FactorNode(Node):
    """因子节点 - 计算具体因子值"""
    pass


class CombineNode(Node):
    """组合节点 - 多因子加权组合"""
    pass
```

### DAG 构建器

```python
class FactorGraph:
    """因子计算图"""

    def __init__(self):
        self.nodes: dict[str, Node] = {}
        self.edges: dict[str, list[str]] = {}  # node_id -> [dependency_ids]
        self.topo_order: list[str] = []

    def add_node(self, node_id: str, node: Node, dependencies: list[str] = None):
        """添加节点及其依赖"""
        self.nodes[node_id] = node
        self.edges[node_id] = dependencies or []
        self._rebuild_topo_order()

    def _rebuild_topo_order(self):
        """重建拓扑排序"""
        # Kahn's algorithm
        in_degree = {n: 0 for n in self.nodes}
        for deps in self.edges.values():
            for dep in deps:
                if dep in in_degree:
                    in_degree[dep] += 1

        queue = [n for n, d in in_degree.items() if d == 0]
        self.topo_order = []

        while queue:
            node = queue.pop(0)
            self.topo_order.append(node)
            for dep in self.edges.get(node, []):
                in_degree[dep] -= 1
                if in_degree[dep] == 0:
                    queue.append(dep)

    def update(self, event) -> dict[str, float]:
        """按拓扑顺序更新所有节点"""
        outputs = {}
        for node_id in self.topo_order:
            node = self.nodes[node_id]
            if node.update(event):
                outputs[node_id] = node.value()
        return outputs

    def get_value(self, node_id: str) -> float:
        """获取指定节点的当前值"""
        return self.nodes[node_id].value()
```

### 计算图配置 (TOML)

```toml
# factor_config.toml - 因子计算图配置

[graph]
name = "obi_mm_factors"
symbol = "BTCUSDT"

# 数据源节点
[[nodes]]
id = "depth_source"
type = "DepthSource"

[[nodes]]
id = "trade_source"
type = "TradeSource"

# 变换节点 - 订单簿不平衡
[[nodes]]
id = "bid_depth_sum"
type = "DepthSum"
side = "bid"
depth_pct = 0.001  # 0.1% 深度
dependencies = ["depth_source"]

[[nodes]]
id = "ask_depth_sum"
type = "DepthSum"
side = "ask"
depth_pct = 0.001
dependencies = ["depth_source"]

[[nodes]]
id = "obi_raw"
type = "Difference"
dependencies = ["bid_depth_sum", "ask_depth_sum"]

[[nodes]]
id = "obi_zscore"
type = "RollingZScore"
window = 3600  # 1小时 (按更新次数)
dependencies = ["obi_raw"]

# 变换节点 - 成交流不平衡
[[nodes]]
id = "buy_volume"
type = "TradeVolumeFilter"
side = "buy"
window_ns = 60_000_000_000  # 60秒
dependencies = ["trade_source"]

[[nodes]]
id = "sell_volume"
type = "TradeVolumeFilter"
side = "sell"
window_ns = 60_000_000_000
dependencies = ["trade_source"]

[[nodes]]
id = "trade_imbalance"
type = "Ratio"
dependencies = ["buy_volume", "sell_volume"]

# 变换节点 - 波动率
[[nodes]]
id = "mid_price"
type = "MidPrice"
dependencies = ["depth_source"]

[[nodes]]
id = "returns"
type = "Returns"
interval_ns = 60_000_000_000  # 60秒收益率
dependencies = ["mid_price"]

[[nodes]]
id = "volatility"
type = "RollingStd"
window = 60  # 60个采样点
dependencies = ["returns"]

# 因子组合
[[nodes]]
id = "alpha_combined"
type = "LinearCombine"
weights = { "obi_zscore" = 0.4, "trade_imbalance" = 0.3, "volatility" = -0.3 }
dependencies = ["obi_zscore", "trade_imbalance", "volatility"]
```

---

## 流式计算节点

### O(1) 滚动统计实现

```python
from numba import njit, float64, int64
from numba.experimental import jitclass
import numpy as np

# ========== Ring Buffer ==========
ring_buffer_spec = [
    ('buffer', float64[:]),
    ('size', int64),
    ('head', int64),
    ('count', int64),
]

@jitclass(ring_buffer_spec)
class RingBuffer:
    """固定大小环形缓冲区"""

    def __init__(self, size: int):
        self.buffer = np.zeros(size, dtype=np.float64)
        self.size = size
        self.head = 0
        self.count = 0

    def push(self, value: float) -> float:
        """添加元素，返回被覆盖的旧值（如果有）"""
        old_value = 0.0
        if self.count == self.size:
            old_value = self.buffer[self.head]
        else:
            self.count += 1

        self.buffer[self.head] = value
        self.head = (self.head + 1) % self.size
        return old_value

    def is_full(self) -> bool:
        return self.count == self.size


# ========== Welford's Algorithm for O(1) Mean/Std ==========
welford_spec = [
    ('n', int64),
    ('mean', float64),
    ('m2', float64),
    ('window', int64),
    ('ring', RingBuffer.class_type.instance_type),
]

@jitclass(welford_spec)
class WelfordRolling:
    """Welford 算法 - O(1) 滚动均值/标准差"""

    def __init__(self, window: int):
        self.window = window
        self.ring = RingBuffer(window)
        self.n = 0
        self.mean = 0.0
        self.m2 = 0.0

    def update(self, value: float):
        """O(1) 更新"""
        if self.ring.is_full():
            # 移除最旧的值
            old_value = self.ring.push(value)
            old_mean = self.mean
            self.mean += (value - old_value) / self.window
            self.m2 += (value - old_value) * (
                (value - self.mean) + (old_value - old_mean)
            )
        else:
            self.ring.push(value)
            self.n += 1
            delta = value - self.mean
            self.mean += delta / self.n
            delta2 = value - self.mean
            self.m2 += delta * delta2

    def get_mean(self) -> float:
        return self.mean

    def get_std(self) -> float:
        n = min(self.n, self.window)
        if n < 2:
            return 0.0
        return np.sqrt(self.m2 / (n - 1))

    def get_zscore(self, value: float) -> float:
        std = self.get_std()
        if std < 1e-10:
            return 0.0
        return (value - self.mean) / std


# ========== Exponential Moving Average ==========
ema_spec = [
    ('alpha', float64),
    ('value', float64),
    ('initialized', int64),
]

@jitclass(ema_spec)
class EMA:
    """指数移动平均 - O(1)"""

    def __init__(self, span: int):
        self.alpha = 2.0 / (span + 1)
        self.value = 0.0
        self.initialized = 0

    def update(self, new_value: float) -> float:
        if self.initialized == 0:
            self.value = new_value
            self.initialized = 1
        else:
            self.value = self.alpha * new_value + (1 - self.alpha) * self.value
        return self.value

    def get_value(self) -> float:
        return self.value


# ========== Time-Windowed Sum with O(1) Amortized ==========
time_window_spec = [
    ('window_ns', int64),
    ('timestamps', int64[:]),
    ('values', float64[:]),
    ('head', int64),
    ('tail', int64),
    ('size', int64),
    ('sum', float64),
]

@jitclass(time_window_spec)
class TimeWindowSum:
    """时间窗口求和 - 摊销 O(1)"""

    def __init__(self, window_ns: int, max_events: int = 100000):
        self.window_ns = window_ns
        self.timestamps = np.zeros(max_events, dtype=np.int64)
        self.values = np.zeros(max_events, dtype=np.float64)
        self.head = 0
        self.tail = 0
        self.size = max_events
        self.sum = 0.0

    def update(self, timestamp: int64, value: float64) -> float64:
        """添加新值并返回当前窗口和"""
        # 添加新值
        self.timestamps[self.tail] = timestamp
        self.values[self.tail] = value
        self.sum += value
        self.tail = (self.tail + 1) % self.size

        # 移除过期值
        cutoff = timestamp - self.window_ns
        while self.head != self.tail and self.timestamps[self.head] < cutoff:
            self.sum -= self.values[self.head]
            self.head = (self.head + 1) % self.size

        return self.sum

    def get_sum(self) -> float64:
        return self.sum
```

### 内置因子节点

```python
# ========== 订单簿不平衡因子 (OBI) ==========
@njit
def compute_obi(
    bid_depth: np.ndarray,
    ask_depth: np.ndarray,
    best_bid_tick: int,
    best_ask_tick: int,
    roi_lb_tick: int,
    mid_price: float,
    looking_depth: float,
    tick_size: float,
) -> float:
    """O(depth_range) 订单簿不平衡计算"""

    # 计算深度范围
    upto_ask_tick = min(
        int(np.floor(mid_price * (1 + looking_depth) / tick_size)),
        len(ask_depth) + roi_lb_tick - 1
    )
    upto_bid_tick = max(
        int(np.ceil(mid_price * (1 - looking_depth) / tick_size)),
        roi_lb_tick
    )

    # 累加卖单深度
    sum_ask_qty = 0.0
    for price_tick in range(best_ask_tick, upto_ask_tick):
        idx = price_tick - roi_lb_tick
        if 0 <= idx < len(ask_depth):
            sum_ask_qty += ask_depth[idx]

    # 累加买单深度
    sum_bid_qty = 0.0
    for price_tick in range(best_bid_tick, upto_bid_tick, -1):
        idx = price_tick - roi_lb_tick
        if 0 <= idx < len(bid_depth):
            sum_bid_qty += bid_depth[idx]

    return sum_bid_qty - sum_ask_qty


# ========== OBI 因子节点 ==========
obi_factor_spec = [
    ('looking_depth', float64),
    ('tick_size', float64),
    ('roi_lb_tick', int64),
    ('welford', WelfordRolling.class_type.instance_type),
    ('current_raw', float64),
    ('current_zscore', float64),
]

@jitclass(obi_factor_spec)
class OBIFactor:
    """订单簿不平衡因子 - 含 Z-Score 标准化"""

    def __init__(self, looking_depth: float, tick_size: float, roi_lb_tick: int, window: int):
        self.looking_depth = looking_depth
        self.tick_size = tick_size
        self.roi_lb_tick = roi_lb_tick
        self.welford = WelfordRolling(window)
        self.current_raw = 0.0
        self.current_zscore = 0.0

    def update(
        self,
        bid_depth: np.ndarray,
        ask_depth: np.ndarray,
        best_bid_tick: int,
        best_ask_tick: int,
        mid_price: float,
    ) -> float:
        """更新并返回标准化后的 OBI"""
        self.current_raw = compute_obi(
            bid_depth, ask_depth,
            best_bid_tick, best_ask_tick,
            self.roi_lb_tick,
            mid_price,
            self.looking_depth,
            self.tick_size
        )

        self.welford.update(self.current_raw)
        self.current_zscore = self.welford.get_zscore(self.current_raw)
        return self.current_zscore

    def get_raw(self) -> float:
        return self.current_raw

    def get_zscore(self) -> float:
        return self.current_zscore


# ========== 成交流不平衡因子 ==========
trade_imbalance_spec = [
    ('window_ns', int64),
    ('buy_sum', TimeWindowSum.class_type.instance_type),
    ('sell_sum', TimeWindowSum.class_type.instance_type),
    ('current_value', float64),
]

@jitclass(trade_imbalance_spec)
class TradeImbalanceFactor:
    """成交流不平衡因子"""

    def __init__(self, window_ns: int, max_events: int = 100000):
        self.window_ns = window_ns
        self.buy_sum = TimeWindowSum(window_ns, max_events)
        self.sell_sum = TimeWindowSum(window_ns, max_events)
        self.current_value = 0.0

    def update(self, timestamp: int, qty: float, is_buy: bool) -> float:
        """处理成交事件"""
        if is_buy:
            self.buy_sum.update(timestamp, qty)
        else:
            self.sell_sum.update(timestamp, qty)

        buy_total = self.buy_sum.get_sum()
        sell_total = self.sell_sum.get_sum()
        total = buy_total + sell_total

        if total > 0:
            self.current_value = (buy_total - sell_total) / total
        else:
            self.current_value = 0.0

        return self.current_value

    def get_value(self) -> float:
        return self.current_value


# ========== 波动率因子 ==========
volatility_spec = [
    ('interval_ns', int64),
    ('last_price', float64),
    ('last_timestamp', int64),
    ('welford', WelfordRolling.class_type.instance_type),
    ('current_value', float64),
]

@jitclass(volatility_spec)
class VolatilityFactor:
    """滚动波动率因子"""

    def __init__(self, interval_ns: int, window: int):
        self.interval_ns = interval_ns
        self.last_price = 0.0
        self.last_timestamp = 0
        self.welford = WelfordRolling(window)
        self.current_value = 0.0

    def update(self, timestamp: int, mid_price: float) -> float:
        """更新波动率估计"""
        if self.last_timestamp == 0:
            self.last_price = mid_price
            self.last_timestamp = timestamp
            return 0.0

        # 检查是否到达计算间隔
        if timestamp - self.last_timestamp >= self.interval_ns:
            if self.last_price > 0:
                ret = mid_price / self.last_price - 1.0
                self.welford.update(ret)
                self.current_value = self.welford.get_std()

            self.last_price = mid_price
            self.last_timestamp = timestamp

        return self.current_value

    def get_value(self) -> float:
        return self.current_value
```

---

## 多因子加权组合

### 线性组合器

```python
linear_combine_spec = [
    ('n_factors', int64),
    ('weights', float64[:]),
    ('values', float64[:]),
    ('combined_value', float64),
]

@jitclass(linear_combine_spec)
class LinearCombiner:
    """多因子线性加权组合"""

    def __init__(self, n_factors: int, weights: np.ndarray):
        self.n_factors = n_factors
        self.weights = weights.copy()
        self.values = np.zeros(n_factors, dtype=np.float64)
        self.combined_value = 0.0

    def set_value(self, factor_idx: int, value: float):
        """设置单个因子值"""
        self.values[factor_idx] = value

    def compute(self) -> float:
        """计算加权组合"""
        self.combined_value = 0.0
        for i in range(self.n_factors):
            self.combined_value += self.weights[i] * self.values[i]
        return self.combined_value

    def update_weight(self, factor_idx: int, new_weight: float):
        """动态更新权重"""
        self.weights[factor_idx] = new_weight

    def get_combined(self) -> float:
        return self.combined_value


# ========== 完整因子引擎 ==========
@njit
def factor_engine_update(
    # 市场数据
    timestamp: int64,
    bid_depth: np.ndarray,
    ask_depth: np.ndarray,
    best_bid_tick: int64,
    best_ask_tick: int64,
    mid_price: float64,
    # 成交数据 (可选)
    has_trade: bool,
    trade_qty: float64,
    trade_is_buy: bool,
    # 因子组件
    obi_factor: OBIFactor,
    trade_factor: TradeImbalanceFactor,
    vol_factor: VolatilityFactor,
    combiner: LinearCombiner,
) -> float64:
    """统一因子更新入口"""

    # 更新 OBI 因子
    obi_zscore = obi_factor.update(
        bid_depth, ask_depth,
        best_bid_tick, best_ask_tick,
        mid_price
    )
    combiner.set_value(0, obi_zscore)

    # 更新成交不平衡因子
    if has_trade:
        trade_imb = trade_factor.update(timestamp, trade_qty, trade_is_buy)
        combiner.set_value(1, trade_imb)

    # 更新波动率因子
    vol = vol_factor.update(timestamp, mid_price)
    combiner.set_value(2, vol)

    # 计算组合因子
    return combiner.compute()
```

### 权重配置与动态调整

```python
class FactorWeightConfig:
    """因子权重配置"""

    def __init__(self, config_path: str = None):
        self.weights: dict[str, float] = {}
        self.constraints: dict[str, tuple[float, float]] = {}  # min, max

        if config_path:
            self.load(config_path)

    def load(self, config_path: str):
        """从 TOML 加载权重配置"""
        import toml
        cfg = toml.load(config_path)

        for factor_cfg in cfg.get('factors', []):
            name = factor_cfg['name']
            self.weights[name] = factor_cfg.get('weight', 1.0)
            if 'min_weight' in factor_cfg and 'max_weight' in factor_cfg:
                self.constraints[name] = (
                    factor_cfg['min_weight'],
                    factor_cfg['max_weight']
                )

    def to_array(self, factor_order: list[str]) -> np.ndarray:
        """按顺序转换为 numpy 数组"""
        return np.array([self.weights.get(f, 0.0) for f in factor_order])

    def normalize(self):
        """归一化权重（可选）"""
        total = sum(abs(w) for w in self.weights.values())
        if total > 0:
            for k in self.weights:
                self.weights[k] /= total
```

---

## 研究环境

### 因子分析器

```python
import numpy as np
import pandas as pd
from scipy import stats
from typing import Optional

class FactorAnalyzer:
    """因子分析器 - 用于研究环境"""

    def __init__(self, factor_data: pd.DataFrame, return_data: pd.Series):
        """
        Args:
            factor_data: 因子值 DataFrame, index=timestamp, columns=factor_names
            return_data: 未来收益 Series, index=timestamp
        """
        self.factor_data = factor_data
        self.return_data = return_data

        # 对齐时间戳
        common_idx = factor_data.index.intersection(return_data.index)
        self.factor_data = factor_data.loc[common_idx]
        self.return_data = return_data.loc[common_idx]

    def compute_ic(self, factor_name: str, method: str = 'spearman') -> pd.Series:
        """计算 Information Coefficient (IC) 序列"""
        factor = self.factor_data[factor_name]

        # 按时间窗口计算滚动 IC
        ic_series = factor.rolling(window=1000).apply(
            lambda x: stats.spearmanr(x, self.return_data.loc[x.index])[0]
            if method == 'spearman'
            else stats.pearsonr(x, self.return_data.loc[x.index])[0]
        )
        return ic_series

    def compute_icir(self, factor_name: str, window: int = 1000) -> float:
        """计算 IC Information Ratio"""
        ic_series = self.compute_ic(factor_name)
        ic_mean = ic_series.mean()
        ic_std = ic_series.std()
        return ic_mean / ic_std if ic_std > 0 else 0.0

    def compute_quantile_returns(
        self,
        factor_name: str,
        n_quantiles: int = 5
    ) -> pd.DataFrame:
        """计算分位数收益"""
        factor = self.factor_data[factor_name]
        returns = self.return_data

        quantile_labels = pd.qcut(factor, q=n_quantiles, labels=False, duplicates='drop')

        results = []
        for q in range(n_quantiles):
            mask = quantile_labels == q
            q_returns = returns[mask]
            results.append({
                'quantile': q + 1,
                'mean_return': q_returns.mean(),
                'std_return': q_returns.std(),
                'count': mask.sum()
            })

        return pd.DataFrame(results)

    def compute_turnover(self, factor_name: str) -> float:
        """计算因子换手率"""
        factor = self.factor_data[factor_name]
        diff = factor.diff().abs()
        return diff.mean() / factor.std() if factor.std() > 0 else 0.0

    def summary(self) -> pd.DataFrame:
        """生成因子分析摘要"""
        results = []
        for factor_name in self.factor_data.columns:
            ic_series = self.compute_ic(factor_name)
            results.append({
                'factor': factor_name,
                'ic_mean': ic_series.mean(),
                'ic_std': ic_series.std(),
                'icir': ic_series.mean() / ic_series.std() if ic_series.std() > 0 else 0,
                'turnover': self.compute_turnover(factor_name),
                'coverage': (~self.factor_data[factor_name].isna()).mean()
            })
        return pd.DataFrame(results)
```

### 参数优化器

```python
from itertools import product
from typing import Callable, Any
import numpy as np

class FactorOptimizer:
    """因子参数优化器"""

    def __init__(
        self,
        factor_builder: Callable[..., Any],
        event_source: Any,
        return_calculator: Callable,
    ):
        """
        Args:
            factor_builder: 因子构建函数, 接受参数返回因子对象
            event_source: 事件数据源
            return_calculator: 收益计算函数
        """
        self.factor_builder = factor_builder
        self.event_source = event_source
        self.return_calculator = return_calculator
        self.results: list[dict] = []

    def grid_search(
        self,
        param_grid: dict[str, list],
        metric: str = 'icir',
        n_jobs: int = 1,
    ) -> pd.DataFrame:
        """网格搜索参数优化"""

        param_names = list(param_grid.keys())
        param_values = list(param_grid.values())

        for params in product(*param_values):
            param_dict = dict(zip(param_names, params))

            # 构建因子
            factor = self.factor_builder(**param_dict)

            # 运行因子计算
            factor_values = self._run_factor(factor)

            # 计算收益
            returns = self.return_calculator(factor_values)

            # 分析
            analyzer = FactorAnalyzer(
                pd.DataFrame({'factor': factor_values}),
                returns
            )
            summary = analyzer.summary().iloc[0]

            result = {
                **param_dict,
                'ic_mean': summary['ic_mean'],
                'ic_std': summary['ic_std'],
                'icir': summary['icir'],
                'turnover': summary['turnover'],
            }
            self.results.append(result)

        return pd.DataFrame(self.results).sort_values(metric, ascending=False)

    def random_search(
        self,
        param_distributions: dict[str, Callable],
        n_iter: int = 100,
        metric: str = 'icir',
    ) -> pd.DataFrame:
        """随机搜索参数优化"""

        for _ in range(n_iter):
            param_dict = {
                name: dist() for name, dist in param_distributions.items()
            }

            factor = self.factor_builder(**param_dict)
            factor_values = self._run_factor(factor)
            returns = self.return_calculator(factor_values)

            analyzer = FactorAnalyzer(
                pd.DataFrame({'factor': factor_values}),
                returns
            )
            summary = analyzer.summary().iloc[0]

            result = {
                **param_dict,
                'ic_mean': summary['ic_mean'],
                'icir': summary['icir'],
            }
            self.results.append(result)

        return pd.DataFrame(self.results).sort_values(metric, ascending=False)

    def _run_factor(self, factor) -> pd.Series:
        """运行因子计算"""
        values = []
        timestamps = []

        for event in self.event_source:
            result = factor.update(event)
            if result is not None:
                values.append(result)
                timestamps.append(event.timestamp)

        return pd.Series(values, index=timestamps)


class WeightOptimizer:
    """多因子权重优化器"""

    def __init__(
        self,
        factor_data: pd.DataFrame,
        return_data: pd.Series,
    ):
        self.factor_data = factor_data
        self.return_data = return_data

    def optimize_ic(self, method: str = 'ridge') -> np.ndarray:
        """优化 IC 最大化的权重"""
        from sklearn.linear_model import Ridge, Lasso, LinearRegression

        X = self.factor_data.values
        y = self.return_data.values

        if method == 'ridge':
            model = Ridge(alpha=1.0)
        elif method == 'lasso':
            model = Lasso(alpha=0.01)
        else:
            model = LinearRegression()

        model.fit(X, y)
        weights = model.coef_

        # 归一化
        weights = weights / np.sum(np.abs(weights))
        return weights

    def optimize_sharpe(
        self,
        constraints: Optional[dict] = None,
        risk_free_rate: float = 0.0,
    ) -> np.ndarray:
        """优化 Sharpe Ratio 的权重"""
        from scipy.optimize import minimize

        n_factors = len(self.factor_data.columns)

        # 计算因子收益协方差矩阵
        factor_returns = self.factor_data.pct_change().dropna()
        cov_matrix = factor_returns.cov().values
        mean_returns = factor_returns.mean().values

        def neg_sharpe(weights):
            port_return = np.dot(weights, mean_returns)
            port_vol = np.sqrt(np.dot(weights.T, np.dot(cov_matrix, weights)))
            return -(port_return - risk_free_rate) / port_vol if port_vol > 0 else 0

        # 约束条件
        cons = [{'type': 'eq', 'fun': lambda w: np.sum(w) - 1}]
        bounds = [(0, 1) for _ in range(n_factors)]

        result = minimize(
            neg_sharpe,
            x0=np.ones(n_factors) / n_factors,
            method='SLSQP',
            bounds=bounds,
            constraints=cons
        )

        return result.x

    def cross_validate(
        self,
        weights: np.ndarray,
        n_splits: int = 5,
    ) -> dict:
        """交叉验证权重稳定性"""
        from sklearn.model_selection import TimeSeriesSplit

        tscv = TimeSeriesSplit(n_splits=n_splits)
        X = self.factor_data.values
        y = self.return_data.values

        train_ics = []
        test_ics = []

        for train_idx, test_idx in tscv.split(X):
            # 训练集 IC
            train_pred = X[train_idx] @ weights
            train_ic = stats.spearmanr(train_pred, y[train_idx])[0]
            train_ics.append(train_ic)

            # 测试集 IC
            test_pred = X[test_idx] @ weights
            test_ic = stats.spearmanr(test_pred, y[test_idx])[0]
            test_ics.append(test_ic)

        return {
            'train_ic_mean': np.mean(train_ics),
            'train_ic_std': np.std(train_ics),
            'test_ic_mean': np.mean(test_ics),
            'test_ic_std': np.std(test_ics),
            'overfit_ratio': np.mean(train_ics) / np.mean(test_ics) if np.mean(test_ics) != 0 else np.inf
        }
```

---

## 因子分析与优化

### 研究工作流

```python
# ========== 研究工作流示例 ==========

# 1. 加载历史数据
from hftbacktest import load_data
from factor_framework import FactorGraph, OBIFactor, TradeImbalanceFactor

# 加载 Collector 采集的历史数据
data = load_data('/data/btcusdt_20240101.npz')

# 2. 构建计算图
graph = FactorGraph()

# 添加 OBI 因子
obi = OBIFactor(
    looking_depth=0.001,
    tick_size=0.1,
    roi_lb_tick=0,
    window=3600
)
graph.add_node('obi', obi)

# 添加成交不平衡因子
trade_imb = TradeImbalanceFactor(
    window_ns=60_000_000_000,
    max_events=100000
)
graph.add_node('trade_imbalance', trade_imb)

# 3. 运行因子计算
factor_values = {'obi': [], 'trade_imbalance': []}
timestamps = []

for event in data:
    outputs = graph.update(event)
    if outputs:
        timestamps.append(event.exch_ts)
        for name, value in outputs.items():
            factor_values[name].append(value)

# 4. 构建分析 DataFrame
import pandas as pd
factor_df = pd.DataFrame(factor_values, index=timestamps)

# 5. 计算未来收益 (用于分析)
def compute_forward_returns(prices: pd.Series, periods: int = 10) -> pd.Series:
    return prices.shift(-periods) / prices - 1

returns = compute_forward_returns(factor_df['mid_price'], periods=100)

# 6. 因子分析
from factor_framework import FactorAnalyzer

analyzer = FactorAnalyzer(factor_df, returns)
summary = analyzer.summary()
print(summary)

# 7. 参数优化
from factor_framework import FactorOptimizer

def build_obi_factor(looking_depth: float, window: int):
    return OBIFactor(looking_depth, tick_size=0.1, roi_lb_tick=0, window=window)

optimizer = FactorOptimizer(
    factor_builder=build_obi_factor,
    event_source=data,
    return_calculator=lambda x: compute_forward_returns(x, 100)
)

param_grid = {
    'looking_depth': [0.0005, 0.001, 0.002, 0.005],
    'window': [1800, 3600, 7200],
}

results = optimizer.grid_search(param_grid, metric='icir')
print(results.head(10))

# 8. 权重优化
from factor_framework import WeightOptimizer

weight_opt = WeightOptimizer(factor_df, returns)
optimal_weights = weight_opt.optimize_ic(method='ridge')
print(f"Optimal weights: {optimal_weights}")

# 9. 交叉验证
cv_results = weight_opt.cross_validate(optimal_weights, n_splits=5)
print(f"CV Results: {cv_results}")
```

### 可视化支持

```python
import matplotlib.pyplot as plt
import seaborn as sns

class FactorVisualizer:
    """因子可视化工具"""

    @staticmethod
    def plot_ic_series(ic_series: pd.Series, title: str = 'IC Time Series'):
        """绘制 IC 时间序列"""
        fig, axes = plt.subplots(2, 1, figsize=(12, 8))

        # IC 时间序列
        axes[0].plot(ic_series, alpha=0.7)
        axes[0].axhline(y=0, color='r', linestyle='--')
        axes[0].axhline(y=ic_series.mean(), color='g', linestyle='-', label=f'Mean: {ic_series.mean():.4f}')
        axes[0].set_title(title)
        axes[0].legend()

        # IC 分布
        axes[1].hist(ic_series.dropna(), bins=50, edgecolor='black')
        axes[1].axvline(x=0, color='r', linestyle='--')
        axes[1].set_title('IC Distribution')

        plt.tight_layout()
        return fig

    @staticmethod
    def plot_quantile_returns(quantile_returns: pd.DataFrame):
        """绘制分位数收益"""
        fig, ax = plt.subplots(figsize=(10, 6))

        ax.bar(quantile_returns['quantile'], quantile_returns['mean_return'])
        ax.set_xlabel('Quantile')
        ax.set_ylabel('Mean Return')
        ax.set_title('Factor Quantile Returns')

        # 添加误差棒
        ax.errorbar(
            quantile_returns['quantile'],
            quantile_returns['mean_return'],
            yerr=quantile_returns['std_return'],
            fmt='none',
            color='black',
            capsize=3
        )

        return fig

    @staticmethod
    def plot_factor_correlation(factor_df: pd.DataFrame):
        """绘制因子相关性热力图"""
        fig, ax = plt.subplots(figsize=(10, 8))

        corr = factor_df.corr()
        sns.heatmap(corr, annot=True, cmap='coolwarm', center=0, ax=ax)
        ax.set_title('Factor Correlation Matrix')

        return fig

    @staticmethod
    def plot_cumulative_returns(returns: pd.Series, factor: pd.Series, n_quantiles: int = 5):
        """绘制分组累计收益"""
        fig, ax = plt.subplots(figsize=(12, 6))

        quantile_labels = pd.qcut(factor, q=n_quantiles, labels=False, duplicates='drop')

        for q in range(n_quantiles):
            mask = quantile_labels == q
            cum_ret = (1 + returns[mask]).cumprod()
            ax.plot(cum_ret, label=f'Q{q+1}')

        ax.set_title('Cumulative Returns by Factor Quantile')
        ax.legend()
        ax.set_xlabel('Time')
        ax.set_ylabel('Cumulative Return')

        return fig
```

---

## 生产部署

### 配置文件结构

```toml
# production_config.toml

[factor_engine]
name = "btcusdt_factors"
symbol = "BTCUSDT"

# IPC 配置
[factor_engine.ipc]
connector_name = "binance_futures"
factor_service_name = "Factor"

# 快照配置
[factor_engine.snapshot]
enabled = true
interval_seconds = 60
max_snapshots = 10
path = "/data/factor_snapshots"
compression = true

# 预热配置
[factor_engine.warmup]
duration_seconds = 300
source = "replay"  # "cold" | "replay" | "snapshot"
replay_path = "/data/historical"

# 因子配置
[[factor_engine.factors]]
name = "obi"
type = "OBIFactor"
enabled = true
[factor_engine.factors.params]
looking_depth = 0.001
window = 3600

[[factor_engine.factors]]
name = "trade_imbalance"
type = "TradeImbalanceFactor"
enabled = true
[factor_engine.factors.params]
window_ns = 60_000_000_000

[[factor_engine.factors]]
name = "volatility"
type = "VolatilityFactor"
enabled = true
[factor_engine.factors.params]
interval_ns = 60_000_000_000
window = 60

# 组合权重
[factor_engine.combination]
type = "linear"
normalize = true
[factor_engine.combination.weights]
obi = 0.4
trade_imbalance = 0.3
volatility = -0.3
```

### 启动流程

```
┌─────────────────────────────────────────────────────────────────┐
│                       生产启动流程                               │
│                                                                  │
│  1. 加载配置文件                                                 │
│                    │                                             │
│                    ▼                                             │
│  2. 初始化因子组件                                               │
│     • 根据配置创建 Factor 对象                                   │
│     • 构建计算图                                                 │
│     • 设置权重                                                   │
│                    │                                             │
│                    ▼                                             │
│  3. 状态恢复（可选）                                             │
│     if snapshot_exists:                                          │
│         load_snapshot() → 热启动                                 │
│     elif replay_enabled:                                         │
│         replay_historical() → 回放启动                           │
│     else:                                                        │
│         cold_start() → 冷启动                                    │
│                    │                                             │
│                    ▼                                             │
│  4. 连接 Connector IPC                                           │
│                    │                                             │
│                    ▼                                             │
│  5. 进入主循环                                                   │
│     while running:                                               │
│         event = receive_event()                                  │
│         factors = graph.update(event)                            │
│         publish_factors(factors)                                 │
│         maybe_snapshot()                                         │
│                    │                                             │
│                    ▼                                             │
│  6. 优雅关闭                                                     │
│     • 保存最终快照                                               │
│     • 关闭 IPC 连接                                              │
└─────────────────────────────────────────────────────────────────┘
```

---

## 实现示例

### 完整因子引擎示例

```python
# factor_engine_example.py

from numba import njit
from numba.typed import Dict
import numpy as np

from hftbacktest import (
    BacktestAsset,
    ROIVectorMarketDepthBacktest,
    Recorder
)

from factor_framework import (
    OBIFactor,
    TradeImbalanceFactor,
    VolatilityFactor,
    LinearCombiner,
    factor_engine_update,
)

@njit
def strategy_with_factors(
    hbt,
    stat,
    # 因子组件
    obi_factor,
    trade_factor,
    vol_factor,
    combiner,
    # 策略参数
    half_spread: float,
    skew: float,
    order_qty: float,
    max_position: float,
):
    """带因子的做市策略"""

    asset_no = 0
    tick_size = hbt.depth(0).tick_size

    while hbt.elapse(10_000_000_000) == 0:  # 10秒间隔
        depth = hbt.depth(asset_no)
        position = hbt.position(asset_no)

        best_bid = depth.best_bid
        best_ask = depth.best_ask
        mid_price = (best_bid + best_ask) / 2.0

        # 更新因子
        alpha = factor_engine_update(
            hbt.current_timestamp,
            depth.bid_depth,
            depth.ask_depth,
            depth.best_bid_tick,
            depth.best_ask_tick,
            mid_price,
            False, 0.0, False,  # 简化：不处理成交
            obi_factor,
            trade_factor,
            vol_factor,
            combiner,
        )

        # 使用因子调整报价
        fair_price = mid_price + alpha * mid_price * 0.0001

        normalized_position = position / order_qty
        reservation_price = fair_price - skew * normalized_position * mid_price

        bid_price = min(reservation_price - half_spread * mid_price, best_bid)
        ask_price = max(reservation_price + half_spread * mid_price, best_ask)

        bid_price = np.floor(bid_price / tick_size) * tick_size
        ask_price = np.ceil(ask_price / tick_size) * tick_size

        # 下单逻辑...
        # (省略具体下单代码)

        stat.record(hbt)


# 使用示例
def main():
    # 初始化因子组件
    obi_factor = OBIFactor(
        looking_depth=0.001,
        tick_size=0.1,
        roi_lb_tick=0,
        window=3600
    )

    trade_factor = TradeImbalanceFactor(
        window_ns=60_000_000_000,
        max_events=100000
    )

    vol_factor = VolatilityFactor(
        interval_ns=60_000_000_000,
        window=60
    )

    # 权重: OBI=0.4, TradeImb=0.3, Vol=-0.3
    weights = np.array([0.4, 0.3, -0.3])
    combiner = LinearCombiner(3, weights)

    # 加载数据并运行回测
    asset = BacktestAsset()
        .data(['/data/btcusdt_20240101.npz'])
        .initial_snapshot('/data/btcusdt_20240101_eod.npz')
        .build()

    hbt = ROIVectorMarketDepthBacktest([asset])
    stat = Recorder()

    strategy_with_factors(
        hbt, stat,
        obi_factor, trade_factor, vol_factor, combiner,
        half_spread=0.0002,
        skew=0.01,
        order_qty=0.01,
        max_position=0.1,
    )

    # 分析结果
    stat.summary()


if __name__ == '__main__':
    main()
```

---

## 附录

### 性能基准

| 操作 | 预期延迟 |
|-----|---------|
| OBI 因子更新 | 100-500 ns |
| 成交不平衡更新 | 50-200 ns |
| 波动率更新 | 20-50 ns |
| 线性组合计算 | 10-30 ns |
| Welford 滚动统计 | 20-50 ns |
| 时间窗口求和（摊销） | O(1) |

### 内存占用估算

| 组件 | 估算内存 |
|-----|---------|
| OBI 因子 (window=3600) | ~30 KB |
| 成交不平衡 (max_events=100000) | ~2.4 MB |
| 波动率因子 (window=60) | ~1 KB |
| 完整计算图 (10 因子) | ~5-10 MB |

### 扩展点

1. **新增因子类型**：实现 `Node` 基类即可
2. **自定义组合方式**：除线性外可扩展非线性组合
3. **在线学习**：权重可根据实时表现动态调整
4. **分布式计算**：因子计算可分片到多进程
