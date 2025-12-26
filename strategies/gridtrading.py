"""
Grid Trading Strategy for Backtesting

This strategy places multiple bid and ask orders around the mid price
with configurable spread and grid parameters.

Usage:
    python strategies/gridtrading.py --data test_data/btcusdt.npz
"""

import argparse
from typing import List

import numpy as np
from numba import njit

from hftbacktest import (
    BacktestAsset,
    HashMapMarketDepthBacktest,
    BUY,
    SELL,
    GTX,
    LIMIT,
)


@njit
def gridtrading(
    hbt,
    half_spread: float = 0.0005,
    grid_interval: float = 0.0002,
    grid_num: int = 5,
    order_qty: float = 0.001,
    max_position: float = 0.1,
    skew: float = 0.0001,
):
    """
    Grid trading market making strategy.

    Args:
        hbt: Backtest instance
        half_spread: Half spread as ratio of mid price (e.g., 0.0005 = 0.05%)
        grid_interval: Grid interval as ratio of mid price
        grid_num: Number of orders on each side
        order_qty: Order quantity per level
        max_position: Maximum position (absolute)
        skew: Position skew factor
    """
    asset_no = 0
    tick_size = hbt.depth(asset_no).tick_size
    lot_size = hbt.depth(asset_no).lot_size

    # Round to tick/lot size
    order_qty = np.round(order_qty / lot_size) * lot_size

    iteration = 0

    # Main loop: 100ms interval
    while hbt.elapse(100_000_000) == 0:
        iteration += 1
        hbt.clear_inactive_orders(asset_no)

        depth = hbt.depth(asset_no)
        position = hbt.position(asset_no)

        # Skip if market depth is invalid
        if depth.best_bid <= 0 or depth.best_ask <= 0:
            continue

        mid_price = (depth.best_bid + depth.best_ask) / 2.0

        # Position-based skew adjustment
        normalized_position = position / order_qty if order_qty > 0 else 0
        bid_depth = half_spread + skew * normalized_position
        ask_depth = half_spread - skew * normalized_position

        # Calculate base prices
        bid_price = mid_price * (1.0 - bid_depth)
        ask_price = mid_price * (1.0 + ask_depth)

        # Ensure we don't cross the spread
        bid_price = min(bid_price, depth.best_bid)
        ask_price = max(ask_price, depth.best_ask)

        # Calculate grid interval in price terms
        grid_step = mid_price * grid_interval
        grid_step = max(grid_step, tick_size)

        # Align to grid
        bid_price = np.floor(bid_price / grid_step) * grid_step
        ask_price = np.ceil(ask_price / grid_step) * grid_step

        # Process time
        if hbt.elapse(1_000_000) != 0:
            return False

        # Track orders to update
        orders = hbt.orders(asset_no)
        last_order_id = -1

        # Cancel orders not on the new grid
        order_values = orders.values()
        while order_values.has_next():
            order = order_values.get()
            should_cancel = False

            if order.side == BUY:
                # Check if order is still on grid
                if position >= max_position:
                    should_cancel = True
                else:
                    on_grid = False
                    for i in range(grid_num):
                        level_price = bid_price - i * grid_step
                        level_tick = np.round(level_price / tick_size)
                        if order.price_tick == level_tick:
                            on_grid = True
                            break
                    if not on_grid:
                        should_cancel = True

            elif order.side == SELL:
                if position <= -max_position:
                    should_cancel = True
                else:
                    on_grid = False
                    for i in range(grid_num):
                        level_price = ask_price + i * grid_step
                        level_tick = np.round(level_price / tick_size)
                        if order.price_tick == level_tick:
                            on_grid = True
                            break
                    if not on_grid:
                        should_cancel = True

            if should_cancel and order.cancellable:
                hbt.cancel(asset_no, order.order_id, False)
                last_order_id = order.order_id

        # Submit new bid orders
        if position < max_position:
            for i in range(grid_num):
                level_price = bid_price - i * grid_step
                order_id = int(np.round(level_price / tick_size))

                # Check if order already exists
                if not orders.contains(order_id):
                    hbt.submit_buy_order(
                        asset_no, order_id, level_price, order_qty, GTX, LIMIT, False
                    )
                    last_order_id = order_id

        # Submit new ask orders
        if position > -max_position:
            for i in range(grid_num):
                level_price = ask_price + i * grid_step
                order_id = int(np.round(level_price / tick_size))

                if not orders.contains(order_id):
                    hbt.submit_sell_order(
                        asset_no, order_id, level_price, order_qty, GTX, LIMIT, False
                    )
                    last_order_id = order_id

        # Wait for order response
        if last_order_id >= 0:
            timeout = 5_000_000_000  # 5 seconds
            if not hbt.wait_order_response(asset_no, last_order_id, timeout):
                return False

    return True


def run_backtest(data_files: List[str], snapshot: str = None):
    """Run backtest with given data files."""

    asset_builder = BacktestAsset()

    # Add data files
    asset_builder = asset_builder.data(data_files)

    # Add initial snapshot if provided
    if snapshot:
        asset_builder = asset_builder.initial_snapshot(snapshot)

    # Configure asset
    asset = (
        asset_builder
        .linear_asset(1.0)
        .power_prob_queue_model(2.0)
        .no_partial_fill_exchange()
        .trading_value_fee_model(-0.00005, 0.0007)  # Maker rebate, taker fee
        .tick_size(0.1)
        .lot_size(0.001)
    )

    # Create backtest instance
    hbt = HashMapMarketDepthBacktest([asset])

    # Run strategy
    print("Running gridtrading backtest...")
    success = gridtrading(
        hbt,
        half_spread=0.0005,
        grid_interval=0.0002,
        grid_num=5,
        order_qty=0.001,
        max_position=0.1,
        skew=0.0001,
    )

    if success:
        print("Backtest completed successfully")
    else:
        print("Backtest ended early")

    # Get final statistics
    # Note: Add your own analysis here
    return hbt


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Grid Trading Backtest")
    parser.add_argument(
        "--data",
        nargs="+",
        default=["test_data/btcusdt.npz"],
        help="Data files for backtest",
    )
    parser.add_argument(
        "--snapshot",
        default=None,
        help="Initial snapshot file",
    )
    args = parser.parse_args()

    run_backtest(args.data, args.snapshot)
