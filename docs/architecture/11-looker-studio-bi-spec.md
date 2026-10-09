# ATSNT Quantitative Trading Engine - Google Looker Studio BI Specification

Este documento define la especificación técnica completa y reproducible para construir y mantener el panel analítico institucional de **ATSNT** en **Google Looker Studio**.

---

## 1. Arquitectura de Fuentes de Datos (Google BigQuery)

- **Proyecto GCP**: `mi-facturador-bot-01` (o `${GCP_PROJECT_ID}`)
- **Región**: `europe-southwest1` (Madrid)
- **Conjunto de Datos (Dataset)**: `atsnt_bi`
- **Conector**: Conector nativo de BigQuery para Looker Studio (Direct Query, sin costes de extracción periódica).

### Mapa Canónico de Tablas y Vistas

| Dominio Analítico | Objeto BigQuery | Tipo | Partición / Clúster | Utilidad en Looker Studio |
| :--- | :--- | :--- | :--- | :--- |
| **Operaciones & PnL** | `atsnt_bi.v_trades` | Vista SQL | `exit_timestamp` (partición subyacente) | Cuaderno de bitácora, PnL diario, Win Rate, comisiones. |
| **Estado Actual en Vivo** | `atsnt_bi.v_live_paper_monitor` | Vista SQL | 1 fila por sesión (`ROW_NUMBER() = 1`) | Tarjetas de saldo, PnL flotante y posición actual exacta. |
| **Curva Temporal de Capital** | `atsnt_bi.equity_snapshots` | Tabla física | `DAY(timestamp)` / `session_id` | Gráfico continuo de Equity y Curva bajo el agua (Drawdown). |
| **Optimización Walk-Forward** | `atsnt_bi.hpo_evaluations` | Tabla física | `DAY(timestamp)` / `strategy_id` | Análisis de sobreajuste, DSR, Sharpe IS vs OOS. |
| **Gestión de Riesgo & Ruina** | `atsnt_bi.monte_carlo_runs` | Tabla física | `DAY(timestamp)` / `strategy_id` | Stress-testing, percentiles P50/P95/P99, probabilidad de ruina. |

---

## 2. Estructura de Páginas del Informe

El informe institucional se compone de **4 páginas especializadas**:

```text
ATSNT Quantitative Dashboard
├── Página 1: Executive Performance & Trade Journal
├── Página 2: Live & Paper Trading Monitor
├── Página 3: Quantitative HPO & Overfitting Surface
└── Página 4: Monte Carlo Stress-Testing & Risk of Ruin
```

---

## 3. Página 1: Executive Performance & Trade Journal

*Objetivo*: Cuadro de mando ejecutivo para evaluar rentabilidad neta, fricción de comisiones y auditoría trade a trade.

### 3.1 Fuente de Datos Principal
- **Fuente**: `atsnt_bi.v_trades`

### 3.2 Controles Superiores (Filtros Globales)
1. **Filtro de Estrategia**: Control de menú desplegable → Campo: `strategy_id`.
2. **Filtro de Modo / Sesión**: Control de menú desplegable → Campo: `session_id` (permite filtrar `bt_...`, `paper_...`, `live_...`).
3. **Filtro de Símbolo**: Control de menú desplegable → Campo: `symbol` (`BTCUSDT`, etc.).
4. **Control por Periodo**: Control de calendario nativo → Campo de fecha: `exit_timestamp`.

### 3.3 Tarjetas de Resultados (Scorecards en Cabecera)
| Tarjeta | Campo / Fórmula | Agregación | Formato |
| :--- | :--- | :--- | :--- |
| **Net PnL ($)** | `net_pnl` | `SUM` | Moneda (USD) |
| **Win Rate (%)** | Campo calculado: `COUNT(CASE WHEN net_pnl > 0 THEN 1 END) / COUNT(trade_id)` | Auto | Porcentaje |
| **Total Trades** | `trade_id` | `COUNT_DISTINCT` | Número entero |
| **Total Fees ($)** | `fees_paid` | `SUM` | Moneda (USD) |
| **Avg Duration (min)** | Campo calculado: `AVG(holding_duration_seconds) / 60` | Auto | Número (1 dec.) |

### 3.4 Gráficos Principales
1. **PnL Diario (Gráfico de Columnas)**:
   - Dimensión: `exit_timestamp` (configurado como tipo `Fecha` `AAAA-MM-DD`).
   - Métrica: `net_pnl` (`SUM`).
   - Estilo: Etiquetas de datos activadas.
2. **Distribución de Salidas (Gráfico Donut / Circular)**:
   - Dimensión: `exit_reason` (`TakeProfit`, `StopLoss`, `TimeBarrier`).
   - Métrica: `trade_id` (`COUNT`).
