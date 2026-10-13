# ATSNT Quantitative Trading Engine - Google Looker Studio BI Specification

Este documento define la especificación técnica completa y paso a paso para construir desde cero el cuadro de mando analítico de **ATSNT** en **Google Looker Studio**, consumiendo directamente las vistas analíticas de **Google BigQuery** (`scripts/gcp/bigquery_schema.sql`).

El informe se organiza en **3 páginas temáticas** optimizadas para eliminar duplicidades, unificar el flujo cuantitativo y evitar cualquier desarrollo o modificación de tablas en BigQuery:
1. **Página 1: Explorador & Comparador de Runs** — *"¿Qué he ejecutado y cuál es el mejor candidato?"*
2. **Página 2: Radiografía del Run (Backtest / Paper / Live)** — *"¿Cómo funciona este run en detalle y cómo gestiona el riesgo?"*
3. **Página 3: Robustez Cuantitativa (WFO & Monte Carlo)** — *"¿El resultado es una ventaja estadística real o es sobreajuste / azar?"*

---

## 1. Fuentes de Datos (Google BigQuery)

- **Proyecto GCP**: `${GCP_PROJECT_ID}`
- **Región**: `europe-southwest1` (Madrid)
- **Dataset**: `atsnt_bi`
- **Conector**: BigQuery nativo (Direct Query).

Looker Studio se conecta exclusivamente a **vistas deduplicadas (`v_*`)**, nunca a tablas físicas directas:

| Fuente en Looker | Vista BigQuery | Grano / Nivel de detalle | Uso principal en el informe |
| :--- | :--- | :--- | :--- |
| `Runs` | `v_runs` | 1 fila por run (último lifecycle + métricas `full` + spec aplanada) | Página 1 (Catálogo y comparador) y Página 2 (Scorecards del run) |
| `Trades` | `v_trades` | 1 fila por trade cerrado (con MAE, MFE y drawdown propio) | Página 2 (Anatomía de salidas, MAE/MFE y bitácora de órdenes) |
| `Equity` | `v_equity` | Puntos de equity por evento (`entry`, `trough`, `peak`, `exit`) | Página 2 (Curva de equity y gráfico underwater) |
| `Latest Equity` | `v_latest_equity`| Último punto de equity registrado por run activo | Página 1 (Monitor de sesiones activas) |
| `HPO Trials` | `v_hpo_trials` | 1 fila por trial de optimización (hiperparámetros JSON + ranking) | Página 3 (Superficie de parámetros, ranking y dispersión IS/OOS) |
| `HPO Folds` | `v_hpo_folds` | 1 fila por trial, fold (`0..N`) y muestra (`is` / `oos`) | Página 3 (Barras de degradación temporal por fold) |
| `Monte Carlo` | `v_monte_carlo`| 1 fila por simulación CBB (percentiles P50, P95, P99 y ruina) | Página 3 (Scorecards de estrés y riesgo de ruina) |
| `Series` | `v_series` | Puntos de serie temporal u ordinal (`series`, `event`, `x`, `value`)| Página 3 (Fan chart de trayectorias de Monte Carlo) |

> [!NOTE]
> Todas las fuentes comparten la clave primaria `run_id` (o `parent_run_id` en el caso de HPO y Monte Carlo). Esto garantiza que los filtros globales afecten a todas las fuentes de manera coherente sin necesidad de uniones complejas en Looker Studio.

---

## 2. Configuración Global del Informe y Cuota Free Tier

### 2.1 Preservación de Coste Cero (Always Free Tier 1 TB/mes)
Para asegurar que el dashboard cumpla la premisa de **0,00 € / mes** de la arquitectura ATSNT sin agotar la cuota de consultas analíticas de BigQuery:
1. En cada fuente de datos: **Recurso** → **Gestionar las fuentes de datos añadidas** → **Editar** en cada vista.
2. Pulsa en **Frescura de datos** (*Data freshness*) arriba a la derecha.
3. Configúralo en **1 hora** o **4 horas** (en lugar de los 15 minutos por defecto).
4. El informe cuenta con el botón manual **Actualizar datos** para cuando se desee forzar la lectura tras finalizar un backtest.

