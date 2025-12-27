"""
OBI MM Strategy Backtest - Python Version

This script runs the OBI MM strategy to compare with the Rust version.
Uses identical parameters for consistency verification.
"""

import argparse
import sys
from typing import List
from dataclasses import dataclass

import numpy as np
from numba import njit, uint64, float64, int64, boolean
from numba.typed import Dict
from numba.experimental import jitclass

from hftbacktest import (
    BacktestAsset,
    HashMapMarketDepthBacktest,
    ROIVectorMarketDepthBacktest,
    GTX,
    LIMIT,
    MARKET,
    BUY,
    SELL,
    FILLED,
    Recorder,
)
from hftbacktest.order import IOC


# Configuration matching Rust defaults
@dataclass
class GlobalConfig:
    order_qty: float = 0.01
    max_position: float = 0.1
    grid_num: int = 3
    grid_interval: float = 0.1
    roi_lb: float = 0.0
    roi_ub: float = 200000.0


@dataclass
class StrategyParams:
    looking_depth: float = 0.001
    window: int = 3600
    volatility_window: int = 60  # Window size for volatility calculation
    half_spread: float = 0.0002
    skew: float = 0.01
    c1: float = 0.0001
    power: float = 1.0
    live_seconds: float = 30.0
    update_interval_ns: int = 10_000_000_000  # 10 seconds
    volatility_interval_ns: int = 60_000_000_000  # 60 seconds


@dataclass
class StopLossConfig:
    volatility_mean: float = 0.005
    volatility_std: float = 0.002
    change_mean: float = 0.001
    change_std: float = 0.003
    volatility_threshold: float = 3.0
    change_threshold: float = 3.0


