//! Live market data ingestion and real-time Dollar Bar aggregation for ATSNT.
//!
//! Subscribes to the Binance Futures WebSocket aggregate trade stream and feeds
//! incoming trades into a `DollarBarAggregator`, printing finalized dollar bars.

use adapters::{AsyncMarketDataStream, BinanceWebSocketStream, BinanceWsConfig};
use anyhow::Result;
use clap::Parser;
use domain::DollarBarAggregator;
use rust_decimal::Decimal;

#[derive(Parser, Debug)]
#[command(
    name = "stream_trades",
    about = "Stream live market trades from Binance WebSocket and aggregate into Dollar Bars"
)]
struct Args {
    /// Trading pair symbol (e.g. btcusdt, ethusdt)
    #[arg(short, long, default_value = "btcusdt")]
    symbol: String,

    /// Dollar bar sampling threshold in USDT
    #[arg(short, long, default_value = "100000")]
    threshold: Decimal,

    /// Connect to Binance Spot WebSocket instead of Futures
    #[arg(long, default_value_t = false)]
    spot: bool,

    /// Custom WebSocket stream URL template
    #[arg(long)]
    url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();

    let args = Args::parse();
    println!("=== ATSNT Live Market Data Ingestion ===");
    println!("Symbol: {}", args.symbol.to_uppercase());
    println!("Dollar Bar Threshold: ${}", args.threshold);

    let config = if let Some(custom_url) = args.url {
        BinanceWsConfig {
            base_url: custom_url,
            symbol: args.symbol.clone(),
            ..Default::default()
        }
    } else if args.spot {
        BinanceWsConfig::spot(&args.symbol)
    } else {
        BinanceWsConfig::futures(&args.symbol)
    };
    println!("Connecting to: {}", config.stream_url());

    let mut stream = BinanceWebSocketStream::connect(config)?;
    let mut aggregator = DollarBarAggregator::new(args.threshold)?;

    println!("Awaiting live trades (press Ctrl+C to terminate)...\n");

    loop {
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutdown signal received. Exiting trade streamer...");
                break;
            }
            res = stream.next_trade() => {
                match res {
                    Ok(Some(trade)) => {
                        println!(
                            "[TRADE] ts: {} | price: {:>10} | qty: {:>8} | side: {:<4} | notional: ${:>10}",
                            trade.timestamp,
                            trade.price,
                            trade.quantity,
                            format!("{:?}", trade.side),
                            trade.dollar_value()
                        );

                        if let Some(bar) = aggregator.process_trade(&trade) {
                            println!(
                                "\n------------------------------------------------------------\n\
                                 >>> FINALIZED DOLLAR BAR <<<\n\
                                 Time Window:  {} -> {} ({} ms)\n\
                                 OHLC:         O: {} | H: {} | L: {} | C: {}\n\
                                 Volume:       {} base | ${} dollar volume\n\
                                 Trade Count:  {}\n\
                                 ------------------------------------------------------------\n",
                                bar.start_time,
                                bar.end_time,
                                bar.end_time - bar.start_time,
                                bar.open,
                                bar.high,
                                bar.low,
                                bar.close,
                                bar.volume,
                                bar.dollar_volume,
                                bar.trade_count
                            );
                        }
                    }
                    Ok(None) => {
                        println!("Trade stream reached EOF or was terminated.");
                        break;
                    }
                    Err(err) => {
                        eprintln!("Error receiving trade: {err}");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
