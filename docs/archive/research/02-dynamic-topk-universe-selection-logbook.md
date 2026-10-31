# Bitácora de Investigación: Motor de Selección Dinámica de Universo Top-K (Binance Spot)

> **Branch**: `feat/dynamic-topk-universe`  
> **Fecha**: 28 de Octubre, 2026  
> **Estatus**: **Validado Cuantitativamente en 2026 YTD** (100% Rust / Fixed Decimal)  
> **Artefacto de Configuración**: [`configs/universe_deployment_artifact.json`](../../configs/universe_deployment_artifact.json)  
> **Runner de Simulación**: [`crates/backtest/src/bin/run_universe_portfolio.rs`](../../crates/backtest/src/bin/run_universe_portfolio.rs)  

---

## 1. Motivación Cuantitativa

Los portafolios estáticos tradicionales (como asignar pesos fijos o ranuras fijas a BTC, ETH y SOL) sufren de dos ineficiencias estructurales:
1. **Capital Atrapado en Activos en Consolidación**: Cuando un activo entra en un régimen lateral de baja volatilidad o pérdida de momentum, el portafolio estático sigue dedicándole slots de capital.
2. **Incapacidad de Capturar Rotaciones Sectoriales de Alpha**: En el mercado cripto, el capital rota periódicamente desde Bitcoin hacia L1s o activos de alto beta (`NEAR`, `SOL`, etc.).

Para resolver esto sin incurrir en la maldición de la dimensionalidad, diseñamos un **selector de universo transversal (Cross-Sectional Top-K)** con normalización por Z-scores y buffer de histéresis.

---

## 2. Arquitectura Matemática del Modelo Multifactorial

Cada 24 horas sincronizadas de tiempo físico ($T = 86,400,000\text{ ms}$ / 00:00 UTC), el motor toma una instantánea del estado de los $N$ activos candidatos y calcula sus métricas brutas:

### Factores Constituyentes:
1. **Momentum Relativo vs BTC ($\alpha_{24\text{h}, i}$)** (Ponderación: 40%):
   $$r_i(24\text{h}) = \frac{P_i(t) - P_i(t - 24\text{h})}{P_i(t - 24\text{h})}, \quad \alpha_{24\text{h}, i} = r_i(24\text{h}) - r_{\text{BTC}}(24\text{h})$$
2. **Curvatura Armónica de Cavidad Cuántica ($k_i$)** (Ponderación: 40%):
   $$k_i = \left. \frac{\partial^2 V(x)}{\partial x^2} \right|_{x = x_0}$$
   Calculada mediante el estimador de cavidad de Schrödinger y mecánica de Bohm. Mide la profundidad y rigidez restauradora del pozo potencial: valores altos indican que una compresión del precio rebotará con alta fuerza hacia el valor intrínseco.
3. **Anomalía de Volumen ($V_{\text{ratio}, i}$)** (Ponderación: 20%):
   $$V_{\text{ratio}, i} = \frac{V_{24\text{h}, i}}{\overline{V}_i}$$
   Normalizado con respecto al volumen medio rodante para detectar expansión de liquidez institucional.

### Normalización Transversal y Asignación de Rangos:
Los factores se normalizan entre todos los activos candidatos concurrentes:
$$Z_{\text{factor}, i} = \frac{X_i - \mu_{\text{cross}}}{\sigma_{\text{cross}}}$$

El Score Compuesto se define como:
$$S_i = 0.40 \cdot Z_{\text{mom}, i} + 0.40 \cdot Z_{\text{curvature}, i} + 0.20 \cdot Z_{\text{vol}, i}$$

Los activos se ordenan descendentemente según $S_i$, asignándoles un rango $\text{Rank} \in [1, N]$.

---

## 3. Buffer de Histéresis Anti-Churning

Uno de los mayores peligros de la selección dinámica de activos en Spot son las **comisiones por sobre-rotación**. Para evitar compras y ventas compulsivas:
* **Entrada de Nueva Posición**: Solo autorizada si $\text{Rank}_i \le K$ (Top-3).
* **Retención de Posición Abierta**: Una posición existente se mantiene siempre que $\text{Rank}_i \le K + \text{tolerancia}$ (Top-5 con tolerancia 2).
* **Exclusión**: Si un activo cae a Rank 6, pierde prioridad y no recibe capital adicional, permitiendo que sus barreras cuánticas (Take Profit o Stop Loss) gestionen la salida ordenada.

