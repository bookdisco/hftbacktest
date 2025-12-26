//! OBI MM Strategy Backtest Runner
//!
//! This binary runs the OBI MM strategy in backtesting mode.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use hftbacktest::{
    backtest::{
        Backtest,
        ExchangeKind,
        L2AssetBuilder,
        assettype::LinearAsset,
        data::DataSource,
        models::{
            CommonFees,
            ConstantLatency,
            PowerProbQueueFunc3,
            ProbQueueModel,
            TradingValueFeeModel,
        },
        recorder::BacktestRecorder,
    },
    depth::ROIVectorMarketDepth,
    prelude::{ApplySnapshot, Bot, HashMapMarketDepth, L2MarketDepth, MarketDepth},
    types::{ElapseResult, Event, Recorder},
};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use obi_mm::{config::StrategyConfig, ObiMmStrategy};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to strategy configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Path to market data files (can specify multiple)
    #[arg(short, long, required = true)]
    data: Vec<PathBuf>,

    /// Path to initial snapshot file
    #[arg(short, long)]
    snapshot: Option<PathBuf>,

    /// Output path for results
    #[arg(short, long, default_value = ".")]
    output: PathBuf,

    /// Tick size
    #[arg(long, default_value = "0.1")]
    tick_size: f64,

    /// Lot size
    #[arg(long, default_value = "0.001")]
    lot_size: f64,

    /// Use ROIVectorMarketDepth instead of HashMapMarketDepth
    #[arg(long)]
    roi: bool,

    /// ROI lower bound (price)
    #[arg(long, default_value = "0.0")]
    roi_lb: f64,

    /// ROI upper bound (price)
    #[arg(long, default_value = "200000.0")]
    roi_ub: f64,
}

/// Runs the backtest loop with a generic market depth type.
fn run_backtest<MD>(
    mut hbt: Backtest<MD>,
    config: StrategyConfig,
    output: PathBuf,
) -> Result<()>
where
    MD: MarketDepth + L2MarketDepth,
{
    // Get tick size from depth
    let tick_size = hbt.depth(0).tick_size();
    info!("Tick size: {}", tick_size);
    info!("Market Depth: {}", std::any::type_name::<MD>());

    // Create strategy
    let mut strategy = ObiMmStrategy::new(config.global, config.params, config.stop_loss, tick_size);

    // Create recorder
    let mut recorder = BacktestRecorder::new(&hbt);

    info!("Starting backtest...");

    let mut iteration = 0u64;
    let mut last_log_time = 0i64;

    // Main backtest loop
    loop {
        match hbt.elapse(1_000_000_000) {
            // 1 second steps
            Ok(ElapseResult::Ok) => {
                // Normal processing
                strategy.update(&mut hbt, 0);

                iteration += 1;

                // Record state
                recorder.record::<MD, _>(&hbt).unwrap();

                // Log progress periodically
                let current_time = hbt.current_timestamp();
                if current_time - last_log_time >= 3600_000_000_000 {
                    // Every hour
                    let position = hbt.position(0);
                    let depth = hbt.depth(0);
                    let mid = (depth.best_bid() + depth.best_ask()) / 2.0;

                    info!(
                        "Time: {}, Position: {:.4}, Mid: {:.2}, Iteration: {}",
                        current_time / 1_000_000_000,
                        position,
                        mid,
                        iteration
                    );
                    last_log_time = current_time;
                }
            }
            Ok(ElapseResult::EndOfData) => {
                info!("End of data reached");
                break;
            }
            Ok(_) => {
                // Other results (MarketFeed, OrderResponse)
                continue;
            }
            Err(e) => {
                info!("Backtest error: {:?}", e);
                break;
            }
        }
    }

    // Close and finalize
    hbt.close().unwrap();

    // Print final statistics
    let final_position = hbt.position(0);
    let state = hbt.state_values(0);

    info!("=== Backtest Complete ===");
    info!("Final Position: {:.4}", final_position);
    info!("Total Iterations: {}", iteration);
    info!("Balance: {:.4}", state.balance);
    info!("Fee: {:.4}", state.fee);

    // Save results
    info!("Saving results to {:?}", output);
    recorder.to_csv("obi_mm", &output).unwrap();

    Ok(())
}

fn main() -> Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let args = Args::parse();

    // Load configuration
    let config = if let Some(config_path) = &args.config {
        info!("Loading config from {:?}", config_path);
        StrategyConfig::from_file(config_path.to_str().unwrap())?
    } else {
        info!("Using default configuration");
        StrategyConfig::default()
    };

    info!("Configuration: {:?}", config);

    // Build data sources
    let data: Vec<DataSource<Event>> = args
        .data
        .iter()
        .map(|p| DataSource::File(p.to_string_lossy().to_string()))
        .collect();

    for path in &args.data {
        info!("Adding data file: {:?}", path);
    }

    let tick_size = args.tick_size;
    let lot_size = args.lot_size;
    let snapshot_path = args.snapshot.clone();
    let roi_lb = args.roi_lb;
    let roi_ub = args.roi_ub;

    if args.roi {
        info!("Using ROIVectorMarketDepth (roi_lb={}, roi_ub={})", roi_lb, roi_ub);

        // Build backtest with ROIVectorMarketDepth
        let hbt: Backtest<ROIVectorMarketDepth> = Backtest::builder()
            .add_asset(
                L2AssetBuilder::new()
                    .data(data)
                    .latency_model(ConstantLatency::new(1_000_000, 1_000_000))
                    .asset_type(LinearAsset::new(1.0))
                    .fee_model(TradingValueFeeModel::new(CommonFees::new(-0.00005, 0.0007)))
                    .exchange(ExchangeKind::NoPartialFillExchange)
                    .queue_model(ProbQueueModel::new(PowerProbQueueFunc3::new(3.0)))
                    .depth(move || {
                        let mut depth = ROIVectorMarketDepth::new(tick_size, lot_size, roi_lb, roi_ub);
                        if let Some(ref snap_path) = snapshot_path {
                            if let Ok(data) = hftbacktest::backtest::data::read_npz_file(
                                snap_path.to_str().unwrap(),
                                "data",
                            ) {
                                depth.apply_snapshot(&data);
                                info!("Applied snapshot from {:?}", snap_path);
                            }
                        }
                        depth
                    })
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();

        run_backtest(hbt, config, args.output)
    } else {
        info!("Using HashMapMarketDepth");

        // Build backtest with HashMapMarketDepth
        let hbt: Backtest<HashMapMarketDepth> = Backtest::builder()
            .add_asset(
                L2AssetBuilder::new()
                    .data(data)
                    .latency_model(ConstantLatency::new(1_000_000, 1_000_000))
                    .asset_type(LinearAsset::new(1.0))
                    .fee_model(TradingValueFeeModel::new(CommonFees::new(-0.00005, 0.0007)))
                    .exchange(ExchangeKind::NoPartialFillExchange)
                    .queue_model(ProbQueueModel::new(PowerProbQueueFunc3::new(3.0)))
                    .depth(move || {
                        let mut depth = HashMapMarketDepth::new(tick_size, lot_size);
                        if let Some(ref snap_path) = snapshot_path {
                            if let Ok(data) = hftbacktest::backtest::data::read_npz_file(
                                snap_path.to_str().unwrap(),
                                "data",
                            ) {
                                depth.apply_snapshot(&data);
                                info!("Applied snapshot from {:?}", snap_path);
                            }
                        }
                        depth
                    })
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();

        run_backtest(hbt, config, args.output)
    }
}
