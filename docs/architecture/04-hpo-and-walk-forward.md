# High-Performance Hyperparameter Optimization (HPO) & Walk-Forward Specification

## 1. Objective & Quantitative Philosophy
To establish a statistically rigorous, highly parallelized Hyperparameter Optimization (HPO) engine in Rust, capable of finding robust parameter configurations without succumbing to the **Backtest Overfitting Problem** (Marcos López de Prado, *AFML* Chapters 11 & 14).

In financial time series, optimizing parameters solely to maximize historical profit guarantees selecting random noise (data-snooping bias). The ATSNT HPO engine enforces **Walk-Forward Validation**, **Parameter Stability Scoring**, and the **Deflated Sharpe Ratio (DSR)**.

---

## 2. The Anti-Overfitting Framework

### 2.1 Walk-Forward Optimization (WFO)
Rather than tuning on a single historical period, the dataset is split into sequential temporal slices:

```text
Time ─────────────────────────────────────────────────────────────────────────────►
Fold 1:  [ Train / In-Sample (3M) ]──[Embargo]──►[ Test / Out-of-Sample (1M) ]
Fold 2:        [ Train / In-Sample (3M) ]──[Embargo]──►[ Test / Out-of-Sample (1M) ]
Fold 3:              [ Train / In-Sample (3M) ]──[Embargo]──►[ Test / Out-of-Sample (1M) ]
```

- **In-Sample (IS)**: The optimizer explores candidate configurations.
- **Embargo Period (AFML Ch. 7)**: A deliberate buffer gap between train and test sets to eliminate information leakage caused by serial correlation in rolling indicators (`RollingZScore` lookback window) and position holding periods.
- **Out-of-Sample (OOS)**: The best parameters from the IS fold are evaluated on unseen data. A strategy is considered viable only if OOS performance preserves positive expectancy across folds.

---

### 2.2 Parameter Stability Plateau vs. Overfitted Peaks
A common mistake in retail optimization is selecting the global maximum parameter coordinate ($\theta^*$), which is frequently a fragile, isolated spike caused by noise:

```text
Performance (Sortino)
        ▲
        │         ▲ Overfitted Peak (Dangerous: tiny volatility shift destroys edge)
        │        ╱ ╲
        │       ╱   ╲           ╭───────────────╮
        │      ╱     ╲          │ Robust Plateau│ (Target: neighboring parameters
        │     ╱       ╲─────────╯               ╰────────
        └─────┴──────────────────────────────────────────► Parameter Space (θ)
```

#### Fitness function with stability penalty
For every candidate $\theta$ the optimizer runs each fold's train (IS) and test (OOS) slice. The ranking is:

$$\text{Fitness}(\theta) = \max\left(0, \overline{\text{Sortino}}_{\text{OOS}}(\theta)\right) \times \text{Stability}(\theta)$$

It applies only to an eligible candidate (section 2.4) whose mean IS Sortino is positive; every other candidate has fitness 0. The ranking lists the eligible candidates first, by descending fitness, and the ineligible ones after them. Equal fitness falls back to the higher OOS Sortino. For example, when no candidate is profitable, every fitness is 0, and the least bad candidate comes first. The OOS folds are the walk-forward test slices. The final, untouched check is the holdout backtest of the winner (`--holdout-from-month`).

$\text{Stability}(\theta)$ is the share of the base performance that its grid neighbours keep. The neighbours are one step down and one step up the grid for each of the six parameters, the others held fixed: up to 12 neighbours. Each neighbour runs on fold 0's training slice:

$$\text{Stability}(\theta) = \text{clamp}\left(\frac{\frac{1}{k}\sum_{i=1}^{k} \max\left(0, S(\theta + \delta_i)\right)}{S(\theta)}, 0, 1\right)$$

- A neighbour with fewer trades than the minimum, or with an undefined Sortino, counts as 0.
- When the base Sortino is not positive, stability is 0.
- 1 means a plateau: every step performs at least as well.
- 0.5 means a step loses half of the edge on average.
- Ineligible candidates skip this computation, which costs up to 12 backtests per candidate.

