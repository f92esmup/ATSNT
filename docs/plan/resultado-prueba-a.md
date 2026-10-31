# Resultado de la prueba A (GA)

Fecha de ejecución: 2026-10-31. Ventana: feb–sep 2026 sobre el proxy de perpetuos BTCUSDT, con los costes
de spot del gate.

## 1. Veredicto

**NO PASA.** La variante de decisión, `spot_solo_largo_salida_maker`, tiene neto −2307,51 con 87
operaciones (al menos 50). Según la regla de `scripts/gate_a.sh`, es NO PASA.

Consecuencia (`HANDOFF.md` §5.2): propuesta de borrado de la ruta de ejecución de Binance (L3N). **No se ha
borrado nada.** Espera la confirmación del usuario.

Corrección a un análisis anterior: con la ventana completa, el bruto también es negativo (−133,2 en la
variante maker). El preflight de febrero sí daba un bruto positivo (+81,0), pero no se sostiene en toda la
ventana. Reducir costes no basta.

## 2. Resumen del script (`~/atsnt-gate/summary.txt`, sin cambios)

```
variante                                       neto   comisiones  operaciones   cortas  rechazos
control_perp_con_cortos                    -1815.28       718.42          186       92  none
spot_solo_largo_salida_maker               -2307.51      2174.10           87        0  none
spot_solo_largo_salida_taker               -2272.61      2190.33           87        0  none
referencia_maker_con_cortacircuitos        -1812.75      1526.27           56        0  none

referencia: cortacircuitos mensual disparado en 2026-06-25T13:32:08.864+00:00
decisión (spot_solo_largo_salida_maker): NO PASA: neto -2307.51 con 87 operaciones
```

Línea del cortacircuitos de la referencia: disparado en 2026-06-25T13:32:08.864+00:00. La simulación se
detuvo ahí; sus 56 operaciones no cubren la ventana completa.

## 3. Desglose de la variante maker (filas del log)

- Operaciones: 87, todas largas. Win rate 24,13 %. Factor de beneficio 0,22.
- Bruto tras slippage, antes de comisiones: −133,2 (suma de las filas del log).
- Comisiones: 2174,10. Slippage: 264,58 (informativo; ya está en el precio de llenado).
- Neto: −2307,51 (`summary.txt`). El neto de las filas suma −2307,33; la diferencia es redondeo.
- Máximo drawdown: 2403,40 (47,16 %). Las variantes del gate no tienen cortacircuitos, que quedan
  desactivados por diseño.
- Salidas: 76 StopLoss, 8 TakeProfit, 3 TimeBarrier.
- Auditoría (2.412.372 evaluaciones): `wait` 86,9 %; veto de ruido 73,1 %; neto largo medio en valor
  absoluto 0,128; `long_up` 5,2 %; `long_down` 8,0 %.

Otras variantes: la taker tiene bruto −82,1 y comisiones 2190,33. El control con cortos tiene 186
operaciones y 92 cortas. La referencia tiene bruto −286,3 hasta su cortacircuitos.

## 4. Datos y reproducibilidad

- Commit ejecutado: `9bf0d38` (origin/main), con el árbol de trabajo limpio. Rama `claude/prueba-a`.
- Línea base antes de la prueba: `cargo fmt --check` sin diferencias, `cargo clippy -D warnings` limpio,
  `cargo test --workspace` con 802 pruebas en verde.
- Meses usados: 2026-02 a 2026-09. Tamaños de los Parquet (bytes, `ls -l`):

| Mes | Bytes |
| :--- | ---: |
| 2026-02 | 442.406.826 |
| 2026-03 | 358.234.804 |
| 2026-04 | 232.028.708 |
| 2026-05 | 190.583.887 |
| 2026-06 | 314.822.804 |
| 2026-07 | 193.551.532 |
| 2026-08 | 200.928.887 |
| 2026-09 | 199.909.021 |
| **Total** | **2.132.466.469** |

Duración (marcas de tiempo del script):

| Paso | Duración |
| :--- | :--- |
| Descarga de febrero (preflight) | 122,9 s |
| Descarga de marzo a septiembre | 7 min 34 s |
| `control_perp_con_cortos` | 10 min 08 s |
| `spot_solo_largo_salida_maker` | 9 min 07 s |
| `spot_solo_largo_salida_taker` | 9 min 07 s |
| `referencia_maker_con_cortacircuitos` | 6 min 24 s (se detuvo por el cortacircuitos) |
| Total del gate | 34 min 46 s |

Los logs completos están en `~/atsnt-gate/` y en el scratchpad de la sesión. Los datos no se versionan
(`/data/` está en `.gitignore`).

## 5. Advertencias

- **Moneda.** Los logs muestran `$`. El plan opera en euros. Pendiente de confirmar.
- **Ventana y HPO.** El scorecard del artefacto reporta 84 operaciones OOS, y esta ventana da 87. Si el HPO
  usó datos de feb–sep de 2026, la prueba no es fuera de muestra. Pendiente de confirmar con el historial
  (`HANDOFF.md` §10.1).
- **Umbral.** 50 operaciones, aprobado por el usuario el 2026-10-31 (U4).
- **Sin cambios.** No se han tocado el umbral, los cortacircuitos, `scripts/gate_a.sh` ni el código.
