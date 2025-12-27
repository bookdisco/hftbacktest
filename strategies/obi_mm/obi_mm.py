import numpy as np

from numba import njit, uint64,float64
from numba.typed import Dict

from hftbacktest import (
    BacktestAsset,
    ROIVectorMarketDepthBacktest,
    GTX,
    GTC,
    IOC,
    LIMIT,
    BUY,
    SELL,
    BUY_EVENT,
    SELL_EVENT,
    FILLED,
    MARKET,
    Recorder
)

from .utils import OrderContainer,StopLossFunc,Global,Params

@njit
def obi_mm(
    hbt,
    stat,
    order_container:OrderContainer,
    stop_loss_func:StopLossFunc,
    g:Global,                          #Global对象 环境变量
    p:Params,                          #Params对象 策略参数
):
    # order_list = []
    asset_no = 0
    imbalance_timeseries = np.full(30_000_000, np.nan, np.float64)
    change_timeseries = np.full(30_000_000, np.nan, np.float64)
    tick_size = hbt.depth(0).tick_size
    lot_size = hbt.depth(0).lot_size

    t = 0
    change_t = 0
    roi_lb_tick = int(round(g.roi_lb / tick_size))
    roi_ub_tick = int(round(g.roi_ub / tick_size))
    
    # 记录上一次检查的时间段
    last_check_period = -1  # 从-1开始，确保第一次会执行
    last_change_period = -1  # 从-1开始，确保第一次会执行
    last_price = 0
    # 止损状态
    stop_loss_status = 0
    volatility = stop_loss_func.volatility_mean

    # 使用事件驱动而不是时间驱动
    while hbt.wait_next_feed(False, 1_000_000_000) in [0, 2, 3]:  # 1秒超时
        current_timestamp = hbt.current_timestamp
        orders = hbt.orders(asset_no)
        order_values = orders.values()
        while order_values.has_next():
            order = order_values.get()
            if order.status == FILLED:
                pass
                order_container.save_order(order, current_timestamp)

        hbt.clear_inactive_orders(asset_no)
        
        # 计算当前时间段（每10s为一个时间段）
        current_period = current_timestamp // (10 * 1_000_000_000)

        
        # 检查是否到达新的检查时间段
        if current_period > last_check_period:
            last_trades = hbt.last_trades(asset_no)
            if len(last_trades) > 0:
                last_trade_ts = last_trades[-1][1]
                hbt.clear_last_trades(asset_no)
                if last_trade_ts == current_timestamp:
                    continue
            # if current_period % (360 * 24) == 0:
            #     print(current_period)


            # 执行检查和下单逻辑
            depth = hbt.depth(asset_no)
            position = hbt.position(asset_no)
            orders = hbt.orders(asset_no)

            best_bid = depth.best_bid
            best_ask = depth.best_ask

            mid_price = (best_bid + best_ask) / 2.0
            # 波动率跟踪计算
            change_period = current_period // 6 # 每60s计算一次变化率
            if change_period > last_change_period:

                if last_price == 0:
                    change_timeseries[change_t] =0
                else:
                    change_timeseries[change_t] = mid_price / last_price - 1

                minute_change = change_timeseries[change_t]
                if change_t > p.volatility_window:
                    volatility = np.nanstd(change_timeseries[max(0, int(change_t + 1 - p.volatility_window)):int(change_t + 1)])
                else:
                    volatility = stop_loss_func.volatility_mean

                stop_loss_status = stop_loss_func.check_stop_loss(volatility, minute_change)
                last_price = mid_price
                last_change_period = change_period
                change_t += 1



            sum_ask_qty = 0.0
            from_tick = max(depth.best_ask_tick, roi_lb_tick)
            upto_tick = min(int(np.floor(mid_price * (1 + p.looking_depth) / tick_size)), roi_ub_tick)
            for price_tick in range(from_tick, upto_tick):
                sum_ask_qty += depth.ask_depth[price_tick - roi_lb_tick]

            sum_bid_qty = 0.0
            from_tick = min(depth.best_bid_tick, roi_ub_tick)
            upto_tick = max(int(np.ceil(mid_price * (1 - p.looking_depth) / tick_size)), roi_lb_tick)
            for price_tick in range(from_tick, upto_tick, -1):
                sum_bid_qty += depth.bid_depth[price_tick - roi_lb_tick]

            imbalance_timeseries[t] = sum_bid_qty - sum_ask_qty

            # Standardizes the order book imbalance timeseries for a given window
            # 第114-115行修改为：
            m = np.nanmean(imbalance_timeseries[max(0, int(t + 1 - p.window)):int(t + 1)])
            s = np.nanstd(imbalance_timeseries[max(0, int(t + 1 - p.window)):int(t + 1)])
            alpha = np.divide(imbalance_timeseries[t] - m, s)

            t += 1
            last_check_period = current_period

            # Skip trading if factors are not ready (still warming up)
            # OBI needs `window` samples, volatility needs `volatility_window` samples
            if t < p.window or change_t < p.volatility_window:
                stat.record(hbt)
                continue

            #--------------------------------------------------------
            # Computes bid price and ask price.
            half_spread = p.half_spread 
            volatility_ratio = (volatility/stop_loss_func.volatility_mean)**p.power
            half_spread *= volatility_ratio



            fair_price = mid_price + p.c1 * alpha * mid_price

            normalized_position = position / g.order_qty

            reservation_price = fair_price - p.skew * normalized_position * mid_price

            bid_price = min(reservation_price - half_spread * mid_price, best_bid)

            ask_price = max(reservation_price + half_spread * mid_price, best_ask)

            bid_price = np.floor(bid_price / tick_size) * tick_size
            ask_price = np.ceil(ask_price / tick_size) * tick_size

            #--------------------------------------------------------
            # Updates quotes.



            if stop_loss_status == 0:
                # Creates a new grid for buy orders.
                new_bid_orders = Dict.empty(uint64, float64)
                if position < g.max_position and np.isfinite(bid_price):
                    for i in range(g.grid_num):
                        bid_price_tick = round(bid_price / tick_size)

                        # order price in tick is used as order id.
                        new_bid_orders[uint64(bid_price_tick)] = bid_price

                        bid_price -= g.grid_interval

                # Creates a new grid for sell orders.
                new_ask_orders = Dict.empty(uint64, float64)
                if position > -g.max_position and np.isfinite(ask_price):
                    for i in range(g.grid_num):
                        ask_price_tick = round(ask_price / tick_size)

                        # order price in tick is used as order id.
                        new_ask_orders[uint64(ask_price_tick)] = ask_price

                        ask_price += g.grid_interval

                holding_buy_order_num = 0
                holding_sell_order_num = 0
                order_values = orders.values()
                while order_values.has_next():
                    order = order_values.get()
                    # Cancels if a working order is not in the new grid.
                    if order.cancellable:
                        if (
                            (order.side == BUY and order.order_id not in new_bid_orders)
                            or (order.side == SELL and order.order_id not in new_ask_orders)
                        ):
                            order_live_seconds = (hbt.current_timestamp - order.exch_timestamp) / 1_000_000_000
                            if order_live_seconds >= p.live_seconds:
                                # print(order_live_seconds)
                                hbt.cancel(asset_no, order.order_id, False)
                                order_container.save_order(order, current_timestamp)
                                if order.side == BUY:
                                    holding_buy_order_num -=1
                                else:
                                    holding_sell_order_num -=1
                            else:
                                if order.side == BUY:
                                    holding_buy_order_num +=1
                                else:
                                    holding_sell_order_num +=1
                        else:
                            if order.side == BUY:
                                holding_buy_order_num +=1
                            else:
                                holding_sell_order_num +=1
                holding_buy_order_num = max(0, holding_buy_order_num)
                holding_sell_order_num = max(0, holding_sell_order_num)

                # 执行下单逻辑
                for order_id, order_price in new_bid_orders.items():
                    # Posts a new buy order if there is no working order at the price on the new grid.
                    if order_id not in orders:
                        if (position + 0.3 * holding_buy_order_num * g.order_qty)  < g.max_position:
                            hbt.submit_buy_order(asset_no, order_id, order_price, g.order_qty, GTX, LIMIT, False)
                for order_id, order_price in new_ask_orders.items():
                    # Posts a new sell order if there is no working order at the price on the new grid.
                    if order_id not in orders:
                        if (position - 0.3 * holding_sell_order_num * g.order_qty)   > -g.max_position:
                            hbt.submit_sell_order(asset_no, order_id, order_price, g.order_qty, GTX, LIMIT, False)
            else:
                # 止损触发 清仓 撤所有单
                orders = hbt.orders(asset_no)
                order_values = orders.values()
                while order_values.has_next():
                    order = order_values.get()
                    if order.cancellable:
                        hbt.cancel(asset_no, order.order_id, False)
                        order_container.save_order(order, current_timestamp)

                position = hbt.position(asset_no)
                order_qty_stop = np.abs(position)
                if order_qty_stop > 1e-9:
                    if stop_loss_status == 1:
                        print(change_t,position,current_timestamp,"止损触发", (volatility-stop_loss_func.volatility_mean)/stop_loss_func.volatility_std, np.abs(minute_change-stop_loss_func.change_mean)/stop_loss_func.change_std)
                    if position > 0:
                        order_price = (np.round(mid_price / tick_size) - 200) * tick_size
                        hbt.submit_sell_order(asset_no, 519, order_price, order_qty_stop, IOC, MARKET, True)
                    elif position < 0:
                        order_price = (np.round(mid_price / tick_size) + 200) * tick_size
                        hbt.submit_buy_order(asset_no, 519, order_price, order_qty_stop, IOC, MARKET, True)

            if t >= len(imbalance_timeseries):
                raise Exception

            # Records the current state for stat calculation (only when strategy logic is executed)
            stat.record(hbt)

        # return order_container