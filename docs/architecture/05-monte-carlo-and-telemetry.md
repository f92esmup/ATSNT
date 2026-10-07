# Monte Carlo Stress-Testing & Web Telemetry Specification

## 1. Quantitative Foundation: Why Monte Carlo?

A single historical backtest is merely **one realized path** out of an infinite number of paths the market could have taken. 

Believing that a strategy is safe because its backtest achieved a maximum drawdown of $-6.5\%$ is the quintessential novice fallacy. If the winning and losing trades had occurred in a different sequence, that same strategy might have experienced a $-28\%$ drawdown or triggered a liquidation event.

Following Marcos López de Prado (*Advances in Financial Machine Learning*, Chapters 12, 13, and 16), ATSNT introduces a **Post-Backtest Monte Carlo Stress-Testing Engine**.

```text
Historical Backtest
      │
      ▼
┌──────────────┐
│ Trade Engine │ ──► Realized Trade Sequence [T₁, T₂, T₃, ..., Tₙ]
└──────────────┘
      │
      ▼
┌────────────────────────────────────────────────────────┐
│ Monte Carlo Resampling Engine (10,000 Iterations)      │
├────────────────────────────────────────────────────────┤
│ • Bootstrapped Trade Sequence Resampling (IID / Block) │
│ • Synthetic Equity Curve Generation                   │
│ • Distribution Analysis (Percentiles 1%, 5%, 50%, 95%) │
└────────────────────────────────────────────────────────┘
      │
      ▼
┌────────────────────────────────────────────────────────┐
│ Risk & Ruin Assessment                                │
├────────────────────────────────────────────────────────┤
│ • Max Drawdown Distribution: P(MaxDD > 20%)            │
│ • Longest Underwater Duration Distribution             │
│ • Probability of Account Ruin (Loss > 30%)             │
└────────────────────────────────────────────────────────┘
```

---

## 2. Bootstrapping Methodologies

### 2.1 Why Not Resample Price Bars?
Resampling raw price bars (e.g., shuffling candlestick closes) destroys:
1. Autocorrelation and volatility clustering ($ARCH$ effects).
2. Microstructure features and Dollar Bar liquidity boundaries.
3. CUSUM filter state continuity.

### 2.2 Resampled Trade Sequence (Discrete Event Bootstrap)
Once a strategy completes an In-Sample (IS) or Out-of-Sample (OOS) backtest, it yields a discrete sequence of realized trade returns:
$$R = \{r_1, r_2, \dots, r_M\} \quad \text{where } r_k = \frac{\text{Exit Price} - \text{Entry Price}}{\text{Entry Price}} \times \text{Side} - \text{Fees}$$

We perform $N = 10,000$ Monte Carlo simulations:
1. **Standard Resampling (with replacement)**: Draws $M$ trades uniformly at random from $R$.
2. **Circular Block Bootstrap (CBB)**: Draws blocks of consecutive trades of length $L$ (e.g., $L = 5$) to preserve potential short-term trade dependency or regime streaks (winning/losing clusters).

For each simulation path $j \in [1, N]$:
$$E_j(t) = E_0 \cdot \prod_{k=1}^t (1 + r_{\pi_j(k)})$$
Where $E_0$ is initial capital and $\pi_j$ is the permutation index sequence for simulation $j$.

---

## 3. Evaluated Risk Distributions

From the $N$ synthetic equity curves, the engine computes:

1. **Drawdown Distribution**:
   For each path $j$, calculate the maximum peak-to-trough decline $\text{MaxDD}_j$:
   $$\text{MaxDD}_j = \max_{t} \left( \frac{\max_{\tau \le t} E_j(\tau) - E_j(t)}{\max_{\tau \le t} E_j(\tau)} \right)$$
   Calculate percentiles: $p_{50}$ (Median), $p_{95}$, $p_{99}$, and $\text{Worst Case}$.

2. **Underwater Duration (Recovery Time)**:
   The maximum number of trades or time duration spent below the previous high-water mark.

3. **Probability of Ruin ($P_{\text{ruin}}$)**:
   The proportion of simulated paths where equity dropped below a critical survival threshold (e.g., $E_t \le 0.70 \cdot E_0$, indicating a 30% account drawdown):
   $$P_{\text{ruin}} = \frac{1}{N} \sum_{j=1}^N \mathbb{I}\left(\min_t E_j(t) \le E_{\text{ruin}}\right)$$

---

## 4. Telemetry & Web Presentation Architecture