3. **Cuaderno de Bitácora (Tabla Estándar de Operaciones)**:
   - Dimensiones: `exit_timestamp`, `symbol`, `side`, `exit_reason`.
   - Métricas: `entry_price` (`AVG`), `exit_price` (`AVG`), `quantity` (`SUM`), `gross_pnl` (`SUM`), `fees_paid` (`SUM`), `net_pnl` (`SUM`).
   - Orden: `exit_timestamp` Descendente.
   - Formato condicional: `net_pnl > 0` (verde suave), `net_pnl < 0` (rojo suave).

---

## 4. Página 2: Live & Paper Trading Monitor

*Objetivo*: Monitorización en vivo o diferido del estado de capital, curva continua de balance, drawdown y posición activa.

### 4.1 Fuentes de Datos
- **Para Scorecards de Estado Actual**: `atsnt_bi.v_live_paper_monitor` (1 fila por sesión, garantiza el último estado exacto).
- **Para Gráficos de Evolución Temporal y Tabla**: `atsnt_bi.equity_snapshots`.

### 4.2 Controles Superiores
1. **Filtro de Sesión**: Menú desplegable → Campo: `session_id`.
2. **Filtro de Símbolo**: Menú desplegable → Campo: `symbol`.
3. **Control por Periodo**: Control de calendario → Campo: `timestamp`.

### 4.3 Tarjetas de Estado en Vivo (Fuente: `v_live_paper_monitor`)
| Tarjeta | Campo | Agregación | Formato | Significado |
| :--- | :--- | :--- | :--- | :--- |
| **Portfolio Equity** | `total_equity` | `MAX` / `AVG` | Moneda (USD) | Saldo total actual (Efectivo + PnL flotante). |
| **Cash Balance** | `cash_equity` | `MAX` / `AVG` | Moneda (USD) | Capital líquido no comprometido en margen. |
| **Unrealized P&L** | `unrealized_pnl` | `MAX` / `AVG` | Moneda (USD) | Ganancia/pérdida de la posición abierta actual. |
| **Current Drawdown** | `drawdown_pct` | `MAX` / `AVG` | Porcentaje | Caída actual desde el pico histórico de la sesión. |

### 4.4 Gráficos Principales (Fuente: `equity_snapshots`)
1. **Curva Continua de Capital (Gráfico de Series Temporales)**:
   - Dimensión: `timestamp` (Fecha y hora).
   - Métricas: `total_equity` (Línea principal, grosor 2), `cash_equity` (Línea secundaria).
2. **Curva Bajo el Agua / Underwater (Gráfico de Series Temporales o Área)**:
   - Dimensión: `timestamp`.
   - Métrica: `drawdown_pct`.
   - Estilo: Color rojo o naranja oscuro.
3. **Historial de Posición & Balance (Tabla)**:
   - Dimensiones: `timestamp`, `symbol`, `active_position_side`.
   - Métricas: `active_position_qty` (`AVG`), `unrealized_pnl` (`AVG`), `total_equity` (`AVG`), `drawdown_pct` (`AVG`).
   - Orden: `timestamp` Descendente.

---

## 5. Página 3: Quantitative HPO & Overfitting Surface

*Objetivo*: Auditoría rigurosa de optimización Walk-Forward, detección de minería de datos (*data snooping*) y análisis de estabilidad de parámetros.

### 5.1 Fuente de Datos
- **Fuente**: `atsnt_bi.hpo_evaluations`

### 5.2 Controles Superiores
1. **Filtro de Estrategia**: Menú desplegable → Campo: `strategy_id`.
2. **Filtro de Optimización**: Menú desplegable → Campo: `run_id`.

### 5.3 Tarjetas de Robustez Estadística (Scorecards)
| Tarjeta | Campo | Agregación | Formato | Significado |
| :--- | :--- | :--- | :--- | :--- |
| **Best OOS Sharpe** | `oos_sharpe` | `MAX` | Número (2 dec.) | Sharpe en periodos de prueba fuera de muestra. |
| **Peak DSR (Anti-Overfitting)** | `deflated_sharpe_ratio` | `MAX` | Porcentaje | Probabilidad de significancia estadística (>95% recomendado). |
| **Max Stability Plateau** | `parameter_stability_score` | `MAX` | Número (2 dec.) | Robustez ante variaciones en hiperparámetros vecinos. |
| **Worst-Case Drawdown** | `max_drawdown_pct` | `MAX` | Porcentaje | Caída máxima observada en la optimización. |

### 5.4 Gráficos Principales
1. **In-Sample vs Out-of-Sample Sharpe (Gráfico de Dispersión / Scatter Plot)**:
   - Dimensión: `parameter_space` (o `run_id`).
   - Eje X: `is_sharpe` (`AVG`).
   - Eje Y: `oos_sharpe` (`AVG`).
   - Tamaño de burbuja: `total_trades` (`SUM` o `AVG`).
   - *Interpretación*: Puntos cercanos a la diagonal X ≈ Y demuestran generalización; puntos con alto IS y bajo OOS revelan sobreajuste.
2. **DSR vs Win Rate (Gráfico de Dispersión)**:
   - Dimensión: `parameter_space`.
   - Eje X: `win_rate` (`AVG`).
   - Eje Y: `deflated_sharpe_ratio` (`AVG`).
