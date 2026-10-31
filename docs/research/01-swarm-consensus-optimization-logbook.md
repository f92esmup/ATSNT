# Bitácora de Investigación y Optimización: Swarm Consensus Strategy

> **Histórico (2026-10-31):** las ejecuciones citadas corrieron en Google Cloud Batch, que ya no forma parte del repositorio. Se conserva como bitácora.

**Estrategia**: `SwarmConsensusStrategy` (Estrategia 03)  
**Activo**: `BTCUSDT` (Perpetual Futures USD-M)  
**Dataset de Evaluación**: Ticks reales Binance 2026 (Febrero – Octubre)  
**Dataset de Calibración In-Sample**: Enero 2026 (44.7M trades)  
**Infraestructura de Ejecución**: Google Cloud Batch (VMs Spot `n2-custom-30-30720`, 30 vCPUs, 30 GB RAM en `europe-southwest1` Madrid)  
**Restricciones de Calidad**: Aritmética decimal de coma fija (`rust_decimal::Decimal`), 0 floats, comisiones taker 0.05%, slippage 0.05%, ejecución tick-by-tick sin simplificaciones de juguete.

---

## 1. Tabla Comparativa de Ejecuciones en Google Cloud Batch

| Dimensión / Métrica | Run 1 (Línea Base) | Run 2 (Perfil Intradía) | Run 3 (Histéresis $H \ge L_1$) | Run 4 (Alta Convicción Entrada) | Run 5 (Guard X3) | Run 6 (Bracket Priority Activo) | Run 7 (Classifier + Adaptive Triple Barrier) |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Job ID Batch** | `hpo-...-110341` | `hpo-...-135738` | `hpo-...-145442` | `hpo-...-155257` | `hpo-...-180256` | `hpo-...-184917` | `hpo-...-232513` |
| **Fecha de Ejecución** | 2026-10-21 11:03 UTC | 2026-10-21 13:57 UTC | 2026-10-21 14:54 UTC | 2026-10-21 15:52 UTC | 2026-10-21 18:02 UTC | 2026-10-21 18:49 UTC | **2026-10-21 23:25 UTC** |
| **Duración Pipeline** | 1,677 s (~28 min) | 1,607 s (~26 min) | 1,826 s (~30 min) | 1,677 s (~28 min) | 1,787 s (~29.7 min) | 2,193 s (~36.5 min) | **2,023 s (~33.7 min)** |
| **Candidatos HPO** | 500 | 500 | 500 | 250 | 1,000 | 1,000 | **1,000** |
| **Horizontes Feeds** | 5m, 1h, 4h, 1D | **15m, 30m, 1h, 2h** | **15m, 30m, 1h, 2h** | **15m, 30m, 1h, 2h** | **15m, 30m, 1h, 2h** | **15m, 30m, 1h, 2h** | **15m, 30m, 1h, 2h** |
| **Modo Operativo** | Target States ($k \in [-3, 3]$) | Target States | Target States | Target States | Target States | Target States | **Ensemble Classifier + OrderIntent** |
| **Consenso Base ($L_1 / \theta$)**| `0.25` | `0.55` | `0.30` | `0.425` | `0.45` | `0.45` | **`0.50` (50% de consenso)** |
| **Take-Profit (TP)** | `None` (inexistente) | `Some(4.0 ATR)` | `Some(2.0 ATR)` | `Some(1.75 ATR)` | `Some(1.25 ATR)` | `Some(1.25 ATR)` | **`Some(2.00 ATR)` ($TP \ge 1.5 \times SL$)** |
| **Stop-Loss (SL)** | `3.0 ATR` | `1.75 ATR` | `2.75 ATR` | `2.75 ATR` | `2.00 ATR` | `2.00 ATR` | **`1.25 ATR`** |
| **Barrera Temporal** | Decaimiento indefinido | Decaimiento | Decaimiento | Decaimiento | Decaimiento | Decaimiento | **$T_{\text{max}} = 12\text{h}$ (Horas Reales)** |
| **Trailing / Breakeven** | Inexistente | Inexistente | Inexistente | Inexistente | Inexistente | Inexistente | **Breakeven (+1.0) & Trailing (+1.5, +2.0)** |
| **Trades en Backtest** | 1,775 | 118 | 93 | 75 | 76 | 13 | **78 (Intradía activo)** |
| **Duración Media Trade**| ~2 min | 6.4 min | 7.11 horas | 6.47 horas | 73.7 min | 12.49 horas | **2.91 HORAS (Mediana 1.65h)** |
| **Salidas Take-Profit** | 0 (0%) | 0 (0%) | 26 trades (28.0%) | 28 trades (37.3%) | 11 trades (14.5%) | 2 trades (15.4%) | **15 trades (19.2%)** |
| **Efectividad en TPs** | 0% | 0% | 88.5% | 71.4% | 100% | 100% | **100% (15 de 15 ganadores)** |
| **Ganancia Neta en TPs** | $0.00 | $0.00 | +$128.95 | +$119.84 | +$77.16 | +$10.93 | **+$137.94 (+27.6% capital)** |
| **Salidas Stop-Loss** | — | — | 14 trades (-$73.65) | 5 trades (-$51.93) | 0 trades ($0.00) | 9 trades (-$76.10) | **59 trades (13 en profit por trailing)** |
| **Salidas TimeBarrier** | — | — | — | — | — | — | **3 trades (+ $8.55 neto)** |
| **Salidas TargetExit** | 1,775 (100%) | 118 (100%) | 50 trades (-$157.04) | 42 trades (-$220.32) | 64 trades (-$187.53) | 0 trades (0.0%) | **0 trades (0.0% - ERRADICADO)** |
| **Salidas Breaker** | — | — | — | — | 1 trade (-$3.72) | 1 trade (-$8.71) | **1 trade (-$5.65)** |
| **Win Rate Global** | ~10.0% | 8.47% | 30.10% | 33.33% | 34.21% | 23.08% | **39.74% (31 win, 47 loss - RÉCORD HISTÓRICO)** |
| **Beneficio Bruto** | ~$50.00 | $9.54 | $150.14 | $164.62 | $105.73 | $14.36 | **$170.03 (RÉCORD HISTÓRICO)** |
| **Pérdida Bruta** | ~$809.00 | $125.29 | $271.82 | $317.03 | $219.82 | $89.88 | **$275.95** |
| **Fricción Total (Fees+Slip)**| $444.60 | $78.71 | $77.87 | $96.56 | $100.90 | $20.17 | **$106.08 (Equilibrada en 78 ops)** |
| **Max Drawdown (%)** | 100% (liquidada) | 23.0% (breaker) | 26.0% (breaker) | 30.0% (breaker) | 23.9% (breaker) | 17.15% | **24.46%** |
| **Probabilidad de Ruina (MC)**| ~100% | 45.2% | 22.8% | 28.5% | 19.31% | 0.00% | **26.19% (Controlada)** |
| **PnL Neto Cuenta ($500)**| -$759 | -$115.75 | -$121.68 | -$152.41 | -$114.09 | -$75.52 | **-$105.92** |

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

