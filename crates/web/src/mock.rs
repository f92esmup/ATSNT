//! Mock market data ticker to simulate live trading events for UI testing and demos.

use std::time::Duration;

use backtest::PaperTradingEvent;
use domain::{DollarBar, OrderIntent, PositionSide, Side};
use rust_decimal_macros::dec;
use tokio::sync::broadcast;
use tracing::info;

/// Runs a synthetic background ticker generating realistic BTCUSDT Dollar Bars and PnL events.
pub fn spawn_mock_ticker(sender: broadcast::Sender<PaperTradingEvent>) {
    tokio::spawn(async move {
        info!("Spawning synthetic background ticker for UI demo and live testing");
        let mut price = dec!(64500.00);
        let mut bar_id = 1u64;
        let mut in_position = false;
        let mut entry_price = dec!(0.0);
        let mut ticks_since_bar = 0;

        let mut bar_open = price;
        let mut bar_high = price;
        let mut bar_low = price;
        let mut total_vol = dec!(0.0);

        loop {
            tokio::time::sleep(Duration::from_millis(800)).await;

            // Random price walk
            let delta = match bar_id % 7 {
                0 | 3 => dec!(15.50),
                1 | 4 => dec!(-12.00),
                2 => dec!(25.00),
                _ => dec!(-8.50),
            };
            price += delta;

            if price > bar_high {
                bar_high = price;
            }
            if price < bar_low {
                bar_low = price;
            }
            total_vol += dec!(0.35);
            ticks_since_bar += 1;

            // Mark to market
            if in_position {
                let unrealized_pnl = (price - entry_price) * dec!(0.15);
                let total_equity = dec!(10000.00) + unrealized_pnl;
                let _ = sender.send(PaperTradingEvent::MarkToMarket {
                    current_price: price,
                    unrealized_pnl,
                    total_equity,
                    drawdown_pct: if unrealized_pnl < dec!(0.0) {
                        (-unrealized_pnl) / dec!(10000.00)
                    } else {
                        dec!(0.0)
                    },
                });
            }

            // Form bar every 5 ticks
            if ticks_since_bar >= 5 {
                let now_ms = chrono_ms();
                let bar = DollarBar {
                    start_time: now_ms - 4000,
                    end_time: now_ms,
                    open: bar_open,
                    high: bar_high,
                    low: bar_low,
                    close: price,
                    volume: total_vol,
                    dollar_volume: total_vol * price,
                    trade_count: ticks_since_bar as u64,
                };

                let _ = sender.send(PaperTradingEvent::BarFormed(bar));

                // Occasional simulated signal and trade
                if bar_id % 8 == 0 && !in_position {
                    in_position = true;
                    entry_price = price;
                    let sl = price * dec!(0.985);
                    let tp = price * dec!(1.025);

                    if let Ok(intent) = OrderIntent::new(now_ms, Side::Buy, price, sl, tp, 15) {
                        let _ = sender.send(PaperTradingEvent::SignalGenerated(intent));
                    }

                    let _ = sender.send(PaperTradingEvent::PositionOpened {
                        side: PositionSide::Long,
                        entry_price: price,
                        quantity: dec!(0.15),
                        stop_loss: sl,
                        take_profit: tp,
                    });
                } else if in_position && bar_id % 15 == 0 {
                    in_position = false;
                    let net_pnl = (price - entry_price) * dec!(0.15);
                    let _ = sender.send(PaperTradingEvent::PositionClosed {
                        exit_reason: if net_pnl > dec!(0) {
                            "TakeProfit".to_string()
                        } else {
                            "StopLoss".to_string()
                        },
                        exit_price: price,
                        net_pnl,
                        total_equity: dec!(10000.00) + net_pnl,
                    });
                }

                // Reset for next bar
                bar_id += 1;
                ticks_since_bar = 0;
                bar_open = price;
                bar_high = price;
                bar_low = price;
                total_vol = dec!(0.0);
            }
        }
    });
}

fn chrono_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
