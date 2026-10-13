# ATSNT Quantitative Trading Engine - Google Looker Studio BI Specification

Este documento define cómo construir el informe analítico de **ATSNT** en **Google Looker Studio** sobre el modelo de datos centrado en runs (`scripts/gcp/bigquery_schema.sql`). El informe se organiza **por preguntas, no por tablas**: todas las páginas comparten la clave `run_id`, de modo que un mismo control filtra el informe completo y cualquier estrategia nueva aparece sin tocar el dashboard.

---

## 1. Fuentes de Datos (Google BigQuery)

- **Proyecto GCP**: `${GCP_PROJECT_ID}`
- **Región**: `europe-southwest1` (Madrid)
- **Dataset**: `atsnt_bi`
- **Conector**: BigQuery nativo (Direct Query). Los datos se actualizan con el botón **Actualizar datos** del informe; no hace falta streaming continuo.

Looker Studio lee **solo vistas**, nunca las tablas físicas:

| Fuente Looker | Vista BigQuery | Grano | Uso |
| :--- | :--- | :--- | :--- |
| `Runs` | `v_runs` | 1 fila por run (último estado + métricas `full` + spec aplanada) | Explorador, scorecards, comparador |
| `Trades` | `v_trades` | 1 fila por trade cerrado (con MAE/MFE) | Anatomía de salidas, bitácora |
| `Equity` | `v_equity` | Puntos de equity por evento (entry/trough/peak/exit) con drawdown | Curva de equity y underwater |
| `Latest Equity` | `v_latest_equity` | Último punto de equity por run | Monitor paper/live |
| `HPO Trials` | `v_hpo_trials` | 1 fila por trial de HPO (params + scores) | Superficie de parámetros, sobreajuste |
| `HPO Folds` | `v_hpo_folds` | 1 fila por trial, fold y muestra (`is`/`oos`) | Degradación IS → OOS |
| `Monte Carlo` | `v_monte_carlo` | 1 fila por run de Monte Carlo (percentiles aplanados) | Riesgo de ruina |
| `Series` | `v_series` | Puntos de serie genéricos (`mc_equity`) | Fan chart de Monte Carlo |

> El max drawdown exacto de un run es `v_runs.max_drawdown_pct` (calculado por el motor barra a barra). `v_equity.drawdown_pct` se deriva de los puntos por evento y sirve para dibujar la curva.

---

## 2. Controles Globales (a nivel de informe)

Crea estos controles en **Ver → Elementos a nivel de informe** para que se apliquen a todas las páginas. Looker Studio filtra entre fuentes distintas cuando el campo tiene el mismo nombre y tipo. Todas las vistas exponen `run_id` (el filtro que une el informe); `strategy_id` y `symbol` solo filtran las fuentes que los incluyen (`Runs`, `Trades`, `Latest Equity`, `HPO Trials`, `Monte Carlo`).

1. **Tipo de run**: lista desplegable → `run_type` (`backtest`, `hpo`, `hpo_trial`, `monte_carlo`, `paper`, `live`).
2. **Estrategia**: lista desplegable → `strategy_id`.
3. **Símbolo**: lista desplegable → `symbol`.
4. **Run**: lista desplegable con búsqueda → `run_id` (fuente `Runs`; dimensión de etiqueta recomendada: campo calculado `CONCAT(run_type, " · ", strategy_id, " · ", FORMAT_DATETIME("%Y-%m-%d %H:%M", started_at))`).
5. **Periodo**: control de intervalo de fechas → `started_at`.

---

## 3. Página 1: Explorador de Runs — "¿Qué he ejecutado?"

*Fuente*: `Runs`.