#### Sortino ratio (per trade)
The return of a trade is $r_i = \text{pnl}^{\text{net}}_i / \text{equity at entry}_i$. Sizing risks a fraction of equity, so this is the return the account sees. The Sortino ratio uses all $N$ trades with a target return of 0:

$$\text{Sortino} = \frac{\bar r}{\sqrt{\frac{1}{N}\sum_{i=1}^{N} \min(r_i, 0)^2}}$$

It is **undefined** (`None`, NULL in BigQuery) without trades or without downside. A fixed cap there (the former `100`) reads as an excellent measurement, so the ratio is left undefined instead. The profit factor follows the same rule: it is undefined without losing trades. In a fold mean, an undefined Sortino counts as 0.

---

### 2.3 Statistical Significance: Deflated Sharpe Ratio (DSR)
When an HPO sweep evaluates $N$ configurations, the maximum observed Sharpe ratio increases purely as a function of the number of trials, even if every strategy is a random walk. The Deflated Sharpe Ratio (Bailey and López de Prado, 2014) tests the observed Sharpe ratio against the best one expected from luck:

$$\text{SR}_0 = \sqrt{V[\widehat{\text{SR}}_n]}\left((1-\gamma)\,\Phi^{-1}\left(1-\tfrac{1}{N}\right) + \gamma\,\Phi^{-1}\left(1-\tfrac{1}{N e}\right)\right)$$

$$p = 1 - \Phi\left(\frac{(\widehat{\text{SR}} - \text{SR}_0)\sqrt{T - 1}}{\sqrt{1 - \gamma_3 \widehat{\text{SR}} + \frac{\gamma_4 - 1}{4}\widehat{\text{SR}}^2}}\right)$$

The terms in this implementation (`deflated_sharpe_p_value`):

- $\widehat{\text{SR}}$ is the per-trade Sharpe ratio (mean over sample standard deviation) of the candidate's out-of-sample trade returns, concatenated over all folds.
- $T$ is the number of those trades.
- $\gamma_3$ and $\gamma_4$ are their skewness and kurtosis (not excess).
- $V[\widehat{\text{SR}}_n]$ is the variance of $\widehat{\text{SR}}$ across all trials that have one.
- $N$ is the number of candidates evaluated.
- $\gamma$ is the Euler-Mascheroni constant.

Only **eligible** trials (section 2.4) get a p-value and enter $V[\widehat{\text{SR}}_n]$. Every other trial gets $p = 1$:
- A Sharpe ratio over a handful of returns is not an estimate. Two near-equal returns give a per-trade Sharpe of 20.
- One such outlier would raise $\text{SR}_0$ enough to deflate every real trial to $p = 1$.

$N$ still counts every candidate tried, because the selection bias comes from all of them.

A candidate is significant at $p < 0.05$. Significance does not change the ranking. The winner's significance is printed, stored in the artifact scorecard and recorded in BigQuery.

#### Deployment gate
`paper_trading` and `live_trading` refuse at start-up an artifact whose scorecard shows no deployable edge (`HpoScorecard::deployment_blocker`):
- The scorecard records no eligible fold (`min_fold_trades` = 0): it predates the minimum-trades rule, or the search ran with `--min-trades-per-fold 0`. Re-run the HPO with the minimum.
- `fitness` <= 0: no positive out-of-sample edge.
- The Deflated Sharpe Ratio is not significant.

The search still writes such a winner, so its holdout backtest can be run and inspected. The backtest prints a `[GATE]` line when paper and live trading would refuse it. `--allow-unproven-strategy` runs it anyway, for instance to test the mechanics on testnet. It prints a warning with the reason, or a notice that it had no effect when nothing was refused. Files without a scorecard (hand-written configurations) carry no evidence either way, and the gate lets them pass.

---

