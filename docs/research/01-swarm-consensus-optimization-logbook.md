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
| **Job ID Batch** | `hpo-...-110341` | `hpo-...-135738` | `hpo-...-145442` | `hpo-...-155257` |
| **Fecha de Ejecución** | 2026-10-21 11:03 UTC | 2026-10-21 13:57 UTC | 2026-10-21 14:54 UTC | 2026-10-21 15:52 UTC |
| **Duración Pipeline** | 1,677 s (~28 min) | 1,607 s (~26 min) | 1,826 s (~30 min) | 1,677 s (~28 min) |
| **Candidatos HPO** | 500 | 500 | 500 | **250** (dominado por ingesta) |
| **Horizontes Feeds** | 5m, 1h, 4h, 1D | **15m, 30m, 1h, 2h** | **15m, 30m, 1h, 2h** | **15m, 30m, 1h, 2h** |
| **Warm-Up Requerido** | 64 días | **5.3 días** | **5.3 días** | **5.3 días** |
| **Consenso Base ($L_1$)** | `0.25` | `0.55` | `0.30` | **`0.425`** |
| **Histéresis ($H$)** | `0.25` | `0.125` | `0.55` ($H > L_1$) | **`0.65` ($L_1 - H = -0.225$)** |
| **Take-Profit (TP)** | `None` (inexistente) | `Some(4.0 ATR)` | `Some(2.0 ATR)` | **`Some(1.75 ATR)`** |
| **Stop-Loss (SL)** | `3.0 ATR` | `1.75 ATR` | `2.75 ATR` | **`2.75 ATR`** |
| **Trades en Backtest** | 1,775 | 118 | 93 | **75** |
| **Duración Mediana Trade**| ~2 min | 6.4 min | 7.11 horas | **6.47 horas** |
| **Salidas Take-Profit** | 0 (0%) | 0 (0%) | 26 trades (28.0%) | **28 trades (37.3%)** |
| **Efectividad en TPs** | 0% | 0% | 88.5% (23 de 26) | **71.4% (20 de 28)** |
| **Ganancia Neta en TPs** | $0.00 | $0.00 | +$128.95 | **+$119.84** |
| **Salidas Stop-Loss** | — | — | 14 trades (-$73.65) | **5 trades (-$51.93)** |
| **Salidas TargetExit** | 1,775 (100%) | 118 (100%) | 50 trades (-$157.04) | **42 trades (-$220.32)** |
| **PnL Neto Solo Brackets**| -$759 | -$115.75 | **+$55.39** | **+$67.91 (Neto Positivo)** |
| **Win Rate Global** | ~10.0% | 8.47% | 30.10% | **33.33%** |
| **Beneficio Bruto** | ~$50.00 | $9.54 | $150.14 | **$164.62** |
| **Pérdida Bruta** | ~$809.00 | $125.29 | $271.82 | **$317.03** |
| **Profit Factor** | 0.10 | 0.07 | 0.55 | **0.52** |
| **Payoff Ratio** | 0.80 | 0.82 | 1.28 | **1.04** |
| **PnL Neto Cuenta ($500)**| -$759 (cuenta liquidada)| -$115.75 (breaker 23%) | -$121.68 (breaker 26%) | **-$152.41** |
| **Fricción (Comisiones+Slip)**| $444.60 | $78.71 | $77.87 | **$96.56** |
| **Sortino Ratio (Backtest)**| -0.80 | -0.60 | -0.31 | **-0.32** |

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

### Run 4: Alta Convicción de Entrada ($L_1 \ge 0.40$) y Evaluación de Candidatos
- **Hipótesis**: Elevar el umbral inferior de `threshold_bases` a `0.40` (rango `[0.40, 0.425, 0.45, 0.475, 0.50, 0.525, 0.55, 0.575, 0.60, 0.65]`) obligará a que el optimizador solo seleccione entradas donde al menos el 40% al 65% del enjambre esté inequívocamente de acuerdo.
- **Resultado Cuantitativo**:
  - Ganador HPO: $L_1 = 0.425$, $H = 0.65$ (salida en $-0.225$), $SL = 2.75\text{ ATR}$, $TP = 1.75\text{ ATR}$.
  - Operaciones totales: **75** (reducción adicional del 19.4% respecto a Run 3).
  - Win rate global: **33.33%** (25 ganadoras, 50 perdedoras).
  - Beneficio bruto: **$164.62** (récord histórico), Pérdida bruta: **$317.03**, PnL neto: **-$152.41**.
- **Hallazgo Computacional (Candidatos vs Duración)**:
  - Reducir de 500 a 250 candidatos **no redujo el tiempo total del Batch Job** (28 minutos en ambos casos).
  - Causa técnica: La ingesta y descompresión de 179 millones de ticks desde GCS (~20 min) y el cómputo de las 16 series de velas (~4 min) dominan el 85% del tiempo. El barrido de folds en 30 vCPUs con Rayon tarda solo ~2-4 min.
  - Conclusión: Se debe mantener 500 candidatos para maximizar la resolución del espacio sin penalización de tiempo.

---

## 3. Diagnóstico Profundo: Puntos de Fuga ("The Leaks") Identificados