For proposed read-only research catalog/detail/comparison/replay requirements, see the [canonical workspace contract](09-read-only-web-workspace.md#5-proposed-laboratory-contract), especially W4/W5. The schemas and route examples here are design specifications, not guarantees of the current web API (documented in [the dashboard contract](07-web-telemetry-dashboard.md#31-rest-api-specification)). The example `fan_chart_trajectories` differs from the current browser's `fan_chart_curves` reader; W4 must reconcile actual report producers and consumers without assuming either example is authoritative for stored reports.

### 4.1 The "Anti-Bloat" Storage Contract
A critical architectural pitfall in quantitative dashboards is attempting to serialize and persist all $10,000$ synthetic equity curves at tick or trade resolution. Doing so creates massive multi-gigabyte JSON files that choke browser rendering, exhaust server memory, and degrade I/O throughput.

**The ATSNT Telemetry Principle**:
- **Discard**: Raw trajectory paths for non-representative Monte Carlo iterations.
- **Retain**: Statistical distributions (histograms, percentiles) and a tightly bounded sample envelope (**Fan Chart** / **Cone of Uncertainty**) of exactly 50 curves:
  - 1 Realized historical path.
  - 5 Worst paths (stress bounds).
  - 5 Best paths (optimistic ceiling).
  - 40 Quantile-spaced representative paths.

```text
Equity ($)
      ▲
      │                                     ╭──── 99th Percentile (Upper Bound)
      │                               ╭─────╯
      │                         ╭─────╯─────────── 75th Percentile
      │                   ╭─────╯
      │             ╭─────╯─────────────────────── Realized Backtest Path
      │       ╭─────╯
      │ ──────┼─────────────────────────────────── 25th Percentile
      │       ╰─────╮
      │             ╰─────╮─────────────────────── 5th Percentile (Risk Floor)
      │                   ╰─────╮
      │                         ╰───────────────── 1st Percentile (Worst Case Stress)
      └──────────────────────────────────────────► Number of Trades
```

---

## 5. Structured Telemetry Schemas

All simulation, HPO, and backtest results are serialized to `storage/reports/` as machine-readable JSON artifacts, ready to be ingested by the `crates/web` REST/WebSocket API.

### 5.1 Monte Carlo Report (`storage/reports/monte_carlo_<id>.json`)
```json
{
  "report_id": "mc_20261004_133000_btc",
  "strategy_id": "dollar_bars_cusum",
  "iterations": 10000,
  "resample_method": "CircularBlockBootstrap",
  "block_size": 5,
  "initial_capital": "10000.00",
  "ruin_threshold_pct": "30.00",
  "metrics_summary": {
    "historical_max_drawdown_pct": "6.85",
    "p50_max_drawdown_pct": "8.12",
    "p95_max_drawdown_pct": "14.40",
    "p99_max_drawdown_pct": "21.05",
    "worst_max_drawdown_pct": "26.30",
    "probability_of_ruin_pct": "0.12",
    "p50_underwater_trades": 18,
    "p95_underwater_trades": 45
  },
  "distribution_histogram": {
    "drawdown_bins": ["0-5%", "5-10%", "10-15%", "15-20%", "20-25%", ">25%"],
    "frequency": [1200, 5800, 2400, 520, 75, 5]
  },
  "fan_chart_trajectories": [
    {
      "label": "Realized",
      "equity_curve": ["10000.00", "10045.20", "10120.00", "10550.00"]
    },
    {
      "label": "P01_Worst",
      "equity_curve": ["10000.00", "9850.00", "9620.00", "9200.00"]
    },
    {
      "label": "P99_Best",
      "equity_curve": ["10000.00", "10180.00", "10450.00", "11200.00"]
    }
  ]
}
```

### 5.2 Backtest Run Report (`storage/reports/backtest_<id>.json`)
Contains:
1. `metadata`: Timestamp, git commit SHA, data source, date range.
2. `config`: Parameter coordinates $\theta$.
3. `summary_metrics`: Total PnL, Sortino, Sharpe, Profit Factor, Fee Drag, Expectancy.
4. `trade_manifest`: Array of completed trades (`TradeRecord`: timestamps, entry/exit prices, fees, slippage, realized PnL).
5. `downsampled_equity`: Downsampled timeseries (e.g. at every trade exit or 1-hour bar) to render smooth, lightweight charts in web frontends.

---

## 6. Hexagonal Placement in ATSNT

To respect our architectural principles:
- **`crates/domain`**: Implements pure mathematical functions (`bootstrap_resample`, `compute_drawdown_distribution`, `calculate_ruin_probability`). Zero I/O, zero network, 100% deterministic via seeded RNG.
- **`crates/backtest`**: Coordinates execution of the strategy over the event stream, invokes domain Monte Carlo routines, and compiles the `BacktestReport`.
- **`crates/adapters`**: Implements file system storage (`FileSystemReportStore`) or future database adapters (`SqliteReportStore`) to persist reports.
- **`crates/web`**: Exposes read endpoints (`GET /api/v1/reports/backtests`, `GET /api/v1/reports/monte-carlo/:id`) and streams updates via WebSockets.