### 3.1 Tabla principal
- Dimensiones: `started_at`, `run_type`, `status`, `strategy_id`, `symbol`, `run_id`, `parent_run_id`, `data_source`, `git_sha`.
- Métricas: `total_trades`, `net_profit`, `profit_factor`, `sortino_ratio`, `max_drawdown_pct`, `fee_drag_pct`, `win_rate`, `duration_seconds`.
- Orden: `started_at` descendente.
- **Interacciones → Filtrado cruzado** activado: al hacer clic en una fila se filtra el resto de la página por ese run.
- Formato condicional: `net_profit < 0` rojo suave; `status = failed` en negrita.

### 3.2 Scorecards de contexto
| Tarjeta | Campo | Agregación |
| :--- | :--- | :--- |
| Runs | `run_id` | `COUNT_DISTINCT` |
| Backtests | `run_id` con filtro `run_type = backtest` | `COUNT_DISTINCT` |
| Sesiones paper/live activas | `run_id` con filtro `status = running` | `COUNT_DISTINCT` |

---

## 4. Página 2: Radiografía de un Run — "¿Cómo es esta estrategia?"

*Uso*: selecciona un único `run_id` (backtest, paper o live) en el control global.

### 4.1 Scorecards (fuente `Runs`)
| Bloque | Tarjetas |
| :--- | :--- |
| Edge | `mathematical_expectancy`, `profit_factor`, `win_rate`, `payoff_ratio` |
| Calidad del retorno | `sortino_ratio`, `max_drawdown_pct`, `max_drawdown_amount`, `net_profit` |
| Fricción | `total_fees`, `total_slippage`, `fee_drag_pct` |
| Ciclo de vida | `avg_holding_seconds` (÷ 60 → minutos), `avg_mae_pct`, `avg_mfe_pct`, `total_trades` |
| Spec usada | `dollar_bar_threshold`, `taker_fee_pct`, `maker_fee_pct`, `slippage_pct`, `initial_capital` |

### 4.2 Gráficos
1. **Curva de equity** (fuente `Equity`, serie temporal): dimensión `ts`; métricas `equity` y `running_peak`.
2. **Underwater** (fuente `Equity`, área): dimensión `ts`; métrica `drawdown_pct` (invertida en el eje).
3. **Anatomía de salidas** (fuente `Trades`, donut): dimensión `exit_reason`; métrica `trade_id` (`COUNT`).
4. **MAE vs resultado** (fuente `Trades`, dispersión): X = campo calculado `(entry_equity - trough_equity) / entry_equity`; Y = `net_pnl`; color = `exit_reason`. Muestra si los stops están demasiado cerca o si los ganadores sufren antes de ganar.
5. **MFE vs resultado** (fuente `Trades`, dispersión): X = `(peak_equity - entry_equity) / entry_equity`; Y = `net_pnl`. Muestra beneficio dejado sobre la mesa.
6. **Bitácora** (fuente `Trades`, tabla): `exit_timestamp`, `side`, `exit_reason`, `entry_price`, `exit_price`, `quantity`, `gross_pnl`, `fees_paid`, `net_pnl`, `holding_duration_seconds`; orden `exit_timestamp` descendente.

---

## 5. Página 3: Comparador — "¿Cuál es mejor y por qué?"

### 5.1 Runs lado a lado (fuente `Runs`)
- Tabla con `run_id`, `strategy_id`, `run_type` y las métricas de la sección 4.1; selecciona varios runs en el control global para compararlos.
- Gráfico de barras agrupadas: dimensión `run_id`; métricas `sortino_ratio`, `profit_factor`, `max_drawdown_pct`.

### 5.2 Superficie de parámetros HPO (fuente `HPO Trials`)
Los parámetros se guardan como JSON (`params`), así que se extraen con campos calculados de texto, válidos para cualquier estrategia:

```text
param_z_entry   = CAST(REGEXP_EXTRACT(params, '"z_entry_threshold":"?(-?[0-9.]+)') AS NUMBER)
param_window    = CAST(REGEXP_EXTRACT(params, '"rolling_window_len":"?(-?[0-9.]+)') AS NUMBER)
```