### 2.2 Dimensiones del Lienzo (*Canvas*)
- **Páginas 1 y 2**: Anchura `1360 px`, Altura `1080 px` (o formato estándar panorámico 16:9).
- **Página 3**: Anchura `1360 px`, Altura `1600 px` (para permitir scroll vertical cómodo entre el bloque de Walk-Forward y el bloque de Monte Carlo).

---

## 3. Campos Calculados en Looker Studio

Para no modificar ninguna vista ni tabla en BigQuery, se crean los siguientes campos calculados directamente en las fuentes de datos de Looker Studio:

### 3.1 En la fuente `Runs` (`v_runs`):
1. **`run_label`** (Etiqueta legible para selectores y gráficos de barras):
   - Tipo: Texto
   - Fórmula:
     ```text
     CONCAT(run_type, " · ", strategy_id, " · ", FORMAT_DATETIME("%Y-%m-%d %H:%M", started_at))
     ```
2. **`holding_minutes`** (Duración media en minutos):
   - Tipo: Numérico → Número
   - Fórmula:
     ```text
     avg_holding_seconds / 60
     ```

### 3.2 En la fuente `Trades` (`v_trades`):
1. **`mae_pct`** (Máxima Excursión Adversa como fracción):
   - Tipo: Numérico → Porcentaje
   - Fórmula:
     ```text
     (entry_equity - trough_equity) / entry_equity
     ```
2. **`mfe_pct`** (Máxima Excursión Favorable como fracción):
   - Tipo: Numérico → Porcentaje
   - Fórmula:
     ```text
     (peak_equity - entry_equity) / entry_equity
     ```

---

## 4. Controles Globales (A Nivel de Informe)

Crea estos 4 controles en la cabecera superior. Selecciónalos juntos, haz clic derecho y elige **Pasar al nivel de informe** para que aparezcan sincronizados en todas las páginas:

1. **Tipo de Run**:
   - Tipo de control: **Lista desplegable** (*Drop-down list*). Nunca usar *Casilla*.
   - Fuente: `v_runs` | Campo de control: `run_type` (`backtest`, `hpo`, `paper`, `live`, etc.).
2. **Estrategia**:
   - Tipo de control: **Lista desplegable**.
   - Fuente: `v_runs` | Campo de control: `strategy_id`.
3. **Símbolo**:
   - Tipo de control: **Lista desplegable**.
   - Fuente: `v_runs` | Campo de control: `symbol`.
4. **Intervalo Temporal**:
   - Tipo de control: **Control de intervalo de fechas**.
   - Campo de fecha: `started_at`.

---

## 5. Página 1: Explorador & Comparador de Runs

> **Pregunta que responde:** *"¿Qué he ejecutado en el sistema y qué estrategias o variantes destacan para analizar en detalle?"*

