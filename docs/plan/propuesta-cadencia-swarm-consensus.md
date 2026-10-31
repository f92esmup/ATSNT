# Propuesta de ajuste de cadencia y coste para swarm_consensus

Estado: propuesta, sin ejecutar. Fecha: 2026-10-31. Actualizada tras el veredicto NO PASA de la ventana completa (`docs/plan/resultado-prueba-a.md`): el bruto de la ventana también es negativo, así que esta vía es opcional y tiene menos probabilidad de éxito.

Base: el preflight de febrero de 2026 (`scripts/gate_a.sh --months 02`) y el artefacto
`configs/hpo_btcusdt_2026.json`. La ventana completa (marzo a septiembre) ya terminó: NO PASA, en `docs/plan/resultado-prueba-a.md`.

## 1. Resumen

**Actualización (2026-10-31):** la ventana completa dio NO PASA con 87 operaciones y bruto negativo (−133,2 en la variante maker). La cadencia ya no es un problema (87 ≥ 50). Y "el coste sí" no basta como explicación: en esta ventana tampoco hay borde bruto. Lo que sigue es la decisión del usuario sobre L3N. El resto de este documento se conserva como análisis previo.

- El umbral de 50 operaciones está aprobado. Febrero dio 10. Pero la cadencia no es el problema
  principal: el coste sí. En febrero el bruto fue +81,00 y las comisiones 257,22. Las comisiones son 3,2
  veces el bruto.
- Ajustar el artefacto hasta llegar a 50 operaciones sobre feb–sep de 2026 contaminaría la prueba: sería
  ajustar sobre el mismo dato con el que se decide. No se propone.
- Propongo una re-optimización con costes de spot, calibrada y evaluada solo con datos anteriores, con un
  criterio de parada escrito antes de ver resultados. La ventana feb–sep se usaría una sola vez, como
  validación.

## 2. Evidencia del preflight de febrero

Variante `spot_solo_largo_salida_maker`. Cifras en las unidades del log (`$`; ver §5, pregunta 3).

| Medida | Valor |
| :--- | :--- |
| Operaciones | 10 (4 ganadoras, 40 %) |
| Factor de beneficio | 0,47 |
| Bruto antes de comisiones, tras slippage | +81,00 en total; +8,10 de media |
| Comisiones | 257,22 en total; 25,72 de media |
| Slippage (ya incluido en el precio de llenado) | 29,70 |
| Neto | −176,24 |
| Salidas | 8 StopLoss, 1 TakeProfit, 1 EndOfSimulation |
| Evaluaciones de agentes en `wait` | 86,4 % |
| Evaluaciones sin patrón (veto de ruido) | 71,3 % |
| Neto largo medio en valor absoluto | 0,119 |
| Umbral de entrada L1 | 0,525, unas 4,4 veces el neto medio |

Comprobación: la suma de los netos por operación da −176,24, igual que el neto del log. La suma de los
brutos da +81,00. La diferencia es la comisión total.

Lecturas:

- **Cadencia.** El umbral de entrada está muy por encima del voto típico del enjambre. Hay pocas entradas
  porque la mayoría de las evaluaciones son abstenciones.
- **Coste.** Con comisiones de spot (entrada taker 0,40 %, salida maker 0,25 % o taker 0,40 %), cada
  operación pierde 25,7 de media frente a un bruto de 8,1. Más operaciones sin mejorar el bruto por
  operación empeoran el neto.

## 3. Discrepancias del artefacto (a confirmar)

- **Fuera de la rejilla documentada** (especificación §14.2): `threshold_base` 0,525 (rejilla 0,25–0,55 en
  pasos de 0,05), `threshold_step` 0,075 (rejilla 0,05, 0,10, 0,15), `hysteresis` 0,55 (rejilla 0,10–0,30)
  y `risk_increment` 0 (rejilla 0,005–0,02). Es decir: el artefacto no sale de la rejilla que documenta la
  especificación.
- **Parámetros activos que la especificación no documenta:** `macro_trend_filter = true` y
  `macro_slope_min = 0,02`. Están implementados en `crates/strategies/src/swarm_consensus/policy.rs` y
  tienen tests propios; hay que describirlos antes de cualquier ajuste. `classifier_mode` aparece también
  en esos tests. `bracket_priority`, `breakeven_atr_multiplier` y `trailing_atr_multiplier` están en el
  artefacto y no los he verificado en el código todavía.
- **Contradicción interna de la especificación:** §1 dice que el modelo de posición objetivo no usa take
  profit. §10.5 y el artefacto (`take_profit_atr_multiplier = 4,0`) sí lo usan.
- **Población de agentes:** el artefacto tiene `agents_per_feed = 12` y 192 agentes en `custom_agents`.
  La especificación (§7.4) dice que el pruning conserva K = 6 por feed (96 agentes).
- **Costes:** el artefacto se optimizó con comisiones de perpetuos (0,02 % maker, 0,05 % taker). La prueba
  A usa spot (0,25 % maker, 0,40 % taker), entre 8 y 12,5 veces más caro. El artefacto no se optimizó para
  ese coste.