### 2.4 Minimum trades per fold
A ratio measured on a handful of trades says more about luck than about the rule: two winning trades have no downside to measure.

- A candidate is **eligible** only when every IS and every OOS fold closed at least `--min-trades-per-fold` trades (default 30, `DEFAULT_MIN_TRADES_PER_FOLD`).
- Ineligible candidates are recorded with their metrics, but they never win.
- When no candidate is eligible, `run_hpo` records the trials and exits non-zero without writing or uploading an artifact.
- The fewest trades in any fold is reported per candidate as `min_fold_trades`.

#### Rejected signals
Signals the risk policy refuses are counted per reason in `BacktestMetrics::rejected_orders` and the `rejected_orders` column of `run_metrics`. Examples are a notional above the cap (`OrderSizeExceeded`), a quantity below one lot (`OrderBelowMinLotSize`), or a day halted by the daily circuit breaker (`DailyHalt`). A candidate with few trades and many rejections is constrained by the account size, not by the market.

---

## 3. High-Performance Concurrency: `Rayon` (CPU-Bound Parallelism)

### 3.1 Why `Rayon` over `Tokio`?
- **`tokio`** is an asynchronous runtime designed for non-blocking I/O (network requests, socket multiplexing). Running heavy number-crunching on Tokio threads starves the event loop.
- **`rayon`** provides native work-stealing CPU parallelism. It saturates all available physical and logical CPU cores (8, 16, 32 threads) without context switching or lock contention.

### 3.2 Parallel Work Distribution
```rust
use rayon::prelude::*;

// High-throughput parallel grid or Bayesian surrogate evaluation
let results: Vec<OptimizationResult> = parameter_candidates
    .par_iter()
    .map(|params| evaluate_candidate_walk_forward(params, &dataset_folds))
    .collect();
```

---

## 4. Generic Multi-Strategy Architecture (Open-Closed Principle)

The optimization engine is completely decoupled from any single trading strategy. It interacts with strategies exclusively through traits defined in `crates/strategies`:

### 4.1 Trait Contracts: `HyperparameterSpace` & `OptimizableStrategy`

```rust
pub trait HyperparameterSpace: Send + Sync + Clone {
    /// Associated hyperparameter configuration type.
    type Config: Clone + Send + Sync + Serialize + DeserializeOwned + Debug + Display + 'static;

    /// Generates combinations/samples within the bounded parameter space.
    fn generate_candidates(&self, max_candidates: usize) -> Vec<Self::Config>;

    /// Perturbs neighboring coordinates around `base` for AFML local stability scoring.
    fn perturb_neighbors(&self, base: &Self::Config) -> Vec<Self::Config>;
}

pub trait OptimizableStrategy: Strategy + Sync + Sized {
    type Config: Clone + Send + Sync + Serialize + DeserializeOwned + Debug + Display + 'static;
    type Space: HyperparameterSpace<Config = Self::Config>;

    /// Canonical identifier for the strategy (e.g. "dollar_bars_cusum").
    fn id() -> &'static str;

    /// Constructs a strategy instance from its parameters.
    fn from_config(config: Self::Config) -> Result<Self, DomainError>;

    /// Provides the default hyperparameter search space.
    fn default_space() -> Self::Space;
}
```

### 4.2 Decoupled Engine Execution
The `WalkForwardOptimizer<S: OptimizableStrategy>` operates agnostically across Rayon worker threads:
- **Candidate Generation**: Delegated to `parameter_space.generate_candidates(max_candidates)`.
- **Plateau Stability**: Evaluated mathematically via `calculate_parameter_stability::<S>()`, delegating local coordinate perturbation to `space.perturb_neighbors(base)`.
- **Simulation**: Executed in isolation across folds via `run_backtest_slice::<S>(config, candidate, slice)`.

### 4.3 Integrating a New Strategy
To enable automated hyperparameter optimization for any new strategy, implement `HyperparameterSpace` and `OptimizableStrategy`:

