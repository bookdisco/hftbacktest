//! OBI MM Strategy Live Trading Runner
//!
//! This binary runs the OBI MM strategy in live trading mode.
//! Requires the connector to be running.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

use obi_mm::config::StrategyConfig;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to strategy configuration file
    #[arg(short, long, required = true)]
    config: PathBuf,

    /// Connector name for IPC communication
    #[arg(short = 'n', long, default_value = "connector")]
    connector_name: String,

    /// Symbol to trade
    #[arg(short, long, required = true)]
    symbol: String,

    /// Dry run mode (no actual orders)
    #[arg(long, default_value = "false")]
    dry_run: bool,
}

fn main() -> Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let args = Args::parse();

    info!("Starting OBI MM Live Strategy");
    info!("Connector: {}", args.connector_name);
    info!("Symbol: {}", args.symbol);
    info!("Dry run: {}", args.dry_run);

    // Load configuration
    info!("Loading config from {:?}", args.config);
    let config = StrategyConfig::from_file(args.config.to_str().unwrap())?;
    info!("Configuration loaded: {:?}", config);

    // Note: Live trading implementation requires the hftbacktest 'live' feature
    // and proper connector setup. This is a placeholder showing the structure.

    #[cfg(feature = "live")]
    {
        use hftbacktest::depth::HashMapMarketDepth;
        use hftbacktest::live::{ipc::iceoryx::IceoryxUnifiedChannel, BotBuilder, LiveBot};
        use hftbacktest::prelude::*;
        use obi_mm::ObiMmStrategy;

        // Build the live bot
        let mut bot: LiveBot<IceoryxUnifiedChannel, HashMapMarketDepth> = BotBuilder::new()
            .add(
                &args.symbol,
                0.1,   // tick_size (should be from config)
                0.001, // lot_size (should be from config)
            )
            .build(&args.connector_name)?;

        let tick_size = bot.depth(0).tick_size();
        info!("Connected! Tick size: {}", tick_size);

        // Create strategy
        let mut strategy = ObiMmStrategy::new(
            config.global,
            config.params,
            config.stop_loss,
            tick_size,
        );

        info!("Starting main loop...");

        // Main trading loop
        loop {
            match bot.wait_next_feed(true, 5_000_000_000) {
                // 5 second timeout
                Ok(result) => match result {
                    0 => {
                        // Timeout - check if we should continue
                        continue;
                    }
                    2 => {
                        // Market feed received
                        if !args.dry_run {
                            strategy.update(&mut bot, 0);
                        } else {
                            // Dry run - just compute but don't submit orders
                            let depth = bot.depth(0);
                            let mid = (depth.best_bid() + depth.best_ask()) / 2.0;
                            let position = bot.position(0);
                            info!("[DRY RUN] Mid: {:.2}, Position: {:.4}", mid, position);
                        }
                    }
                    3 => {
                        // Order response received
                        let orders = bot.orders(0);
                        info!("Order update received, active orders: {}", orders.len());
                    }
                    1 => {
                        warn!("End of data signal received");
                        break;
                    }
                    _ => {}
                },
                Err(e) => {
                    warn!("Error in wait_next_feed: {:?}", e);
                    // Continue on recoverable errors
                    continue;
                }
            }
        }

        info!("Live trading loop ended");
    }

    #[cfg(not(feature = "live"))]
    {
        warn!("Live trading requires the 'live' feature to be enabled in hftbacktest");
        warn!("This binary is a placeholder for live trading functionality");
    }

    Ok(())
}