```text
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ [CABECERA GLOBAL]: Filtros a nivel de informe (Tipo Run | Estrategia | Símbolo | Fecha)│
├────────────────────────────────────────────────────────────────────────────────────────┤
│ [FILA 1: KPIs GLOBALES]                                                                │
│ ┌──────────────────┐ ┌──────────────────┐ ┌──────────────────┐ ┌─────────────────────┐ │
│ │ Total Runs       │ │ Backtests Éxito  │ │ En Ejecución     │ │ Máx Sortino         │ │
│ └──────────────────┘ └──────────────────┘ └──────────────────┘ └─────────────────────┘ │
├────────────────────────────────────────────────────────────────────────────────────────┤
│ [FILA 2: CATÁLOGO DE RUNS] (Tabla interactiva con Filtrado Cruzado)                    │
│ started_at | run_type | status | strategy | symbol | Sortino | PF | Net PnL | MaxDD%   │
├────────────────────────────────────────────────────────────────────────────────────────┤
│ [FILA 3: COMPARADOR LADO A LADO] (Activado al pulsar filas del catálogo)               │
│ ┌────────────────────────────────────────┐ ┌─────────────────────────────────────────┐ │
│ │ Gráfico Barras: Sortino, PF y MaxDD %  │ │ Tabla Specs: Capital, Umbral, Comisiones│ │
│ └────────────────────────────────────────┘ └─────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

### 5.1 Fila 1: Scorecards Ejecutivos (Fuente `v_runs`)
- **Tarjeta 1 (Total Runs)**: Métrica `run_id` | Agregación: `Recuento único` (`COUNT_DISTINCT`).
- **Tarjeta 2 (Backtests Éxito)**: Métrica `run_id` (`COUNT_DISTINCT`) con filtro de tarjeta: `run_type = 'backtest'` Y `net_profit > 0`.
- **Tarjeta 3 (En Ejecución)**: Métrica `run_id` (`COUNT_DISTINCT`) con filtro de tarjeta: `status = 'running'`.
- **Tarjeta 4 (Máx Sortino)**: Métrica `sortino_ratio` | Agregación: `MAX`.

### 5.2 Fila 2: Tabla Principal del Catálogo de Runs (Fuente `v_runs`)
- **Dimensiones**: `started_at` (formato Fecha Hora), `run_type`, `status`, `strategy_id`, `symbol`, `run_id`.
- **Métricas**:
  - `sortino_ratio` (`AVG` | 2 decimales)
  - `profit_factor` (`AVG` | 2 decimales)
  - `net_profit` (`SUM` | Moneda)
  - `max_drawdown_pct` (`MAX` | Porcentaje)
  - `win_rate` (`AVG` | Porcentaje)
  - `total_trades` (`SUM` | Número entero)
  - `fee_drag_pct` (`AVG` | Porcentaje)
- **Ordenación**: `started_at` descendente.
- **Paginación**: 20 filas por página.
- **Interacción**: Marcar casilla **Filtrado cruzado** (*Cross-filtering*).
- **Formato condicional**: `net_profit < 0` fondo rojo suave; `net_profit > 0` fondo verde suave; `status = failed` texto rojo negrita.

### 5.3 Fila 3: Bloque Comparador Inferior (Fuente `v_runs`)
1. **Gráfico de Barras Agrupadas (Izquierda, 50% ancho)**:
   - Dimensión: `run_label` (o `run_id`).
   - Métricas: `sortino_ratio` (`AVG`), `profit_factor` (`AVG`), `max_drawdown_pct` (`MAX`).
   - Estilo: Mostrar etiquetas de datos activado.
2. **Tabla de Especificaciones de Ejecución (Derecha, 50% ancho)**:
   - Dimensión: `run_label`.
   - Métricas: `initial_capital` (`MAX` | Moneda), `dollar_bar_threshold` (`MAX` | Número), `taker_fee_pct` (`MAX` | Porcentaje), `slippage_pct` (`MAX` | Porcentaje), `duration_seconds` (`MAX` | Segundos).

---

## 6. Página 2: Radiografía del Run (Backtest / Paper / Live)

> **Pregunta que responde:** *"¿Por qué gana o pierde dinero este run concreto y cómo gestiona el riesgo y las comisiones barra a barra?"*

```text
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ [CONTROL OBLIGATORIO]: Selector de run_id / run_label con Selección Única Activada    │
├────────────────────────────────────────────────────────────────────────────────────────┤
│ [FILA 1: MATRIZ DE SCORECARDS]                                                         │
│  BLOQUE A: Edge & Retorno   │  BLOQUE B: Riesgo & Drawdown │  BLOQUE C: Fricción & Tpo │
│  Net PnL | PF | WinRate     │  Sortino | MaxDD% | MaxDD $  │  Fees | Drag % | Duración │
├────────────────────────────────────────────────────────────────────────────────────────┤
│ [FILA 2: CURVA DE EQUITY & UNDERWATER] (Alineados en el eje temporal 'ts')             │
│ ┌────────────────────────────────────────────────────────────────────────────────────┐ │
│ │ Serie Temporal: Equity (azul continua) vs Running Peak (gris punteada)             │ │
│ ├────────────────────────────────────────────────────────────────────────────────────┤ │
│ │ Gráfico Área: Drawdown % (área roja, eje invertido con 0% arriba)                  │ │
│ └────────────────────────────────────────────────────────────────────────────────────┘ │
├────────────────────────────────────────────────────────────────────────────────────────┤
│ [FILA 3: TRADE INTELLIGENCE]                                                           │
│ ┌─────────────────────────┐ ┌───────────────────────────┐ ┌──────────────────────────┐│
│ │ Donut: Salidas          │ │ Dispersión MAE vs PnL     │ │ Dispersión MFE vs PnL    ││
│ │ (TP vs SL vs Barreras)  │ │ (¿Stops asfixiados?)      │ │ (¿Beneficio regalado?)   ││
│ └─────────────────────────┘ └───────────────────────────┘ └──────────────────────────┘│
├────────────────────────────────────────────────────────────────────────────────────────┤
│ [FILA 4: BITÁCORA DE TRADES] (Tabla detallada paginada a 50 filas)                     │
│ exit_ts | side | exit_reason | entry_px | exit_px | fees | net_pnl | return% | max_dd% │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