```rust
impl OptimizableStrategy for MyNewStrategy {
    type Config = MyNewConfig;
    type Space = MyNewSpace;

    fn id() -> &'static str { "my_new_strategy" }
    fn from_config(config: Self::Config) -> Result<Self, DomainError> { Self::new(config) }
    fn default_space() -> Self::Space { MyNewSpace::default() }
}

// Optimization runs seamlessly with zero engine modifications:
let optimizer = WalkForwardOptimizer::<MyNewStrategy>::with_default_space(engine_config);
let results = optimizer.run_optimization(&bars, &folds, 60)?;
```

---

## 5. Configuration Persistence & Strategy Loading

### 5.1 Output Schema (`configs/hpo_results.json`)
The HPO engine produces a structured JSON artifact summarizing the optimization run:

```json
{
  "timestamp": 1700000000000,
  "strategy": "dollar_bars_cusum",
  "dataset": "BTCUSDT_2024_Q1_aggTrades",
  "trials_evaluated": 60,
  "winner": {
    "config": {
      "rolling_window_len": 20,
      "cusum_vol_multiplier": "2.00",
      "z_entry_threshold": "1.80",
      "z_stop_threshold": "3.50",
      "time_barrier_bars": 15
    },
    "mean_is_sortino": "0.21",
    "mean_oos_sortino": "0.14",
    "mean_oos_profit_factor": "1.32",
    "oos_sharpe": "0.081",
    "total_oos_trades": 412,
    "min_fold_trades": 37,
    "stability_score": "0.78",
    "fitness": "0.109",
    "dsr_p_value": "0.0120",
    "is_statistically_significant": true
  }
}
```

---

## 6. Automated Production Pipeline Integration

For unattended staging sweeps, the HPO engine is orchestrated automatically by [`scripts/run_production_pipeline.sh`](../../scripts/run_production_pipeline.sh):

```bash
# Standard execution (defaults to dollar_bars_cusum):
./scripts/run_production_pipeline.sh --symbol BTCUSDT --year 2026 --candidates 60 --folds 5

# Custom strategy with runtime parameter space override:
./scripts/run_production_pipeline.sh --strategy dollar_bars_cusum --param-space configs/custom_space.json

# Simulate every trial with a real account's risk, drawdown, leverage and notional caps:
./scripts/run_production_pipeline.sh --execution-context configs/execution_real_500eur.json --candidates 120

# Out-of-sample check: search January-August, backtest the winner on September onwards:
./scripts/run_production_pipeline.sh --year 2026 --holdout-from-month 9 --candidates 120
```

`--holdout-from-month MM` (2-12, not with `--month`) links the year's files into `splits/<symbol>-<year>-holdout-MM/{train,holdout}`:
the HPO reads the months before MM, the backtest MM onwards (the current month's daily archives included). It
fails when either side has no data.

`--execution-context` feeds the HPO stage only; the winning artifact carries the same context into
the backtest, paper and live runs, and the option is refused with `--skip-hpo`. The shipped search
space (`configs/param_space_daytrading_cusum.json`) covers the window, the CUSUM multiplier, the entry
and stop z-scores, the minimum take-profit floor (`min_take_profit_pcts`, the lever against fees) and
the intraday holding horizon (`time_barrier_bars`, 100 to 2,000 bars). See [Capital scaling](../capital-scaling.md) for how these
values depend on the account size.

1. **Purged Walk-Forward Folds**: Dynamically constructs rolling train/embargo/test folds across the in-memory Dollar Bar series.
2. **Rayon Thread Pool**: Evaluates candidate parameter combinations concurrently across all physical CPU cores.
3. **Artifact Serialization**: Persists the winning parameter set to `configs/hpo_<symbol>_<year>.json` and complete audit metrics to `storage/reports/hpo_run_<timestamp>.json`.
4. **Instant Backtest Handshake**: The downstream Backtest and Monte Carlo stage ingests the winning configuration artifact immediately with zero manual intervention.