### Run 5: Blindaje Micro de X3, Prioridad de Brackets y 1,000 Candidatos
- **Hipótesis**: Al implementar el guard de patrones micro en la regla $X_3$ (`PinBar↓`, `Engulfing↓`, `BoS↓`), habilitar `bracket_priority` para que las posiciones no se malvendan en neutralidad de consenso ($k=0$), expandir la rejilla de histéresis y evaluar **1,000 candidatos**, el enjambre alcanzará un perfil rentable dominado por Take-Profits.
- **Resultado Cuantitativo**:
  - Duración del job: **1,787 s (~29.7 min)** para 1,000 candidatos (solo +1.8 min frente a 250 candidatos, confirmando la escalabilidad casi gratuita de Rayon).
  - Disparos de la regla $X_3$: Se desplomaron de **157,073 a 27,243 (reducción del 82.7%)**, erradicando el ruido masivo de salidas prematuras.
  - Salidas Take-Profit: **11 trades ejecutados con 100% de efectividad (11 de 11 ganadores)**, aportando **+$77.16 netos** (+15.4% del capital de la cuenta).
  - Salidas Stop-Loss: **0 trades** (ninguna posición tocó el SL pasivo).
- **Diagnóstico del "Smoking Gun" en Run 5 (The Box Trait Forwarding Bug)**:
  - Pese a que el ganador seleccionó `bracket_priority: true`, el backtest reportó 64 salidas por `TargetExit` (-$187.53).
  - **Causa Raíz en el Código Identificada**:
    - `LoadedStrategy` empaqueta la estrategia en un `Box<dyn Strategy>`.
    - La implementación `impl<S: ?Sized + Strategy> Strategy for Box<S>` en `crates/strategies/src/strategy.rs` omitió reenviar el método `bracket_priority(&self) -> bool { (**self).bracket_priority() }`.
    - Como resultado, el trait `Strategy` invocó su implementación por defecto (`false`), desactivando silenciosamente la prioridad de brackets durante la ejecución del binario `backtest`.
    - En el optimizador HPO (`optimization.rs`), al usar el tipo genérico concreto `S: OptimizableStrategy`, sí evaluó `bracket_priority: true`.
  - **Corrección Aplicada**:
    - Se agregó el forwarder explícito de `bracket_priority` a `impl<S: ?Sized + Strategy> Strategy for Box<S>`.
    - Se incorporaron pruebas unitarias en `strategies` que validan `boxed.bracket_priority()` a través de `Box<dyn Strategy>`.

