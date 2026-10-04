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

## 4. Configuration Persistence & Strategy Loading

### 4.1 Output Schema (`configs/hpo_results.json`)
The HPO engine produces a structured JSON artifact summarizing the optimization run:

```json
{
  "timestamp": 1700000000000,
  "dataset": "BTCUSDT_2024_Q1_aggTrades",
  "trials_evaluated": 500,
  "best_configuration": {
    "rolling_window_len": 24,
    "cusum_vol_multiplier": "2.10",
    "z_entry_threshold": "1.85",
    "z_stop_threshold": "3.40",
    "time_barrier_bars": 12
  },
  "in_sample_metrics": {
    "sortino_ratio": "2.45",
    "profit_factor": "1.82",
    "expectancy": "42.50"
  },
  "out_of_sample_metrics": {
    "sortino_ratio": "2.10",
    "profit_factor": "1.65",
    "expectancy": "36.20"
  },
  "statistical_validation": {
    "deflated_sharpe_ratio": "0.985",
    "is_statistically_significant": true,
    "stability_score": "0.92"
  }
}
```

### 4.2 Seamless Strategy Loading
Because `DollarBarsCusumConfig` implements `serde::Deserialize`, the strategy can be initialized directly from the verified HPO artifact:

```rust
let config = DollarBarsCusumConfig::from_file("configs/hpo_results.json")?;
let strategy = DollarBarsCusumStrategy::new(config)?;
```