- **Posible solapamiento con la ventana:** el scorecard del artefacto reporta 84 operaciones OOS. A unas
  10 operaciones por mes, la ventana feb–sep daría del orden de 80 (estimación). Si la ventana completa da
  un número parecido, hay indicios de que coincide con la del HPO. Pendiente de confirmar con el
  historial (`HANDOFF.md` §10.1).

## 4. Propuesta

### 4.1 Lo que no se hace

- No se cambia el artefacto para llegar a 50 operaciones sobre feb–sep.
- No se cambian el umbral de 50, los cortacircuitos ni `scripts/gate_a.sh`.
- No se toca la rama `feat/kraken-eur-spot-portfolio`.

### 4.2 Re-optimización con costes de spot, solo con datos anteriores

- **Datos.** BTCUSDT USD-M, aggTrades de 2024 y 2025. Descargarlos requiere tu permiso (unos 6 GB:
  los ocho meses de 2026 ocupan 2,1 GB).
- **Calibración de umbrales** (tick, volumen, rango; §5.4): con datos de 2024-H1, que terminan antes del
  primer trade del tape. La especificación lo exige (§5.4 y §15.2).
- **Tape y evaluación:** de 2024-07 a 2025-12. Con 70 días de calentamiento, la evaluación efectiva empieza
  hacia 2024-09 (§15.2).
- **Costes:** los de la prueba A: maker 0,25 %, taker 0,40 %, slippage 0,05 %, apalancamiento 1 y
  `resting_maker_exits` activo. La latencia de 100 ms de §11.2 no aparece en el artefacto; hay que
  revisarla.
- **Espacio de búsqueda:** la rejilla de §14.2 completa. Incluye `threshold_base` 0,25–0,55,
  `hysteresis` 0,10–0,30, `stop_atr_multiplier` 1,5–4,0 y `take_profit` en {ninguno, 1,5, 2,5, 4,0}.
  Se añaden dos dimensiones, que se documentan como desviación: `decay_horizon_seconds` en {ninguno, 1
  día, 3 días} y `macro_trend_filter` en {sí, no}.
- **Objetivo:** el fitness vigente (Sortino OOS neto por estabilidad) y la elegibilidad por operaciones
  mínimas por fold. El número de operaciones se informa; no se fuerza.
- **Contar pruebas:** todos los candidatos evaluados, para el DSR.

### 4.3 Criterio de parada, escrito antes de ejecutar

- Si ningún candidato tiene neto positivo con costes de spot en la evaluación de 2024–2025, la hipótesis
  cae ahí. No se usa feb–sep ni se hace más búsqueda.
- Si hay candidatos positivos, se congela el mejor y no se vuelve a tocar.

### 4.4 Validación, una vez por ventana

- **Ventana 1: feb–sep 2026**, la de la prueba A. El diseño solo usa datos de 2024–2025, pero la motivación
  (la cadencia baja) viene de ver febrero. Cuenta como validación parcial. Reglas de siempre: NO PASA si
  neto ≤ 0 con al menos 50 operaciones; NO CONCLUYENTE si hay menos de 50; PASA LA PANTALLA si neto > 0 con
  al menos 50. Ninguna autoriza ejecución.
- **Ventana 2: datos desde octubre de 2026**, cuando se publiquen. Es la limpia. A unas 10 operaciones por
  mes, 50 operaciones tardarían unos 5 meses.

### 4.5 Alternativas, con sus costes

| Opción | Ventaja | Coste o riesgo |
| :--- | :--- | :--- |
| Órdenes límite en la entrada (maker 0,25 % en lugar de taker 0,40 %) | Reduce el coste de entrada un 37,5 % | Cambia el modelo de ejecución, que la especificación fija a mercado (§11.4). Es desarrollo nuevo. |
| Más pares EUR (6 configs en la rama de Kraken) | Más operaciones por mes | Calibración por par. La correlación alta reduce el tamaño efectivo de la muestra. Pertenece a la prueba B. |
| Esperar datos frescos | Limpio | Lento: unos 5 meses para 50 operaciones. |

## 5. Preguntas abiertas

1. ¿El HPO que produjo `hpo_btcusdt_2026.json` usó datos de feb–sep de 2026? Decide si la ventana de la
   prueba A ya fue de entrenamiento.
2. Permiso para descargar 2024–2025 (unos 6 GB).
3. Moneda del log (`$`): ¿euros o dólares? El plan opera en euros.

## 6. Resultado de la ventana completa

- Resultado: **NO PASA**, con 87 operaciones y neto −2307,51. No hay que ajustar para cadencia. Se sigue el
  plan (L3N), pendiente de tu confirmación.
- Bruto negativo (−133,2 en maker): la re-optimización de §4.2 ya no parte de que solo el coste sea el
  problema. Si la quieres, sería una hipótesis nueva con probabilidad de éxito baja.