3. **Ranking de Parámetros Ganadores (Tabla)**:
   - Dimensiones: `parameter_space`, `strategy_id`.
   - Métricas: `oos_sharpe` (`AVG`), `deflated_sharpe_ratio` (`AVG`), `parameter_stability_score` (`AVG`), `win_rate` (`AVG`), `max_drawdown_pct` (`AVG`), `total_trades` (`SUM`).
   - Orden: `deflated_sharpe_ratio` Descendente.

---

## 6. Página 4: Monte Carlo Stress-Testing & Risk of Ruin

*Objetivo*: Evaluación estocástica de riesgo extremo mediante bootstrapping por bloques circulares, modelando escenarios de colapso de cuenta y cisnes negros.

### 6.1 Fuente de Datos
- **Fuente**: `atsnt_bi.monte_carlo_runs`

### 6.2 Controles Superiores
1. **Filtro de Estrategia**: Menú desplegable → Campo: `strategy_id`.
2. **Filtro de Ejecución (Run ID)**: Menú desplegable → Campo: `run_id`.
3. **Filtro de Método de Remuestreo**: Menú desplegable → Campo: `resample_method` (`CircularBlockBootstrap` / `IID`).

### 6.3 Tarjetas de Riesgo Extremo (Scorecards)
| Tarjeta | Campo | Agregación | Formato | Significado |
| :--- | :--- | :--- | :--- | :--- |
| **Probability of Ruin (%)** | `probability_of_ruin_pct` | `MAX` / `AVG` | Porcentaje | Probabilidad de que la cuenta pierda el 100% del capital. |
| **Historical Max Drawdown** | `historical_max_drawdown` | `MAX` | Porcentaje | Caída máxima registrada en la serie temporal original. |
| **P50 Expected Drawdown** | `p50_max_drawdown` | `AVG` | Porcentaje | Caída mediana esperada en los miles de escenarios simulados. |
| **P95 Severe Drawdown** | `p95_max_drawdown` | `MAX` / `AVG` | Porcentaje | Caída en el percentil 95 (escenario muy adverso). |
| **P99 Extreme Drawdown** | `p99_max_drawdown` | `MAX` / `AVG` | Porcentaje | Caída en el percentil 99 (escenario de cisne negro). |

### 6.4 Gráficos Principales
1. **Perfil de Drawdown por Percentiles (Gráfico de Barras / Columnas)**:
   - Dimensión: `strategy_id` (o `run_id`).
   - Métricas:
     - `historical_max_drawdown` (`AVG`)
     - `p50_max_drawdown` (`AVG`)
     - `p95_max_drawdown` (`AVG`)
     - `p99_max_drawdown` (`AVG`)
   - Estilo: Barras agrupadas. Permite comparar visualmente cómo escala el riesgo desde el caso histórico hasta el P99.
2. **Comparativa de Riesgo por Método de Remuestreo (Gráfico de Barras)**:
   - Dimensión: `resample_method` (`CircularBlockBootstrap` vs `IID`).
   - Métrica: `probability_of_ruin_pct` (`MAX` o `AVG`).
   - *Nota Cuantitativa*: `CircularBlockBootstrap` preserva la autocorrelación y agrupamiento de volatilidad (*volatility clustering*), ofreciendo una medida de ruina mucho más realista que IID.
3. **Auditoría de Simulaciones Monte Carlo (Tabla)**:
   - Dimensiones: `run_id`, `strategy_id`, `resample_method`.
   - Métricas: `iterations` (`MAX`), `historical_max_drawdown` (`AVG`), `p50_max_drawdown` (`AVG`), `p95_max_drawdown` (`AVG`), `p99_max_drawdown` (`AVG`), `probability_of_ruin_pct` (`MAX`).
   - Orden: `probability_of_ruin_pct` Descendente.
   - Formato condicional: Si `probability_of_ruin_pct > 0`, resaltar celda en rojo de alerta.

---

## 7. Reglas Generales de Formateo y Buenas Prácticas

1. **Ratios vs Acumuladores**:
   - Monedas y conteos (`net_pnl`, `fees_paid`, `quantity`, `total_trades`): Siempre agregación **`SUM`**.
   - Ratios, porcentajes y precios (`is_sharpe`, `oos_sharpe`, `deflated_sharpe_ratio`, `drawdown_pct`, `price`): Siempre agregación **`AVG`** o **`MAX`**.
2. **Sincronización de Filtros**:
   - En Looker Studio, los controles superiores se aplican automáticamente a todos los gráficos de la página que compartan la misma fuente de datos.
3. **Actualización Automática**:
   - Al usar BigQuery nativo, cada vez que un bot en GCP hace streaming de un nuevo trade o snapshot, refrescar el informe en el navegador (`Ctrl + R` o botón Actualizar datos) renderiza inmediatamente las nuevas métricas sin desfase.