### 6.1 Control Específico de Página
- **Tipo de control**: **Lista desplegable** (*Drop-down list*).
- **Fuente**: `v_runs` | **Campo de control**: `run_label` (o `run_id`).
- **Estilo**: Marcar obligatoriamente **Habilitar selección única** (evita agregaciones accidentales de runs múltiples).

### 6.2 Fila 1: Matriz de Scorecards (Fuente `v_runs`)
- **Bloque A (Edge & Rendimiento)**:
  - `net_profit` (`SUM` | Moneda)
  - `profit_factor` (`AVG` | 2 decimales)
  - `win_rate` (`AVG` | Porcentaje)
  - `payoff_ratio` (`AVG` | 2 decimales)
  - `mathematical_expectancy` (`AVG` | Moneda)
- **Bloque B (Riesgo & Capital)**:
  - `sortino_ratio` (`AVG` | 2 decimales)
  - `max_drawdown_pct` (`MAX` | Porcentaje)
  - `max_drawdown_amount` (`MAX` | Moneda)
  - `total_trades` (`SUM` | Entero)
- **Bloque C (Fricción & Realidad)**:
  - `total_fees` (`SUM` | Moneda)
  - `total_slippage` (`SUM` | Moneda)
  - `fee_drag_pct` (`AVG` | Porcentaje)
  - `holding_minutes` (`AVG` | 1 decimal)

### 6.3 Fila 2: Curva de Equity & Underwater (Fuente `v_equity`)
1. **Curva de Equity (Serie temporal superior)**:
   - Dimensión temporal: `ts`.
   - Métricas: `equity` (`AVG` | Línea sólida azul/verde) y `running_peak` (`MAX` | Línea punteada gris).
2. **Underwater / Drawdown (Gráfico de área inferior)**:
   - Dimensión temporal: `ts`.
   - Métrica: `drawdown_pct` (`MAX` | Porcentaje).
   - Estilo: Gráfico de área color rojo suave / semi-transparente.
   - En **Eje Y izquierdo**: Marcar **Invertir dirección del eje** (el 0 % queda arriba y las caídas descienden).

### 6.4 Fila 3: Trade Intelligence (Fuente `v_trades`)
1. **Donut: Anatomía de Salidas (30% ancho)**:
   - Dimensión: `exit_reason` (`TakeProfit`, `StopLoss`, `TimeBarrier`, etc.).
   - Métrica: `trade_id` (`Recuento único` / `COUNT_DISTINCT`).
2. **Dispersión MAE vs Resultado (35% ancho)**:
   - Dimensión: `trade_id`.
   - Eje X: `mae_pct` (campo calculado de máxima excursión adversa).
   - Eje Y: `net_pnl`.
   - Color / Desglose: `exit_reason`.