1. **Dispersión parámetro vs OOS**: X = `param_z_entry`; Y = `mean_oos_sortino`; tamaño = `total_oos_trades`; color = `is_statistically_significant`.
2. **Mapa de calor**: filas `param_window`, columnas `param_z_entry`, métrica `fitness` (`AVG`). Las mesetas estables son preferibles a los picos aislados.
3. **Ranking**: tabla con `rank`, `params`, `fitness`, `stability_score`, `mean_is_sortino`, `mean_oos_sortino`, `dsr_p_value`; orden `rank` ascendente. Control de filtro por `hpo_run_id`.

---

## 6. Página 4: Robustez — "¿Es real o es suerte?"

### 6.1 Sobreajuste IS vs OOS
1. **Dispersión IS vs OOS** (fuente `HPO Trials`): X = `mean_is_sortino`; Y = `mean_oos_sortino`; línea de referencia `y = x`. Los puntos muy por debajo de la diagonal están sobreajustados.
2. **Degradación por fold** (fuente `HPO Folds`): dimensión `fold_idx`; desglose `sample`; métricas `sortino_ratio` y `profit_factor` (`AVG`); filtro por `run_id` del trial.
3. **Scorecard DSR**: `dsr_p_value` del trial ganador (`rank = 1`) con formato condicional `< 0.05` verde.

### 6.2 Monte Carlo (fuentes `Monte Carlo` y `Series`)
1. **Scorecards**: `probability_of_ruin_pct`, `historical_max_drawdown_pct`, `p50_max_drawdown_pct`, `p95_max_drawdown_pct`, `p99_max_drawdown_pct`, `p95_underwater_trades`, `iterations`, `resample_method`.
2. **Fan chart** (fuente `Series`, gráfico de líneas): filtro `series = mc_equity`; dimensión `x` (número de trade); desglose `event` (`Realized`, `Worst_Stress_1`, `Best_Ceiling_1`, `Quantile_0.05` … `Quantile_0.95`); métrica `value`.
3. Para ir del backtest a su Monte Carlo usa `backtest_run_id` = `run_id` del backtest seleccionado.

---

## 7. Página 5: Monitor Paper / Live — "¿Cómo va ahora?"

*Uso*: control `run_type` en `paper` o `live`.

1. **Estado actual** (fuente `Latest Equity`): tabla con `run_id`, `status`, `strategy_id`, `symbol`, `ts`, `equity`, `drawdown_pct`.
2. **Curva de la sesión** (fuente `Equity`): serie temporal `ts` / `equity` del run seleccionado.
3. **Últimos trades** (fuente `Trades`): tabla ordenada por `exit_timestamp` descendente, limitada a 20 filas.
4. **Scorecards** (fuente `Runs`): `net_profit`, `total_trades`, `win_rate`, `max_drawdown_pct` (se rellenan al cerrar la sesión, cuando se escribe la fila final del run).

> La equity solo se registra cuando hay flujo de dinero (entrada, valle, pico y salida de cada trade). Entre trades la cuenta es plana, así que no hay snapshots por tiempo.

---

## 8. Reglas Generales de Formateo

1. **Agregaciones**: importes y conteos (`net_pnl`, `fees_paid`, `total_trades`) con `SUM`; ratios y porcentajes (`sortino_ratio`, `win_rate`, `max_drawdown_pct`, `fee_drag_pct`) con `AVG` o `MAX`, nunca `SUM`.
2. **Un run por gráfico de radiografía**: las páginas 2 y 5 asumen un único `run_id` seleccionado; sin selección, los scorecards agregan todos los runs filtrados.
3. **Porcentajes**: los campos `*_pct` están en fracción (0.05 = 5 %); configúralos como tipo **Porcentaje**.
4. **Estrategias nuevas**: no requieren cambios; solo añade campos calculados `REGEXP_EXTRACT` para sus parámetros en la página 3.