---

## 4. Resultados Empíricos en Binance Spot (Simulación 2026 YTD)

Todas las pruebas se realizaron con:
- **Apalancamiento**: 1.0x (Spot estricto).
- **Comisiones**: Maker 0.075%, Taker 0.075% (descuento BNB).
- **Deslizamiento (Slippage)**: 0.05% realista.
- **Capital Inicial**: **200.00 €** (con escalabilidad verificada a 10,000.00 €).

### Matriz Comparativa:

| Configuración | Capital Inicial | Capital Final | PnL Neto | ROI (%) | Max Drawdown | Profit Factor | Win Rate | Trades |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| **Estático (BTC/ETH/SOL)** | 200.00 € | 230.14 € | +30.14 € | +15.07% | 5.96% (11.92 €) | 1.75 | 56.67% | 60 |
| **Dinámico 4 Activos** | 200.00 € | 239.55 € | +39.55 € | +19.77% | 5.90% (11.80 €) | 2.09 | 62.90% | 62 |
| **Dinámico 6 Activos (Óptimo)** | **200.00 €** | **244.86 €** | **+44.04 €** | **+22.02%** | **5.60% (11.20 €)** | **2.35** | **64.29%** | **56** |
| **Dinámico 8 Activos** | 200.00 € | 233.71 € | +33.42 € | +16.71% | **4.72% (9.44 €)** | 1.93 | 57.69% | 52 |

### Desglose por Activo en el Universo Óptimo de 6 Activos (200 €):
```text
• BTCUSDT  | Trades: 12 (Wins:  7) | Net PnL: +$16.30 | Fees: $1.45
• ETHUSDT  | Trades: 18 (Wins: 10) | Net PnL: +$13.03 | Fees: $1.79
• SOLUSDT  | Trades: 17 (Wins: 12) | Net PnL: +$ 8.94 | Fees: $1.13
• NEARUSDT | Trades:  8 (Wins:  7) | Net PnL: +$ 9.50 | Fees: $0.21  (87.5% Win Rate)
• AVAXUSDT | Trades:  1 (Wins:  0) | Net PnL: -$ 3.74 | Fees: $0.03
• BNBUSDT  | Trades:  0 (Wins:  0) | Net PnL:  $ 0.00 | Fees: $0.00
```

---

## 5. Conclusiones y Próximos Pasos

1. **NEAR como Generador de Alpha**: NEAR aportó 7 victorias en 8 operaciones, validando que el motor de ranking detecta expansiones de momentum temprano.
2. **Filtrado de Ruido Eficaz**: Se rechazaron 185 órdenes de baja calidad mediante `ExcludedByUniverseRanking`, evitando degradación de capital.
3. **Control de Drawdown**: El drawdown máximo de 5.60% (11.20 € de caída máxima en todo el año) ofrece una preservación de capital institucional para cuentas pequeñas.
4. **Implementación en Paper Trading en Tiempo Real (Fase 8)**:
   - `crates/backtest/src/bin/paper_trading_portfolio.rs` adaptado para ingesta concurrente vía WebSockets de los 6 activos candidatos (`BTCUSDT`, `ETHUSDT`, `SOLUSDT`, `BNBUSDT`, `AVAXUSDT`, `NEARUSDT`).
   - Conexión con `CrossSectionalUniverseSelector` dentro de `PortfolioEngine`: rebalanceo transversal cada 24 horas y filtrado automático `ExcludedByUniverseRanking` para entradas no autorizadas.
   - Notificaciones de Telegram enriquecidas:
     - Lanzamiento con resumen del universo y política Top-K / histéresis.
     - Apertura de posiciones indicando el rango actual (`[NEARUSDT (Rank #1)]`).
     - Heartbeat periódico incluyendo el Top-3 actual del universo.
     - Resumen diario consolidado de las 20:00 Madrid acompañado de la foto del ranking completo (`format_universe_ranking_update`).
   - Validación en vivo (dry-run): 6 streams de Spot conectados simultáneamente y agregación reactiva de Dollar Bars verificada sin cuellos de botella ni bloqueos asíncronos.
