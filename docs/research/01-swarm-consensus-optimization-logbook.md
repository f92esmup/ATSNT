# Bitácora de Investigación y Optimización: Swarm Consensus Strategy

**Estrategia**: `SwarmConsensusStrategy` (Estrategia 03)  
**Activo**: `BTCUSDT` (Perpetual Futures USD-M)  
**Dataset de Evaluación**: Ticks reales Binance 2026 (Febrero – Octubre)  
**Dataset de Calibración In-Sample**: Enero 2026 (44.7M trades)  
**Infraestructura de Ejecución**: Google Cloud Batch (VMs Spot `n2-custom-30-30720`, 30 vCPUs, 30 GB RAM en `europe-southwest1` Madrid)  
**Restricciones de Calidad**: Aritmética decimal de coma fija (`rust_decimal::Decimal`), 0 floats, comisiones taker 0.05%, slippage 0.05%, ejecución tick-by-tick sin simplificaciones de juguete.

---

## 1. Tabla Comparativa de Ejecuciones en Google Cloud Batch

| Dimensión / Métrica | Run 1 (Línea Base) | Run 2 (Perfil Intradía) | Run 3 (Histéresis $H \ge L_1$) | Run 4 (Alta Convicción Entrada) |
| :--- | :--- | :--- | :--- | :--- |
| **Job ID Batch** | `hpo-...-110341` | `hpo-...-135738` | `hpo-...-145442` | `hpo-...-Pendiente` |
| **Fecha de Ejecución** | 2026-10-21 11:03 UTC | 2026-10-21 13:57 UTC | 2026-10-21 14:54 UTC | 2026-10-21 (En curso) |
| **Duración Pipeline** | 1,677 s (~28 min) | 1,607 s (~26 min) | 1,826 s (~30 min) | — |
| **Candidatos HPO** | 500 | 500 | 500 | **250** |
| **Horizontes Feeds** | 5m, 1h, 4h, 1D | **15m, 30m, 1h, 2h** | **15m, 30m, 1h, 2h** | **15m, 30m, 1h, 2h** |
| **Warm-Up Requerido** | 64 días | **5.3 días** | **5.3 días** | **5.3 días** |
| **Consenso Base ($L_1$)** | `0.25` | `0.55` | `0.30` | **$\ge 0.40$ (Rango 0.40–0.65)** |
| **Histéresis ($H$)** | `0.25` | `0.125` | **`0.55` ($H > L_1$)** | **`0.15 – 1.00`** |
| **Take-Profit (TP)** | `None` (inexistente) | `Some(4.0 ATR)` | **`Some(2.0 ATR)`** | **`Some(1.25 – 4.0 ATR)`** |
| **Stop-Loss (SL)** | `3.0 ATR` | `1.75 ATR` | **`2.75 ATR`** | `1.5 – 4.0 ATR` |
| **Trades en Backtest** | 1,775 | 118 | **93** | — |
| **Duración Mediana Trade**| ~2 min | 6.4 min | **7.11 horas** | — |
| **Salidas Take-Profit** | 0 (0%) | 0 (0%) | **26 trades (28.0%)** | — |
| **Efectividad en TPs** | 0% | 0% | **88.5% (23 de 26)** | — |
| **Ganancia Neta en TPs** | $0.00 | $0.00 | **+$128.95** | — |
| **Win Rate Global** | ~10.0% | 8.47% | **30.10%** | — |
| **Beneficio Bruto** | ~$50.00 | $9.54 | **$150.14** | — |
| **Profit Factor** | 0.10 | 0.07 | **0.55** | — |
| **Payoff Ratio** | 0.80 | 0.82 | **1.28** | — |
| **PnL Neto Cuenta ($500)**| -$759 (cuenta liquidada)| -$115.75 (breaker 23%) | **-$121.68 (breaker 26%)** | — |
| **Fricción (Comisiones+Slip)**| $444.60 | $78.71 | **$77.87** | — |
| **Sortino Ratio (Backtest)**| -0.80 | -0.60 | **-0.31** | — |

---

## 2. Diario de Diagnóstico y Aprendizajes por Ejecución

### Run 1: Línea Base Inicial (5m a 1D)
- **Hipótesis**: La configuración de feeds del paper original (5m, 1h, 4h, 1D) combinada con el optimizador WFO por defecto encontraría un perfil viable.
- **Resultado**: 1,775 operaciones hiperactivas, cuenta liquidada en drawdown continuo (-$759).
- **Diagnóstico (The Flaws)**:
  1. *Dilución de consenso*: La vela diaria necesitaba 64 días de calentamiento, y su desfase con la vela de 5m provocó que el 72.6% del tiempo los agentes votasen discordantemente.
  2. *Take-Profit omitido*: El espacio de búsqueda incluía `None`, y el optimizador seleccionó una configuración sin TP.
  3. *Consenso mínimo forzado*: `--min-trades-per-fold 30` obligó al HPO a descartar candidatos selectivos y conformarse con $L_1 = 0.25$ (puro ruido de mercado).
  4. *Fricción devoradora*: El 58.5% de las pérdidas fueron comisiones y slippage.