### Punto de Fuga #1: La Sangría Asimétrica de `TargetExit` vs Órdenes Bracket
Al desglosar las 75 operaciones del Run 4 por su `exit_reason` exacto en BigQuery (`atsnt_bi.trades`):

| Tipo de Salida | Operaciones | Ganadoras | Win Rate | PnL Bruto | PnL Neto | Fricción (Fees+Slip) | Duración Media |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **TakeProfit** | 28 | 20 | **71.4%** | **+$139.05** | **+$119.84** | $38.88 | 6.13 horas |
| **StopLoss** | 5 | 0 | 0.0% | -$48.37 | **-$51.93** | $7.13 | 1.38 horas |
| **Subtotal Brackets (TP + SL)** | **33** | **20** | **60.6%** | **+$90.68** | **+$67.91** | **$46.01** | — |
| **TargetExit (Cierre a Mercado)** | **42** | **5** | **11.9%** | **-$195.03** | **-$220.32** | **$50.56** | 7.32 horas |

**La Revelación Estadística**:
- Las órdenes bracket situadas en el libro de órdenes (TakeProfit y StopLoss) son **altamente rentables por sí solas (+ $67.91 netos con un 60.6% de win rate)**.
- El 100% de las pérdidas netas del sistema provienen exclusivamente de **`TargetExit` (-$220.32 en 42 operaciones)**.
- Además, el 69.0% de los `TargetExit` (29 de 42 trades) cerraron a mercado dentro del 20% inferior del precio mínimo de la operación (`trough_equity`), es decir, **malvendieron en el suelo exacto del retroceso**.

### Punto de Fuga #2: La Asimetría de la Regla X3 (`Macro(Bearish)`)
Al auditar los disparos de reglas en el enjambre (`strategy_audit.rule_firings`):
- Regla E1 a E5: disparadas entre 431 y 22,334 veces (promedio ~5,000).
- Reglas X1, X2, X4: disparadas entre 1,125 y 4,965 veces.
- **Regla X3**: disparada **157,073 veces** (¡7x más que la regla de entrada más frecuente y 30x más que el resto de salidas!).

**Causa Raíz en el Código**:
- En `crates/strategies/src/swarm_consensus/rules.rs`:
  - Todas las reglas de entrada (E1-E5) y salidas (X1, X2, X4) exigen una combinación estricta de régimen macro **y** un patrón micro de confirmación (PinBar, Engulfing, BoS, InsideBar).
  - La regla X3 exige **únicamente `Macro(Bearish)`** (`alma_slope < -alma_slope_gene`), sin ningún patrón micro.
  - Como el ruido es global (`micros.is_empty() == false` si hay cualquier patrón residual en la ventana de 20 velas), cualquier retroceso intradía de $50-$100 en BTC vuelve el ALMA negativo. Decenas de agentes disparan X3 a la vez, hundiendo el consenso por debajo de $-0.225$ y provocando una orden de venta a mercado de pánico en pleno soporte.

### Punto de Fuga #3: Conflicto Filosófico entre Target Positions y Brackets
- Cuando una posición entra con bracket orders (TP a +1.75 ATR, SL a -2.75 ATR), la premisa es que el precio oscilará hasta resolver la tesis.
- Si el decaimiento natural del consenso a neutro ($k=0$) cancela los brackets y lanza una orden a mercado:
  1. Se paga doble tarifa taker (0.05% comisiones + 0.05% slippage = $50.56 adicionales).
  2. Se abortan operaciones que habrían alcanzado el TakeProfit horas después.

---

## 4. Propuestas Técnicas de Solución

1. **Propuesta A: Blindaje de la Regla X3 con Confirmación Micro**
   - Modificar la regla X3 en `crates/strategies/src/swarm_consensus/rules.rs` para requerir confirmación micro bajista: `All(&[Macro(Bearish), Any(&[Micro(PinBar, Down), Micro(Engulfing, Down), Micro(BreakOfStructure, Down)])])`.
   - Esto evitará que una simple inflexión de ALMA liquide posiciones en retrocesos saludables.

2. **Propuesta B: Política de Cierre por Fusión / Inversión Pura (`TargetFlip` Only)**
   - Permitir que las posiciones abiertas con bracket orders (TP y SL) se resuelvan por sus propios límites pasivos en el libro de órdenes.
   - El decaimiento a nivel neutro ($k=0$) retira el tamaño adicional pero no malvende a mercado la posición base; la posición solo se cierra/invierte activamente ante una señal de convicción contraria de entrada (`TargetFlip` con consenso opuesto $\ge L_1$).

3. **Propuesta C: Expansión de Histéresis a Régimen Simétrico Fuerte ($H \ge L_1 + 0.30$)**
   - Extender el rango de histéresis hasta $H \in [0.80, 1.10]$, garantizando que el umbral de salida $L_1 - H$ se sitúe por debajo de $-0.40$. De este modo, una posición alcista nunca se cierra por ruido hasta que el consenso bajista supere el 40%.

4. **Propuesta D: Restaurar 500 Candidatos en HPO**
   - Mantener el grid de 500 candidatos en los archivos de configuración para maximizar la exploración del espacio sin penalización apreciable de tiempo.