@njit
def obi_mm_strategy(
    hbt,
    recorder,
    # Global config
    order_qty: float,
    max_position: float,
    grid_num: int,
    grid_interval: float,
    roi_lb: float,
    roi_ub: float,
    # Strategy params
    looking_depth: float,
    window: int,
    volatility_window: int,
    half_spread: float,
    skew: float,
    c1: float,
    power: float,
    live_seconds: float,
    update_interval_ns: int,
    volatility_interval_ns: int,
    # Stop loss config
    volatility_mean: float,
    volatility_std: float,
    change_mean: float,
    change_std: float,
    volatility_threshold: float,
    change_threshold: float,
):
    """
    OBI Market Making Strategy - matches Rust implementation.
    """
    asset_no = 0
    tick_size = hbt.depth(asset_no).tick_size
    lot_size = hbt.depth(asset_no).lot_size

    # Pre-allocate timeseries (matching Python original)
    imbalance_timeseries = np.full(30_000_000, np.nan, np.float64)
    change_timeseries = np.full(30_000_000, np.nan, np.float64)

    t = 0
    change_t = 0
    roi_lb_tick = int(round(roi_lb / tick_size))
    roi_ub_tick = int(round(roi_ub / tick_size))

    last_check_period = -1
    last_change_period = -1
    last_price = 0.0
    stop_loss_status = 0
    volatility = volatility_mean

    iteration = 0
    last_log_time = int64(0)

    # Main loop using wait_next_feed (event-driven)
    while hbt.elapse(1_000_000_000) == 0:  # 1 second steps
        current_timestamp = hbt.current_timestamp
        iteration += 1

        # Clear filled orders
        hbt.clear_inactive_orders(asset_no)

        # Calculate current period (every 10 seconds)
        current_period = current_timestamp // update_interval_ns

        # Check if we should update
        if current_period <= last_check_period:
            recorder.record(hbt)
            continue

        # Get market data
        depth = hbt.depth(asset_no)
        position = hbt.position(asset_no)

        best_bid = depth.best_bid
        best_ask = depth.best_ask

        if best_bid <= 0.0 or best_ask <= 0.0:
            recorder.record(hbt)
            continue

        mid_price = (best_bid + best_ask) / 2.0
        best_bid_tick = depth.best_bid_tick
        best_ask_tick = depth.best_ask_tick

        # Update volatility (every 60 seconds)
        change_period = current_period // 6
        minute_change = 0.0

        if change_period > last_change_period:
            if last_price > 0.0:
                minute_change = mid_price / last_price - 1.0
                change_timeseries[change_t] = minute_change
            else:
                change_timeseries[change_t] = 0.0

            if change_t > volatility_window:
                volatility = np.nanstd(change_timeseries[max(0, change_t + 1 - volatility_window):change_t + 1])
            else:
                volatility = volatility_mean

            # Check stop loss
            vol_zscore = (volatility - volatility_mean) / volatility_std if volatility_std > 0 else 0
            change_zscore = np.abs(minute_change - change_mean) / change_std if change_std > 0 else 0

            if vol_zscore > volatility_threshold or change_zscore > change_threshold:
                stop_loss_status = 1
            else:
                stop_loss_status = 0

            last_price = mid_price
            last_change_period = change_period
            change_t += 1

        # Calculate OBI
        sum_ask_qty = 0.0
        from_tick = max(best_ask_tick, roi_lb_tick)
        upto_tick = min(int(np.floor(mid_price * (1 + looking_depth) / tick_size)), roi_ub_tick)
        for price_tick in range(from_tick, upto_tick):
            sum_ask_qty += depth.ask_depth[price_tick - roi_lb_tick]

        sum_bid_qty = 0.0
        from_tick = min(best_bid_tick, roi_ub_tick)
        upto_tick = max(int(np.ceil(mid_price * (1 - looking_depth) / tick_size)), roi_lb_tick)
        for price_tick in range(from_tick, upto_tick, -1):
            sum_bid_qty += depth.bid_depth[price_tick - roi_lb_tick]

        imbalance_timeseries[t] = sum_bid_qty - sum_ask_qty

        # Standardize OBI (z-score)
        m = np.nanmean(imbalance_timeseries[max(0, t + 1 - window):t + 1])
        s = np.nanstd(imbalance_timeseries[max(0, t + 1 - window):t + 1])
        alpha = np.divide(imbalance_timeseries[t] - m, s) if s > 1e-10 else 0.0

        t += 1
        last_check_period = current_period

        # Skip trading if factors are not ready (still warming up)
        # OBI needs `window` samples, volatility needs `volatility_window` samples
        if t < window or change_t < volatility_window:
            if current_timestamp - last_log_time >= 3600_000_000_000:
                print(f"[WARMUP] OBI: {t}/{window}, Vol: {change_t}/{volatility_window}")
                last_log_time = current_timestamp
            recorder.record(hbt)
            continue

        # Compute prices
        volatility_ratio = (volatility / volatility_mean) ** power
        adjusted_half_spread = half_spread * volatility_ratio

        fair_price = mid_price + c1 * alpha * mid_price
        normalized_position = position / order_qty
        reservation_price = fair_price - skew * normalized_position * mid_price

        bid_price = min(reservation_price - adjusted_half_spread * mid_price, best_bid)
        ask_price = max(reservation_price + adjusted_half_spread * mid_price, best_ask)

        bid_price = np.floor(bid_price / tick_size) * tick_size
        ask_price = np.ceil(ask_price / tick_size) * tick_size

        # Update orders based on stop loss status
        if stop_loss_status == 0:
            # Normal operation - update grid orders
            new_bid_orders = Dict.empty(uint64, float64)
            if position < max_position and np.isfinite(bid_price):
                for i in range(grid_num):
                    bid_price_tick = uint64(round(bid_price / tick_size))
                    new_bid_orders[bid_price_tick] = bid_price
                    bid_price -= grid_interval

            new_ask_orders = Dict.empty(uint64, float64)
            if position > -max_position and np.isfinite(ask_price):
                for i in range(grid_num):
                    ask_price_tick = uint64(round(ask_price / tick_size))
                    new_ask_orders[ask_price_tick] = ask_price
                    ask_price += grid_interval

            # Cancel and count orders
            holding_buy_order_num = 0
            holding_sell_order_num = 0
            orders = hbt.orders(asset_no)
            order_values = orders.values()

            while order_values.has_next():
                order = order_values.get()
                if order.cancellable:
                    dominated = False
                    if order.side == BUY:
                        dominated = order.order_id not in new_bid_orders
                    elif order.side == SELL:
                        dominated = order.order_id not in new_ask_orders

                    if dominated:
                        order_live_seconds = (current_timestamp - order.exch_timestamp) / 1_000_000_000
                        if order_live_seconds >= live_seconds:
                            hbt.cancel(asset_no, order.order_id, False)
                            if order.side == BUY:
                                holding_buy_order_num -= 1
                            else:
                                holding_sell_order_num -= 1
                        else:
                            if order.side == BUY:
                                holding_buy_order_num += 1
                            else:
                                holding_sell_order_num += 1
                    else:
                        if order.side == BUY:
                            holding_buy_order_num += 1
                        else:
                            holding_sell_order_num += 1

            holding_buy_order_num = max(0, holding_buy_order_num)
            holding_sell_order_num = max(0, holding_sell_order_num)

            # Submit new orders
            orders = hbt.orders(asset_no)
            for order_id, order_price in new_bid_orders.items():
                if order_id not in orders:
                    effective_position = position + 0.3 * holding_buy_order_num * order_qty
                    if effective_position < max_position:
                        hbt.submit_buy_order(asset_no, order_id, order_price, order_qty, GTX, LIMIT, False)

            for order_id, order_price in new_ask_orders.items():
                if order_id not in orders:
                    effective_position = position - 0.3 * holding_sell_order_num * order_qty
                    if effective_position > -max_position:
                        hbt.submit_sell_order(asset_no, order_id, order_price, order_qty, GTX, LIMIT, False)
        else:
            # Stop loss triggered - cancel all and close position
            orders = hbt.orders(asset_no)
            order_values = orders.values()
            while order_values.has_next():
                order = order_values.get()
                if order.cancellable:
                    hbt.cancel(asset_no, order.order_id, False)

            position = hbt.position(asset_no)
            order_qty_stop = np.abs(position)
            if order_qty_stop > 1e-9:
                if position > 0:
                    order_price = (np.round(mid_price / tick_size) - 200) * tick_size
                    hbt.submit_sell_order(asset_no, 519, order_price, order_qty_stop, IOC, MARKET, True)
                elif position < 0:
                    order_price = (np.round(mid_price / tick_size) + 200) * tick_size
                    hbt.submit_buy_order(asset_no, 519, order_price, order_qty_stop, IOC, MARKET, True)

        # Log progress
        if current_timestamp - last_log_time >= 3600_000_000_000:  # Every hour
            print("Time:", current_timestamp // 1_000_000_000, "Position:", position, "Mid:", mid_price)
            last_log_time = current_timestamp

        if t >= len(imbalance_timeseries):
            break

        recorder.record(hbt)

    return True


def run_backtest(data_files: List[str], output_path: str = "."):
    """Run OBI MM backtest with given data files."""

    # Use default configs (matching Rust)
    g = GlobalConfig()
    p = StrategyParams()
    sl = StopLossConfig()

    asset = (
        BacktestAsset()
        .data(data_files)
        .linear_asset(1.0)
        .power_prob_queue_model(3.0)  # Match Rust: PowerProbQueueFunc3::new(3.0)
        .no_partial_fill_exchange()
        .trading_value_fee_model(-0.00005, 0.0007)  # Maker rebate, taker fee
        .constant_order_latency(1_000_000, 1_000_000)  # 1ms
        .tick_size(0.1)
        .lot_size(0.001)
        .roi_lb(g.roi_lb)
        .roi_ub(g.roi_ub)
    )

    # Create backtest instance (use ROIVector for direct depth array access)
    hbt = ROIVectorMarketDepthBacktest([asset])

    # Create recorder (1 asset, 100k records max)
    recorder = Recorder(1, 100_000)

    print("Running OBI MM backtest (Python version)...")
    print(f"Data files: {data_files}")

    success = obi_mm_strategy(
        hbt,
        recorder.recorder,  # Pass the jitclass instance
        # Global config
        g.order_qty,
        g.max_position,
        g.grid_num,
        g.grid_interval,
        g.roi_lb,
        g.roi_ub,
        # Strategy params
        p.looking_depth,
        p.window,
        p.volatility_window,
        p.half_spread,
        p.skew,
        p.c1,
        p.power,
        p.live_seconds,
        p.update_interval_ns,
        p.volatility_interval_ns,
        # Stop loss config
        sl.volatility_mean,
        sl.volatility_std,
        sl.change_mean,
        sl.change_std,
        sl.volatility_threshold,
        sl.change_threshold,
    )

    if success:
        print("Backtest completed successfully")
    else:
        print("Backtest ended early")

    # Get final statistics
    state = hbt.state_values(0)
    position = hbt.position(0)

    print("\n=== Backtest Complete ===")
    print(f"Final Position: {position:.4f}")
    print(f"Balance: {state.balance:.4f}")
    print(f"Fee: {state.fee:.4f}")
    print(f"Trade Count: {state.num_trades}")
    print(f"Trade Volume: {state.trading_volume:.4f}")
    print(f"Trade Amount: {state.trading_value:.4f}")

    # Calculate net PnL (balance + fee for maker rebate)
    net_pnl = state.balance + state.fee
    print(f"Net PnL: {net_pnl:.4f}")

    # Save recorder data
    import os
    os.makedirs(output_path, exist_ok=True)
    recorder.to_npz(f"{output_path}/obi_mm_python")

    return hbt, state


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="OBI MM Backtest (Python)")
    parser.add_argument(
        "--data",
        nargs="+",
        required=True,
        help="Data files for backtest",
    )
    parser.add_argument(
        "--output",
        default=".",
        help="Output directory for results",
    )
    args = parser.parse_args()

    run_backtest(args.data, args.output)
