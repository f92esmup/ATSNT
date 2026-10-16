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

The schemas and route examples in this section are historical design specifications from before the in-process web server was decommissioned (Milestone 5); they are not a current API. Consumers read the BigQuery views described in [12-bi-data-contract.md](12-bi-data-contract.md).

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

## 5. Persisted Telemetry (Run Model)

Results are not written to local JSON files. With `--gcp-bigquery`, a Monte Carlo stress test is recorded as a `monte_carlo` run whose `parent_run_id` is the backtest run it resampled (see [10-gcp-cloud-architecture.md §4](10-gcp-cloud-architecture.md#4-bigquery-data-warehouse-schemas-atsnt_bi)):

- `run_metrics` (`scope = full`): `extras` JSON with every percentile (`p01`..`p99`, worst), `probability_of_ruin_pct`, underwater statistics, and the simulation settings actually used (`iterations`, `resample_method`, `block_size`, `ruin_threshold_pct`, `seed`).
- `run_series` (`series = mc_equity`, `x` = trade index): 8 representative curves from the fan chart — `Realized`, `Worst_Stress_1`, `Best_Ceiling_1`, and `Quantile_0.05/0.25/0.50/0.75/0.95` (ranked by final equity, best first).
- No trades are stored for Monte Carlo runs.

---

## 6. Hexagonal Placement in ATSNT

To respect our architectural principles:
- **`crates/domain`**: Implements pure mathematical functions (`bootstrap_resample`, `compute_drawdown_distribution`, `calculate_ruin_probability`). Zero I/O, zero network, 100% deterministic via seeded RNG.
- **`crates/backtest`**: Coordinates execution of the strategy over the event stream, invokes domain Monte Carlo routines, and compiles the `BacktestReport`.
- **`crates/adapters`**: Implements the `RunRecorder` port (`BigQuerySink`, plus `InMemoryRunRecorder` for tests) that persists runs, metrics, trades, and event series to BigQuery for downstream consumers, `GcsArtifactSink` for deployment artifact uploads, and `TelegramNotifier` for real-time mobile push alerting. `crates/backtest::recording` translates domain results into those rows.
