# Capital scaling for the spot account

How position sizing and the safety caps depend on the capital of the Kraken spot account. The
rules below are the plan. The spot rules are implemented in WP-11, after the economics screen. The
code today sizes perpetual futures (`RiskPolicy::size_order`, `crates/domain/src/risk.rs`).

## Reference account

- **Operating capital** is a parameter, `operating_capital_eur`. Its value is decided at GP
  (WP-17), after paper trading. Every figure here uses 5,000 EUR as the reference.
- **Risk base** is `min(operating capital, EUR equity of the account)`. The system never sizes on
  money it was not given, and never on more than the account holds.
- **Spot has no leverage and no short selling.** The leverage cap is 1, the funding is zero, and a
  sell without a position is rejected (`RiskError::ShortNotSupported`, WP-11).

## Sizing (planned, WP-11)

1. `risk_eur = risk base * risk_per_trade_pct`. The starting point is 0.5 % to 1 %; simulation
   fixes it.
2. `quantity = risk_eur / stop distance`.
3. Round the price to the pair's tick and the quantity to its lot, both from the AssetPairs
   catalogue (WP-09). Reject an order below the pair's minimum.
4. Reject an order that breaks a cap. Never clamp it to fit.

## Caps and breakers

- The daily and monthly breakers are measured on the risk base, not on the initial capital.
- The absolute caps on order and position notional are in EUR, in the spot artifact. They follow
  the operating capital. Raising them is an explicit change, never a side effect of a new balance.

## Fees decide the strategy

A round trip costs 0.65 % of the notional when the take profit exits as a maker, and 0.80 % when
the stop loss exits as a taker, at the base tier (plan v4, §2.6). Costs are paid on the whole
notional, so the strategy must clear them trade by trade before any sizing question matters. The
economics screen measures exactly that.

## Re-optimisation

- A large change of operating capital changes the lot rounding and the caps. Re-run the optimizer
  (WP-12) when the change is more than 2x.
- Otherwise, a backtest with the same parameters and the new context is enough. Compare it with
  the previous run before funding anything.

## Checklist before raising the capital

1. At least 100 closed trades in paper trading over four weeks, with no breaker tripped.
2. Positive net PnL after fees, and a realised drawdown under two thirds of both limits.
3. Raise the capital by at most 2x per step. Update the artifact, re-run the comparison backtest,
   and only then fund the account.