3. **Dispersión MFE vs Resultado (35% ancho)**:
   - Dimensión: `trade_id`.
   - Eje X: `mfe_pct` (campo calculado de máxima excursión favorable).
   - Eje Y: `net_pnl`.

### 6.5 Fila 4: Bitácora de Trades (Fuente `v_trades`)
- **Dimensiones**: `exit_timestamp`, `side`, `exit_reason`.
- **Métricas**: `entry_price` (`AVG`), `exit_price` (`AVG`), `quantity` (`SUM`), `gross_pnl` (`SUM`), `fees_paid` (`SUM`), `net_pnl` (`SUM`), `return_pct` (`AVG` | Porcentaje), `max_drawdown_pct` (`MAX` | Porcentaje).
- **Ordenación**: `exit_timestamp` descendente.
- **Paginación**: 50 filas por página (vital para evitar lentitud en navegador con miles de trades).
- **Formato condicional**: `net_pnl > 0` verde suave; `net_pnl < 0` rojo suave.

---

## 7. Página 3: Robustez Cuantitativa (WFO & Monte Carlo)

> **Pregunta que responde:** *"¿Esta estrategia tiene una ventaja estadística real fuera de muestra o fue producto de la suerte y el sobreajuste?"*

```text
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ [SECCIÓN A: WALK-FORWARD OPTIMIZATION] (Fuente: v_hpo_trials y v_hpo_folds)            │
│  [Selector HPO Run ID]                                                                 │
│  Scorecards: Rank #1 Fitness | Mean OOS Sortino | Stability Score | DSR p-value (<0.05)│
│ ┌───────────────────────────────────────┐ ┌──────────────────────────────────────────┐ │
│ │ Dispersión IS vs OOS                  │ │ Barras Degradación por Fold:             │ │
│ │ (X=IS Sortino, Y=OOS Sortino)         │ │ (Dim=fold_idx, Desglose=sample is vs oos)│ │
│ └───────────────────────────────────────┘ └──────────────────────────────────────────┘ │
│ ┌────────────────────────────────────────────────────────────────────────────────────┐ │
│ │ Tabla Ranking de Trials: rank | params | fitness | stability | is/oos Sortino | DSR│ │
│ └────────────────────────────────────────────────────────────────────────────────────┘ │
├────────────────────────────────────────────────────────────────────────────────────────┤
│ [SECCIÓN B: MONTE CARLO STRESS TESTING] (Fuente: v_monte_carlo y v_series)             │
│  Scorecards: Prob. Ruina % | MaxDD Histórico | P95 Max Drawdown | P95 Rachas Bajo Agua │
│ ┌────────────────────────────────────────────────────────────────────────────────────┐ │
│ │ Fan Chart de Trayectorias CBB (Fuente: v_series, Filtro: series = 'mc_equity')     │ │
│ │ Dimensión: x (ordinal trade) | Desglose: event (Quantiles & Realized) | Métrica: val│ │
│ └────────────────────────────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

### 7.1 SECCIÓN A: Walk-Forward Optimization (HPO)

1. **Control de Optimización**:
   - Tipo de control: **Lista desplegable** (*Drop-down list* con selección única).
   - Fuente: `v_hpo_trials` | Campo de control: `hpo_run_id`.
2. **Scorecards del Trial Ganador (#1)** (Fuente `v_hpo_trials`):
   - `fitness`: `MAX` | 2 decimales.
   - `mean_oos_sortino`: `MAX` | 2 decimales (Sortino medio fuera de muestra).
   - `stability_score`: `MAX` | 2 decimales (Penalización de picos inestables).
   - `dsr_p_value`: `MIN` | 4 decimales. Formato condicional: `< 0.05` en verde (Deflated Sharpe Ratio estadísticamente significativo).
3. **Dispersión IS vs OOS (Detector de Sobreajuste)**:
   - Gráfico: **Gráfico de dispersión** (*Scatter plot*).
   - Fuente: `v_hpo_trials` | Dimensión: `run_id`.
   - Eje X: `mean_is_sortino`.
   - Eje Y: `mean_oos_sortino`.
   - Tamaño de burbuja: `total_oos_trades`.
   - *Lectura*: Puntos muy alejados por debajo de la diagonal teórica $y=x$ denotan sobreajuste severo.
4. **Degradación por Fold**:
   - Gráfico: **Gráfico de barras de columnas agrupadas**.
   - Fuente: `v_hpo_folds`.
   - Dimensión: `fold_idx` (Fold 0, Fold 1, Fold 2...).
   - Dimensión de desglose: `sample` (`is` vs `oos`).
   - Métrica: `sortino_ratio` (`AVG`).
5. **Tabla Ranking de Trials de HPO**:
   - Fuente: `v_hpo_trials`.
   - Dimensiones: `rank`, `params`.
   - Métricas: `fitness` (`AVG`), `stability_score` (`AVG`), `mean_is_sortino` (`AVG`), `mean_oos_sortino` (`AVG`), `dsr_p_value` (`AVG`).
   - Ordenación: `rank` ascendente (#1 arriba).

---

### 7.2 SECCIÓN B: Monte Carlo Stress Testing (CBB)

1. **Scorecards de Riesgo de Ruina y Estrés** (Fuente `v_monte_carlo`):
   - `probability_of_ruin_pct`: `MAX` | Porcentaje (Objetivo: 0.00 %).
   - `historical_max_drawdown_pct`: `MAX` | Porcentaje (El drawdown real sufrido en el backtest).
   - `p95_max_drawdown_pct`: `MAX` | Porcentaje (El 95% de los caminos simulados no superan esta pérdida).
   - `p95_underwater_trades`: `MAX` | Número entero (Racha consecutiva de trades en pérdidas bajo el percentil 95).
2. **Fan Chart de Trayectorias de Monte Carlo**:
   - Gráfico: **Gráfico de serie temporal** o **Gráfico de líneas**.
   - Fuente: `v_series`.
   - **Dimensión**: `x` (índice ordinal de la operación simulada: trade 1, trade 2, trade 100...).
   - **Dimensión de desglose**: `event` (genera las curvas: `Realized`, `Worst_Stress_1`, `Best_Ceiling_1`, `Quantile_0.05`, `Quantile_0.50`, `Quantile_0.95`).
   - **Métrica**: `value` (`AVG` | Moneda / Capital simulado).
   - **Filtro del gráfico (OBLIGATORIO)**:
     - Crear filtro a nivel de este gráfico: **Incluir** → `series` → **Igual a (=)** → `mc_equity`.
   - **Estilo**: Línea `Realized` destacada en azul/blanco; cuantiles `Quantile_*` en tonos degradados naranjas o grises.

---

## 8. Reglas Maestras de Agregación y Formateo

1. **Agregaciones matemáticas**:
   - Magnitudes acumulativas y conteos (`net_profit`, `total_trades`, `total_fees`, `gross_pnl`): **`SUM`**.
   - Ratios y rendimientos ponderados (`sortino_ratio`, `profit_factor`, `win_rate`, `fee_drag_pct`): **`AVG`**.
   - Valores extremos de riesgo (`max_drawdown_pct`, `max_drawdown_amount`, percentiles de Monte Carlo): **`MAX`**.
   - Parámetros fijos de configuración (`initial_capital`, `dollar_bar_threshold`, comisiones): **`AVG`** o **`MAX`**.
2. **Tipo de datos en Looker Studio**:
   - Todos los campos con sufijo `*_pct` deben configurarse en su métrica como **Numérico → Porcentaje** (BigQuery los entrega en base 1, ej: `0.154` = `15.4%`).
   - Los importes monetarios deben configurarse como **Numérico → Moneda (USD / EUR)**.
3. **Paginación y Rendimiento del Navegador**:
   - La tabla del catálogo de runs debe paginarse a **20 filas**.
   - La bitácora de trades debe paginarse a **50 filas**. Nunca renderizar listas infinitas de trades directas de BigQuery para evitar bloqueos del navegador.
