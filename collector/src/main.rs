use anyhow::anyhow;
use clap::Parser;
use tokio::{self, select, signal, sync::mpsc::unbounded_channel};
use tracing::{error, info};

use crate::file::Writer;

mod binance;
mod binancefuturescm;
mod binancefuturesum;
mod bybit;
mod error;
mod file;
mod hyperliquid;
mod ipc_collector;
mod throttler;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path for the files where collected data will be written.
    path: String,

    /// Name of the exchange (ignored when --ipc is used)
    #[arg(default_value = "")]
    exchange: String,

    /// Symbols for which data will be collected (ignored when --ipc is used)
    symbols: Vec<String>,

    /// IPC mode: subscribe to a running connector instead of direct exchange connection.
    /// Provide the connector name (e.g., "bf" for a connector started with --name bf)
    #[arg(long)]
    ipc: Option<String>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), anyhow::Error> {
    let args = Args::parse();

    tracing_subscriber::fmt::init();

    let (writer_tx, mut writer_rx) = unbounded_channel();

    // Check if IPC mode is requested
    let _handle = if let Some(connector_name) = args.ipc {
        // IPC mode: subscribe to a running connector
        if args.symbols.is_empty() {
            return Err(anyhow!(
                "At least one symbol is required for IPC mode. Example: collector ./data --ipc bf btcusdt"
            ));
        }
        info!(%connector_name, symbols = ?args.symbols, "Starting in IPC mode");
        tokio::spawn(ipc_collector::run_ipc_collection(
            connector_name,
            args.symbols,
            writer_tx,
        ))
    } else {
        // Direct connection mode: connect to exchange WebSocket
        if args.exchange.is_empty() {
            return Err(anyhow!(
                "Exchange name is required in direct mode. Use --ipc for IPC mode."
            ));
        }

        match args.exchange.as_str() {
            "binancefutures" | "binancefuturesum" => {
                let streams = [
                    "$symbol@trade",
                    "$symbol@bookTicker",
                    "$symbol@depth@0ms",
                    // "$symbol@@markPrice@1s"
                ]
                .iter()
                .map(|stream| stream.to_string())
                .collect();

                tokio::spawn(binancefuturesum::run_collection(
                    streams,
                    args.symbols,
                    writer_tx,
                ))
            }
            "binancefuturescm" => {
                let streams = [
                    "$symbol@trade",
                    "$symbol@bookTicker",
                    "$symbol@depth@0ms",
                    // "$symbol@@markPrice@1s"
                ]
                .iter()
                .map(|stream| stream.to_string())
                .collect();

                tokio::spawn(binancefuturescm::run_collection(
                    streams,
                    args.symbols,
                    writer_tx,
                ))
            }
            "binance" | "binancespot" => {
                let streams = ["$symbol@trade", "$symbol@bookTicker", "$symbol@depth@100ms"]
                    .iter()
                    .map(|stream| stream.to_string())
                    .collect();

                tokio::spawn(binance::run_collection(streams, args.symbols, writer_tx))
            }
            "bybit" => {
                let topics = [
                    "orderbook.1.$symbol",
                    "orderbook.50.$symbol",
                    "orderbook.500.$symbol",
                    "publicTrade.$symbol",
                ]
                .iter()
                .map(|topic| topic.to_string())
                .collect();

                tokio::spawn(bybit::run_collection(topics, args.symbols, writer_tx))
            }
            "hyperliquid" => {
                let subscriptions = ["trades", "l2Book", "bbo"]
                    .iter()
                    .map(|sub| sub.to_string())
                    .collect();

                tokio::spawn(hyperliquid::run_collection(
                    subscriptions,
                    args.symbols,
                    writer_tx,
                ))
            }
            exchange => {
                return Err(anyhow!("{exchange} is not supported."));
            }
        }
    };

    let mut writer = Writer::new(&args.path);
    loop {
        select! {
            _ = signal::ctrl_c() => {
                info!("ctrl-c received");
                break;
            }
            r = writer_rx.recv() => match r {
                Some((recv_time, symbol, data)) => {
                    if let Err(error) = writer.write(recv_time, symbol, data) {
                        error!(?error, "write error");
                        break;
                    }
                }
                None => {
                    break;
                }
            }
        }
    }
    // let _ = handle.await;
    Ok(())
}
