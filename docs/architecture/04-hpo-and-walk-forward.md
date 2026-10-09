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

#### Mathematical Fitness Function with Stability Penalty:
For any candidate parameter vector $\theta = (\theta_{\text{dollar}}, h_{\text{mult}}, Z_{\text{entry}}, Z_{\text{stop}}, K_{\text{bars}})$:
$$\text{Fitness}(\theta) = \text{Metric}_{\text{IS}}(\theta) \times \text{StabilityScore}(\theta)$$

Where $\text{StabilityScore}(\theta)$ measures the standard deviation of performance when perturbing $\theta$ by $\pm \epsilon$:
$$\text{StabilityScore}(\theta) = \exp\left(-\lambda \cdot \text{StdDev}\left(\{\text{Metric}(\theta + \delta_i)\}_{i=1}^k\right)\right)$$

This forces the search algorithm to select broad parameter **plateaus** where small regime shifts do not destroy profitability.

---

### 2.3 Statistical Significance: Deflated Sharpe Ratio (DSR)
When evaluating $N$ configurations during an HPO sweep, the maximum observed Sharpe ratio increases purely as a function of trials $N$, even if all strategies are random walks.

The **Deflated Sharpe Ratio (DSR)** adjusts the observed Sharpe ratio $\widehat{\text{SR}}$ for:
1. Number of parameter trials tested ($N$).
2. Return distribution non-normality (skewness $\gamma_3$ and kurtosis $\gamma_4$).
3. Length of observation track record ($T$).

$$\text{DSR} = \Phi\left(\frac{(\widehat{\text{SR}} - \text{SR}^*) \cdot \sqrt{T - 1}}{\sqrt{1 - \gamma_3 \widehat{\text{SR}} + \frac{\gamma_4 - 1}{4}\widehat{\text{SR}}^2}}\right)$$
Where $\text{SR}^*$ is the expected maximum Sharpe ratio under the null hypothesis of zero skill:
$$\text{SR}^* \approx \sqrt{2 \ln(N)} + \frac{\gamma}{\sqrt{2 \ln(N)}}$$

Configurations failing to achieve a statistically significant DSR ($p < 0.05$) are discarded.

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
    "mean_is_sortino": "2.45",
    "mean_oos_sortino": "2.10",
    "stability_score": "0.92",
    "fitness": "2.25",
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
```

1. **Purged Walk-Forward Folds**: Dynamically constructs rolling train/embargo/test folds across the in-memory Dollar Bar series.
2. **Rayon Thread Pool**: Evaluates candidate parameter combinations concurrently across all physical CPU cores.
3. **Artifact Serialization**: Persists the winning parameter set to `configs/hpo_<symbol>_<year>.json` and complete audit metrics to `storage/reports/hpo_run_<timestamp>.json`.
4. **Instant Backtest Handshake**: The downstream Backtest and Monte Carlo stage ingests the winning configuration artifact immediately with zero manual intervention.


