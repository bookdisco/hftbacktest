# Strategies

Python strategies for backtesting and research.

## Directory Structure

```
strategies/
├── README.md
├── gridtrading.py          # Grid trading strategy
├── market_making.py        # Market making with alpha
└── utils/                  # Shared utilities
    └── indicators.py
```

## Usage

```python
# Install hftbacktest
pip install hftbacktest

# Or install from local development
cd py-hftbacktest && maturin develop --release

# Run backtest
python strategies/gridtrading.py
```

## Notes

- Python strategies are for **backtesting and research only**
- Live trading uses Rust implementation in `examples/testnet/` or `connector/`
- Data files should be placed in `test_data/` or specified via config