### Run 2: Perfil Intradía y Take-Profit Mandatorio
- **Hipótesis**: Al armonizar las 16 fuentes de velas en horizontes intradía concordantes (`[15m, 30m, 1h, 2h]`), reducir el warm-up a 5.3 días, exigir Take-Profit y permitir candidatos selectivos con `--min-trades-per-fold 12`, el enjambre operaría con alta convicción.
- **Resultado**: Reducción drástica del 93.4% en sobreoperativa (de 1,775 a 118 trades). El circuit breaker mensual detuvo la simulación protegiendo el capital restante ($384.25).
- **Diagnóstico (The Smoking Gun - La Trampa de Histéresis)**:
  1. El 100% de los 118 trades salieron por `TargetExit` con una duración mediana de solo 6.4 minutos.
  2. Razón matemática: Con $L_1 = 0.55$ e histéresis $H = 0.125$, el nivel de salida era $L_1 - H = 0.425$. Como el consenso medio del enjambre es de 0.137, cualquier impulso inicial que entraba a 0.55 caía de vuelta a 0.41 en 6 minutos, forzando un cierre inmediato a mercado (`Target(0)`).
  3. Ni el Take-Profit (+4.0 ATR) ni el Stop-Loss (-1.75 ATR) tuvieron tiempo material de alcanzarse. Se pagaron $78.71 en comisiones de entrada y salida cada 6 minutos (68% de las pérdidas netas).

### Run 3: Histéresis de la Tesis Original ($H \ge L_1$)
- **Hipótesis**: En la tesis doctoral original, $H$ se diseñó para ser mayor o igual a $L_1$ ($H \ge L_1 \implies L_1 - H \le 0$). Al expandir el grid de histéresis de `[0.10, ..., 0.30]` a `[0.15, ..., 1.00]`, las posiciones no se cerrarán ante retrocesos normales hacia la media, dejando correr el precio hacia las órdenes bracket en el libro de órdenes.
- **Resultado**: **El mayor hito cuantitativo del proyecto hasta la fecha**:
  1. El optimizador escogió de forma autónoma $H = 0.55 > L_1 = 0.30$ (salida en consenso negativo de $-0.25$).
  2. La duración mediana de las operaciones saltó de 6.4 minutos a **7.11 horas**.
  3. **Por primera vez se ejecutaron 26 Take-Profits** (+2.0 ATR) con una efectividad del **88.5%**, generando **+$128.95 netos**.
  4. El beneficio bruto subió de $9.54 a **$150.14** (15.7x) y el Profit Factor se multiplicó por 8x (de 0.07 a **0.55**).
  5. El Payoff Ratio se consolidó en **1.28** (las operaciones ganadoras ganan un 28% más de lo que pierden las perdedoras).
- **Diagnóstico (The Next Frontier - Falsos Arranques)**:
  1. Las 26 salidas por Take-Profit fueron muy rentables (+$128.95), pero 50 operaciones salieron por `TargetExit` (-$156.86) tras aguantar ~10 horas cuando el consenso colapsó a $-0.25$.
  2. Causa: El umbral de entrada base ($L_1 = 0.30$) admitió impulsos débiles con solo 30% de consenso que terminaron disolviéndose tras 10 horas.

### Run 4: Alta Convicción de Entrada ($L_1 \ge 0.40$) y 250 Candidatos
- **Hipótesis**: Elevar el umbral inferior de `threshold_bases` a `0.40` (rango `[0.40, 0.425, 0.45, 0.475, 0.50, 0.525, 0.55, 0.575, 0.60, 0.65]`) obligará a que el optimizador solo seleccione entradas donde al menos el 40% al 65% del enjambre esté inequívocamente de acuerdo.
- **Objetivo**: Eliminar las 50 operaciones de falso arranque y consolidar las 26 operaciones ganadoras de Take-Profit para llevar el Profit Factor por encima de 1.0 y el Sortino a terreno positivo.
- **Optimización de cómputo**: Reducción de 500 a 250 candidatos para agilizar la iteración en Cloud Batch manteniendo una cobertura estadística sólida.

---

## 3. Próximos Pasos de Investigación
1. Analizar el artefacto y métricas del Run 4 en BigQuery (`atsnt_bi.runs` y `atsnt_bi.run_metrics`).
2. Evaluar el impacto de la poda de agentes por feed (`keep_per_feed` 4 vs 6).
3. Evaluar la incorporación opcional de pesos heterogéneos en los feeds (`feed_weights`).
