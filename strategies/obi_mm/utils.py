import numba as nb
import numpy as np
from numba.experimental import jitclass

# ===== 订单容器 =====


order_record_dtype = np.dtype([
    ('order_id', 'u8'),      # 订单ID
    ('price', 'f8'),         # 价格
    ('qty', 'f8'),           # 数量
    ('side', 'i1'),          # 方向 (1=买, -1=卖)
    ('created_timestamp', 'u8'),     # 订单创建时间戳
    ('updated_timestamp', 'u8'),     # 更新时间 (修正拼写)
    ('status', 'i1'),        # 状态 
])

@jitclass([
    ('data', nb.from_dtype(order_record_dtype)[:]),
    ('current_index', nb.int64),
    ('capacity', nb.int64),
]) # type: ignore[misc]

class OrderContainer:
    def __init__(self, capacity):
        self.data = np.empty(capacity, dtype=order_record_dtype)
        self.current_index = 0
        self.capacity = capacity
    
    def save_order(self, order, updated_timestamp):
        """从订单对象添加记录"""
        if self.current_index < self.capacity:
            self.data[self.current_index]['order_id'] = order.order_id
            self.data[self.current_index]['price'] = order.price
            self.data[self.current_index]['qty'] = order.qty
            self.data[self.current_index]['side'] = order.side
            self.data[self.current_index]['created_timestamp'] = order.exch_timestamp
            self.data[self.current_index]['updated_timestamp'] = updated_timestamp
            self.data[self.current_index]['status'] = order.status
            self.current_index += 1
            return True
        return False

def create_order_container(capacity):
    """创建订单容器"""
    return OrderContainer(capacity) 



@jitclass([
    ('status', nb.int64),
    ('threshold', nb.float64),
    ('volatility_mean', nb.float64),
    ('volatility_std', nb.float64),
    ('change_mean', nb.float64),
    ('change_std', nb.float64),
]) # type: ignore[misc]

class StopLossFunc:
    def __init__(self, threshold, volatility_mean, volatility_std, change_mean, change_std):
        self.status = 0
        self.threshold = threshold
        self.volatility_mean = volatility_mean
        self.volatility_std = volatility_std
        self.change_mean = change_mean
        self.change_std = change_std
    def check_stop_loss(self, volatility, change):
        if (volatility - self.volatility_mean) / self.volatility_std >=  self.threshold and np.abs(change - self.change_mean) / self.change_std >= self.threshold * 2:
                self.status = 1
        elif (volatility - self.volatility_mean) / self.volatility_std <=  self.threshold / 2:
                self.status = 0
        return self.status

def create_stop_loss_func(threshold, volatility_mean, volatility_std, change_mean, change_std):
    """创建订单容器"""
    return StopLossFunc(threshold, volatility_mean, volatility_std, change_mean, change_std) 


@jitclass([
    ('order_qty', nb.float64),
    ('max_position', nb.float64),
    ('grid_num', nb.int64),
    ('grid_interval', nb.float64),
    ('roi_lb', nb.float64),
    ('roi_ub', nb.float64),
]) # type: ignore[misc]

class Global:
    def __init__(self, order_qty, max_position, grid_num, grid_interval, roi_lb, roi_ub):
        self.order_qty = order_qty
        self.max_position = max_position
        self.grid_num = grid_num
        self.grid_interval = grid_interval
        self.roi_lb = roi_lb
        self.roi_ub = roi_ub

@jitclass([
    ('half_spread', nb.float64),
    ('skew', nb.float64),
    ('c1', nb.float64),
    ('looking_depth', nb.float64),
    ('window', nb.int64),
    ('volatility_window', nb.int64),
    ('live_seconds', nb.float64),
    ('power', nb.float64),
]) # type: ignore[misc]

class Params:
    def __init__(self, half_spread, skew, c1, looking_depth, window, volatility_window, live_seconds, power):
        self.half_spread = half_spread
        self.skew = skew
        self.c1 = c1
        self.looking_depth = looking_depth
        self.window = window
        self.volatility_window = volatility_window
        self.live_seconds = live_seconds
        self.power = power


def extract_key_metrics(stats_df):
    """提取关键指标"""
    if len(stats_df) == 0:
        return None
        
    row = stats_df.row(0, named=True)
    return {
        '夏普比率': round(row['SR'], 3),
        '索提诺比率': round(row['Sortino'], 3), 
        '总收益率': round(row['Return'], 4),
        '最大回撤': round(row['MaxDrawdown'], 4),
        '收益回撤比': round(row['ReturnOverMDD'], 3),
        '日均交易': round(row['DailyNumberOfTrades'], 2),
        '日均换手': round(row['DailyTurnover'], 2)
    }