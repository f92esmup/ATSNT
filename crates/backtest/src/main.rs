use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use adapters::{BinanceCsvReader, MarketDataStream};
use anyhow::{Context, Result};
use backtest::{BacktestConfig, BacktestEngine};
use domain::DollarBarAggregator;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{DollarBarsCusumConfig, DollarBarsCusumStrategy, Strategy};

fn main() -> Result<()> {
    println!("============================================================");
    println!("     ATSNT - Quantitative Backtesting Engine (100% Rust)    ");
    println!("============================================================");

    let data_path = Path::new("data/sample_trades.csv");
    println!(
        "[*] Loading market data stream from: {}",
        data_path.display()
    );

    let file =
        File::open(data_path).with_context(|| format!("Failed to open {}", data_path.display()))?;
    let mut reader = BinanceCsvReader::new(BufReader::new(file));

    // 1. Initialize Dollar Bar Aggregator (Threshold: $200,000 per bar)
    let dollar_threshold = dec!(200_000);
    let mut aggregator = DollarBarAggregator::new(dollar_threshold)
        .map_err(|e| anyhow::anyhow!("Failed to initialize aggregator: {e}"))?;

    println!("[*] Configured DollarBarAggregator with threshold: ${dollar_threshold}");

    // 2. Initialize Strategy (Dollar Bars + CUSUM + Z-Score)
    let strat_config = DollarBarsCusumConfig {
        rolling_window_len: 4,
        cusum_vol_multiplier: dec!(1.0),
        z_entry_threshold: dec!(1.5),
        z_stop_threshold: dec!(3.0),
        time_barrier_bars: 10,
    };
    let mut strategy = DollarBarsCusumStrategy::new(strat_config)
        .map_err(|e| anyhow::anyhow!("Failed to initialize strategy: {e}"))?;

    println!("[*] Strategy loaded: {}", strategy.name());

    // 3. Initialize Backtest Engine with 1:1 realistic friction
    let engine_config = BacktestConfig {
        initial_capital: dec!(10_000),
        maker_fee_pct: dec!(0.0002),    // 0.02%
        taker_fee_pct: dec!(0.0005),    // 0.05%
        slippage_pct: dec!(0.0005),     // 0.05%
        risk_per_trade_pct: dec!(0.01), // 1.0% risk per trade
        max_daily_drawdown_pct: dec!(0.05),
    };
    let mut engine = BacktestEngine::new(engine_config);

    println!("[*] Initial Capital: ${}", engine.cash_equity());
    println!("[*] Replaying discrete market trade ticks...\n");

    let mut total_trades_processed = 0usize;
    let mut total_bars_emitted = 0usize;
    let mut last_price = Decimal::ZERO;

    while let Some(trade) = reader.next_trade()? {
        total_trades_processed += 1;
        last_price = trade.price;

        if let Some(bar) = aggregator.process_trade(&trade) {
            total_bars_emitted += 1;
            println!(
                "  [DollarBar #{:02}] Close: ${:.2} | Vol: {:.2} BTC | Notional: ${:.2}",
                total_bars_emitted, bar.close, bar.volume, bar.dollar_volume
            );
            engine.process_bar(&mut strategy, &bar);
        }
    }

    let mark = if last_price > Decimal::ZERO {
        Some(last_price)
    } else {
        None
    };
    let metrics = engine.finish(mark);

    println!("\n============================================================");
    println!("               QUANTITATIVE PERFORMANCE REPORT              ");
    println!("============================================================");
    println!(" Raw Ticks Processed:       {}", total_trades_processed);
    println!(" Dollar Bars Formed:        {}", total_bars_emitted);
    println!(" Total Trades Executed:     {}", metrics.total_trades);
    println!(" Winning Trades:            {}", metrics.winning_trades);
    println!(" Losing Trades:             {}", metrics.losing_trades);
    println!(
        " Win Rate:                  {:.2}%",
        metrics.win_rate * dec!(100)
    );
    println!(" Profit Factor:             {:.2}", metrics.profit_factor);
    println!(" Payoff Ratio:              {:.2}", metrics.payoff_ratio);
    println!(
        " Mathematical Expectancy:   ${:.2} per trade",
        metrics.mathematical_expectancy
    );
    println!("------------------------------------------------------------");
    println!(" Gross Profit:              ${:.2}", metrics.gross_profit);
    println!(" Gross Loss:                ${:.2}", metrics.gross_loss);
    println!(" Net Profit (Real PnL):     ${:.2}", metrics.net_profit);
    println!(" Total Fees Paid (Friction):${:.2}", metrics.total_fees);
    println!(" Total Slippage Paid:       ${:.2}", metrics.total_slippage);
    println!(
        " Max Drawdown ($):          ${:.2}",
        metrics.max_drawdown_amount
    );
    println!(
        " Max Drawdown (%):          {:.2}%",
        metrics.max_drawdown_pct * dec!(100)
    );
    println!(" Sortino Ratio:             {:.2}", metrics.sortino_ratio);
    println!("============================================================");

    Ok(())
}