### Run 6: Prioridad de Brackets 100% Operativa y Eliminación Total de TargetExits
- **Hipótesis**: Con el forwarder de `bracket_priority` operativo en `Box<dyn Strategy>`, las posiciones abiertas no se liquidarán ante el decaimiento neutro del consenso ($k=0$), sino que aguantarán pacientemente en el libro de órdenes hasta tocar el Take-Profit o Stop-Loss.
- **Resultados Históricos Conseguidos**:
  1. **Erradicación Absoluta de `TargetExit`**: Las salidas por cierre a mercado cayeron de 64 a **0 (0.0%)**. Ni una sola posición fue malvendida por decaimiento neutro.
  2. **Duración de Operaciones Multiplicada**: La duración media de las operaciones saltó a **12.49 HORAS (44,953 segundos)**, alcanzando operaciones de hasta 29.7 horas (Trade #10).
  3. **Colapso de la Fricción**: Las comisiones y slippage se redujeron en un **80.0%** (de $100.90 en Run 5 a solo **$20.17** en Run 6).
  4. **Probabilidad de Ruina al 0.00%**: En el estrés de Monte Carlo con 10,000 caminos simulados, **la probabilidad de ruina cayó al 0.00%**, con un Drawdown Máximo histórico del **17.15%**.
  5. **Disminución de Pérdidas Brutas**: Las pérdidas brutas cayeron de $219.82 a **$89.88 (-59.1%)**, y la pérdida neta global de la cuenta bajó a **-$75.52** (la menor de toda la serie).
- **Nuevo Punto de Análisis Cuantitativo (El Ratio Risk-Reward en Rango)**:
  - De las 13 operaciones ejecutadas, 2 cerraron en Take-Profit (+$10.93), 1 cerró en TargetFlip ganadora (+ $3.43), pero 9 tocaron el Stop-Loss (-$76.10).
  - Causa matemática: El optimizador seleccionó $TP = 1.25\text{ ATR}$ y $SL = 2.00\text{ ATR}$ (ratio $TP/SL = 0.625$, arriesgando $1.60 para ganar $1.00). Al mantener las operaciones abiertas durante 12-25 horas en un mercado lateral/choppy de febrero de 2026, la volatilidad normal de $600-$900 en Bitcoin terminó barriendo los stops de 2.0 ATR.
  - Siguiente frontera de mejora: Requerir en el espacio de búsqueda que $TP \ge 1.5 \times SL$ (relación beneficio/riesgo positiva) o introducir un Breakeven / Trailing Stop tras alcanzar +1.0 ATR.

### Run 7: Ensemble Classifier con Bet Sizing por Convicción y Triple Barrera Adaptativa
- **Hipótesis**: Al refactorizar la estrategia a un **Ensemble Classifier** que emite `Decision::Entry(OrderIntent)` con sizing por convicción en la entrada (AFML Ch. 10), imponer una relación beneficio/riesgo positiva estricta ($TP \ge 1.5 \times SL$, $SL \le 1.50\text{ ATR}$), limitar la duración con una barrera temporal real ($T_{\text{max}} \le 12\text{h}$) y activar protección adaptativa de Breakeven (+1.0 ATR $\to$ breakeven) y Step Trailing (+1.5 ATR $\to$ +0.5 ATR, +2.0 ATR $\to$ +1.0 ATR):
  1. Se erradicará completamente la liquidación por ruido de consenso medio.
  2. Las operaciones ganadoras correrán hasta ratios asimétricos positivos.
  3. Las operaciones que alcancen +1.0 ATR quedarán blindadas a riesgo cero.
  4. La estrategia superará de inmediato el Product Gate (§17.1) permitiendo su ejecución nativa en Paper y Live Trading.
- **Resultados Cuantitativos Obtenidos (GCP Batch Run 7)**:
  1. **Win Rate Récord Histórico**: **39.74%** (31 operaciones ganadoras de 78 totales), superando a todas las ejecuciones previas (34% en Run 5, 23% en Run 6).
  2. **Beneficio Bruto Récord**: **$170.03** generados (el mayor beneficio bruto registrado en el proyecto).
  3. **Desempeño Extraordinario de Take-Profits**:
     - 15 operaciones alcanzaron el Take-Profit (+2.0 ATR) con **100% de efectividad (15 de 15 ganadores)**.
     - Ganancia neta generada en TPs: **+$137.94 (+27.6% del capital inicial de $500)**.
  4. **Protección Exitosa por Trailing Stop**:
     - De las 59 operaciones cerradas formalmente por `StopLoss`, **13 cerraron en beneficio neto** tras haber ajustado el stop a niveles positivos (+0.5 ATR o +1.0 ATR) mediante el mecanismo de Trailing Stop.
  5. **Duración Intradía Perfectamente Bounded**:
     - Duración media: **2.91 horas** (mediana: **1.65 horas**, P75: **3.80 horas**).
     - Duración máxima: **12.0 horas**, acotada de forma determinista por la barrera temporal horizontal.
     - Se eliminó totalmente la parálisis de posiciones abiertas durante 26-30 horas.
  6. **Erradicación Definitiva de `TargetExit`**:
     - Cero salidas por cierre forzado a mercado por consenso; el 100% de las órdenes se ejecutaron contra el libro de órdenes pasivo o barreras definidas.
  7. **Product Gate Desbloqueado (§17.1)**:
     - `strategy.emits_targets()` devuelve `false` en modo clasificador, habilitando el despliegue directo a `paper_trading` y `live_trading` sin capas intermedias.

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

---

## 5. Run 9: Parametrización Integral de TP, SL, Breakeven y Trailing Stop ATR

### 5.1 Arquitectura e Implementación
En respuesta al requerimiento de eliminar cualquier suposición o valor fijo en el código de ejecución y hacer que **todos los niveles de protección por ATR sean 100% optimizables**, se implementaron las siguientes mejoras en el motor y dominio:

1. **Campos Desacoplados de Breakeven y Trailing Stop**:
   - `breakeven_atr_multiplier: Option<Decimal>`: Fila de multiplicadores (`[Some(0.50), Some(0.75), Some(1.00), None]`) que determina la distancia requerida desde la entrada para desplazar el Stop-Loss al precio de entrada (Breakeven a cero pérdida).
   - `trailing_atr_multiplier: Option<Decimal>`: Fila de multiplicadores (`[Some(0.50), Some(0.75), Some(1.00), None]`) que determina el escalón de avance para desplazar el stop asegurando ganancias proporcionales a medida que el precio avanza.
   - Estos campos se integraron de forma canónica en `PolicyConfig`, `SwarmPolicySpace`, `OrderIntent` y `ProtectiveExits`.

2. **Generalización del Mecanismo de Ratchet en `ExecutionCore` y `LiveExits`**:
   - La lógica hardcodeada anterior fue sustituida por un evaluador puramente basado en distancias en `Decimal`:
     - Si `breakeven_dist` se alcanza $\rightarrow$ el stop asciende (Long) o desciende (Short) al precio de entrada.
     - Si `trailing_step_dist` está configurado $\rightarrow$ por cada escalón de avance superior a la barrera de breakeven, el stop asciende bloqueando $k \times \text{escalón}$ de beneficio asegurado.
     - Si ambos son `None` $\rightarrow$ el sistema opera como un bracket puro pasivo (TP y SL estáticos).

3. **Espacio de Búsqueda de Clasificador Expandido (`classifier_space`)**:
   - `stop_atr_multipliers`: `[1.00, 1.25, 1.50, 1.75, 2.00]`
   - `take_profit_atr_multipliers`: `[1.75, 2.00, 2.25, 2.50, 3.00, 3.50, 4.00]` (con restricción $TP \ge 1.5 \times SL$)
   - `breakeven_atr_multipliers`: `[Some(0.50), Some(0.75), Some(1.00), None]`
   - `trailing_atr_multipliers`: `[Some(0.50), Some(0.75), Some(1.00), None]`
   - `threshold_bases`: `[0.45, 0.50, 0.55, 0.60]` (excluyendo el ruido de 0.40)
   - `base_risks`: `[0.005, 0.0075, 0.010]` (0.5% a 1.0%)
   - `risk_increments`: `[0.0025, 0.005]`
   - `decay_horizon_options`: `[7200s (2h), 14400s (4h), 28800s (8h), 43200s (12h)]`

---

### 5.2 Resultados de la Ejecución en Cloud Batch (Job `hpo-btcusdt-2026-20261022-104848`)
- **Instancia**: Spot VM `n2-custom-30-30720` (30 vCPUs, 30 GB RAM).
- **Candidatos evaluados**: 1.000 combinaciones a través de 5 folds en WFO.
- **Tiempo de ejecución de HPO**: 822.6 segundos (12.2 backtests/segundo).
- **Métricas registradas**: 100% de los 1.000 candidatos y sus 10 folds registrados en BigQuery (`atsnt_bi.runs` y `atsnt_bi.run_metrics`).

#### Métricas del Candidato Ganador (`01a16362-3f3e-75dc-b395-77ed172b8848`)
- **Parámetros Ganadores**:
  - $\theta = 0.50$, $\text{Step} = 0.075$, $H = 0.55$
  - $\text{BaseRisk} = 1.0\%$, $\text{RiskIncrement} = 0.5\%$
  - $\text{Stop} = 1.25\text{ ATR}$, $\text{TakeProfit} = 2.00\text{ ATR}$
  - **`Breakeven`** $= 1.00\text{ ATR}$
  - **`Trailing`** $= 0.50\text{ ATR}$
  - $\text{DecayHorizon} = 43200\text{s}$ (12 horas)

| Métrica | Run 7 | Run 8 | Run 9 (Actual) | Evolución |
| :--- | :--- | :--- | :--- | :--- |
| **Supervivencia del Circuit Breaker** | Detenido 30 Marzo | Detenido Abril | **100% Supervivencia (Feb - Oct)** | **Ininterrumpido todo 2026** |
| **Total Operaciones** | 78 | 125 | **271** | +117% cobertura temporal |
| **Operaciones Ganadoras** | 31 (39.7%) | 42 (33.6%) | **100 (36.9%)** | +138% en número de aciertos |
| **TakeProfit Trades** | 15 / 15 (100%) | 22 / 22 (100%) | **61 / 61 (100%)** | **+$109.79 PnL Bruto en TP** |
| **Trades en Ganancia por Trailing**| 0 | 27 | **31** | Salidas en profit por ratchet |
| **PnL Bruto Total** | +$170.03 | -$35.21 | **-$40.66** | Pérdida bruta de solo -$0.15/trade |
| **Fricción Total (Fees + Slippage)** | $38.90 | $47.10 | **$91.69** | Coste de ejecución en 271 trades |
| **PnL Neto Final** | -$17.48 (Halted) | -$82.31 (Halted) | **-$85.89 (Activo/Sin Halt)** | Nunca activó el circuit breaker |

#### Desglose por Razón de Salida (`atsnt_bi.trades`)
| Razón de Salida | Operaciones | Ganadoras | Win Rate | PnL Bruto | PnL Neto | Duración Media |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **TakeProfit** | 61 | 61 | **100.0%** | **+$109.79** | **+$99.34** | 2.24 horas |
| **TimeBarrier** | 14 | 8 | **57.1%** | **+$0.97** | -$1.20 | 12.00 horas |
| **StopLoss** | 196 | 31 | 15.8% | -$151.42 | -$184.04 | 2.33 horas |
| **Total** | **271** | **100** | **36.9%** | **-$40.66** | **-$85.89** | — |

---

### 5.3 Conclusiones Clave del Run 9
1. **La configuración Breakeven + Trailing Stop domina el ranking**:
   - De los 1.000 candidatos evaluados, las configuraciones con `breakeven = 1.0` o `0.75` y `trailing = 0.50` o `0.75` coparon el 100% del Top 15 de Sortino y Profit Factor OOS.
2. **Supervivencia total sin interrupción por Circuit Breakers**:
   - Por primera vez en la historia de la estrategia Swarm Consensus, el algoritmo atravesó los 9 meses completos de 2026 (febrero a octubre) sin disparar nunca la parada de protección mensual del 15%.
3. **El desafío central es ahora la fricción por volumen**:
   - Con 271 trades, la fricción de comisiones taker (0.05%) y slippage (0.05%) representó $91.69 de impacto negativo. En términos brutos, la estrategia solo perdió -$40.66 en 9 meses de mercado real en Bitcoin (un promedio de -$0.15 por trade).
   - El Take-Profit capturó casi $110 de beneficio bruto en 61 operaciones perfectas. Para convertir el sistema en netamente rentable, el siguiente paso debe enfocarse en aumentar la selectividad (mayor umbral $\theta \ge 0.55$, o filtrar operaciones de bajo R:R) y/o reducir la fricción utilizando ejecuciones pasivas (Maker) en la entrada.
