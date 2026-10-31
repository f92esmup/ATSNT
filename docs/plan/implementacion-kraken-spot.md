# Plan de implementación: swarm en spot de Kraken

> **Estado (2026-10-31):** aprobado en G0. Lote 1 y lote 2 hechos en `claude/awesome-lamport-ch9ytr`; GA, GB y GP pendientes. Ver [`docs/ROADMAP.md`](../ROADMAP.md).
> **Relación con otros documentos:** la estrategia y las decisiones están en [`refactor-local-kraken.md`](refactor-local-kraken.md) (v4). Este documento detalla cómo se construye.
> **Base de código:** commit `50b4ee5`. Línea base en verde: `cargo check`, `cargo test --workspace`, `cargo fmt --check`, `cargo clippy -D warnings`.
> **Fecha:** 2026-10-30.

## 0. Qué empieza al aprobar y qué espera

El plan tiene cuatro puertas. Cada una decide si se sigue.

| Puerta | Qué es | Quién la ejecuta | Si no pasa |
|---|---|---|---|
| **G0** | Aprobación de este plan | Tú | No se toca el código |
| **GA** | Prueba económica A: swarm solo largo con comisiones de spot, sobre datos de perpetuos de Binance (proxy) | Tú, en tu equipo, con los comandos de la v4 | Se para la ejecución. Solo queda investigación de estrategia |
| **GB** | Prueba B: recalibrar con datos de Kraken, walk-forward y DSR | Yo, con datos de Kraken | No se construye ejecución |
| **GP** | Paper: al menos 4 semanas, con paridad dentro de tolerancia | Tú y yo | No se pasa a dinero real |

**Al aprobar (G0), empieza esto:** WP-00 a WP-07. Son trabajo que no depende del resultado de la prueba: goldens, reglas hexagonales, refactor y borrado.

**Espera GA:** WP-08 a WP-11, y WP-12 (la prueba B en sí). El libro de la grabadora (WP-10) conviene empezar en cuanto pase GA, porque el libro no se puede reconstruir después.

**Espera GB:** WP-13 a WP-16 (ejecución, fidelidad, operación).

**Espera GP:** WP-17 (dinero real, por tramos).

**Dependencias de orden dentro de la prueba B:** WP-05 (renombrar tape) y WP-08 (convertidor de trades de Kraken a `Trade`) van antes que WP-12. Sin ellos, la prueba B no puede leer datos de Kraken.

**Lo que no hace este plan:** no opera con dinero real, no toca futuros ni Kraken Prop, y no crea ventaja. Si la prueba A o la B fallan, se para la parte de ejecución.

## 1. Lo que cambia respecto a la v4

- **Prueba B es un reajuste, no la misma prueba con mejores datos.** El umbral de dólar-barra y los umbrales por feed de `swarm_shared.json` se calibraron con el volumen de BTCUSDT en Binance. XBT/EUR en Kraken tiene mucho menos volumen y está en EUR, así que el mismo artefacto no significa lo mismo. La prueba B recalibra con `calibrate_feeds` sobre datos de Kraken, y después hace walk-forward con `run_hpo` y DSR.
- **La prueba B necesita meses de cinta, no un mes.** El presupuesto de descarga se calcula con la medida real del primer día (sección 4, WP-09).
- **La tarifa de la cuenta sale de `TradeVolume`, no de la captura.** Los campos de comisiones de `AssetPairs` llevan vacíos desde la actualización del 8 de septiembre de 2026 (fragmento de la documentación; verificar literal). `FeeSource` llama a `TradeVolume` con el par, porque solo devuelve objetos de comisión cuando se pasa `pair`.
- **La clave de API de spot necesita permiso de WebSocket.** Sin él no se obtiene el token del WebSocket privado. Especificación de la clave: consulta de fondos, crear órdenes, cancelar órdenes, acceso a WebSocket y **sin retirada**.
- **Cuerpo de la petición en formulario.** Se elige una sola codificación (formulario) y se firman exactamente los bytes que se envían. El fragmento de la documentación advierte de un desajuste entre JSON y formulario en el propio ayudante de Kraken.
- **Precios y cantidades del libro se leen como texto o `Decimal`, nunca como `f64`**, para que el checksum CRC32 pueda reconstruirse.

## 2. Fuentes y verificaciones pendientes

Los datos vienen de fragmentos de búsqueda, no del texto literal de las páginas. `docs.kraken.com` no se puede abrir desde esta sesión (no resuelve el dominio). Cada punto de esta tabla se marca "verificar literal" hasta que se lea la página completa. **Ningún valor de prueba (fixture) se escribe en este plan.** Los vectores de firma y de checksum se copian del texto literal de la guía en el momento de implementarlos.

| Tema | Lo que sabemos (fragmento) | Qué falta | Dónde verificar | Bloquea a |
|---|---|---|---|---|
| Pares (`AssetPairs`) | Campos `altname`, `wsname`, `ordermin`, `costmin`, `tick_size`, `pair_decimals`, `lot_decimals`, `status`. El ejemplo es XBT/USD | Pares EUR reales (XBT/EUR, ETH/EUR, SOL/EUR) y su semántica exacta | [Get Tradable Asset Pairs](https://docs.kraken.com/api/docs/rest-api/get-tradable-asset-pairs) | WP-08, WP-09 |
| Comisiones en `AssetPairs` | Arrays `fees` y `fees_maker` vacíos desde el 8 de septiembre de 2026 | Confirmar con la cuenta real | [Changelog](https://docs.kraken.com/exchange/changelog) | WP-13 |
| `TradeVolume` | Devuelve `fee`, `minfee`, `maxfee`, `nextfee`, `tiervolume`, `nextvolume`, `fees` (taker) y `fees_maker`. Requiere `pair` para los objetos de comisión | Formato exacto de la respuesta | [Get Trade Volume](https://docs.kraken.com/api-reference/account-data/get-trade-volume) | WP-13 |
| Trades públicos | `since` en nanosegundos, `count` hasta 1000, campo `last` para paginar (fragmento de la primera búsqueda) | **Profundidad histórica** del endpoint; formato de cada trade (campos y orden) | [Get Recent Trades](https://docs.kraken.com/api/docs/rest-api/get-recent-trades/) | WP-08, WP-09, GB |
| Firma API-Sign | Fórmula: HMAC-SHA512 con clave base64-decodificada sobre ruta + SHA256(nonce + postdata) | **Vector de prueba literal** y el formato exacto de `postdata` | [Spot REST Authentication](https://docs.kraken.com/api/docs/guides/spot-rest-auth/) | WP-13 |
| AddOrder | Campos obligatorios: `nonce`, `ordertype`, `type`, `volume`, `pair`. `volume` en unidades base | Esquema de respuesta y `txid`; nombres exactos de `oflags` (`post`, `fcib`, `fciq`, `nompp`, `viqc`); efecto de `validate` | [Add Order](https://docs.kraken.com/api/docs/rest-api/add-order) | WP-14 |
| Libro v2 | Checksum CRC32 sobre los 10 niveles superiores; los niveles con cantidad cero se borran; se concatenan precio y cantidad sin punto decimal ni ceros a la izquierda; asks antes que bids | **Ejemplo de cálculo literal** con cadenas intermedias | [Book checksum (WebSocket v2)](https://docs.kraken.com/api/docs/guides/spot-ws-book-v2/) | WP-10, WP-15 |
| Canal `trade` v2 | Campos `symbol`, `side`, `price`, `qty`, `ord_type`, `trade_id`, `timestamp` (UTC, microsegundos). `snapshot` como opción de suscripción | Significado exacto de `trade_id`; si `price` es precio medio | [Trades (v2)](https://docs.kraken.com/api/docs/websocket-v2/trade) | WP-08, WP-10 |
| Nombres en v2 | Pares v1 usan `wsname` (XBT/USD) | Si v2 usa `XBT/EUR` o `BTC/EUR` | `wsname` de `AssetPairs` para el par EUR | WP-10 |
| WebSocket privado | Host `wss://ws-auth.kraken.com/v2`. Token con `POST /0/private/GetWebSocketsToken`, firmado como el resto de privados | Renovación del token | [Spot WebSocket Authentication](https://docs.kraken.com/api/docs/guides/spot-ws-auth/) | WP-14 |
| Caducidad del token | Fuentes contradictorias: 15 minutos para usarlo; no caduca una vez hay suscripción privada | Comportamiento exacto | [Get Websockets Token](https://docs.kraken.com/api/docs/rest-api/get-websockets-token/) | WP-14 |
| Canal `executions` | Suscripción con token y `snap_orders`, `snap_trades` | Nombres y semántica de las banderas | [Executions](https://docs.kraken.com/api/docs/websocket-v2/executions/) | WP-14 |
| Límites de tasa (REST) | Support: Starter 15 puntos / 0,33 por segundo; Intermediate 20 / 0,5; Pro 20 / 1. Cada clave tiene su contador | Cifras oficiales; **el coste de los históricos está en conflicto** (2 puntos en docs, 4 en soporte) | [Spot REST Rate Limits](https://docs.kraken.com/api/docs/guides/spot-rest-ratelimits/) | WP-13, WP-09 |
| Límites de trading | Contador por par; REST, WebSocket y FIX comparten presupuesto; cancelar cuesta hasta 8 puntos si ocurre a menos de 5 s de crear la orden; cortes en 60, 125 y 180 | Confirmar en la guía oficial | [Spot Trading Limits](https://docs.kraken.com/api/docs/guides/spot-ratelimits/) | WP-14 |
| Nivel de verificación | Determina el tier de límites | **Tu nivel** (no lo conozco) | Tu cuenta | WP-13 |
| Permisos de clave | Consulta, crear, cancelar, WebSocket, sin retirada | Confirmar nombres de los permisos | [API key permissions](https://docs.kraken.com/exchange/guides/rest/api-keys) | WP-00 |

## 3. Paquetes de trabajo

Cada paquete indica archivos y tipos, pruebas y fixtures, criterio de aceptación, dependencias y la puerta que espera.

### WP-00. Preparación (al aprobar; lo hace el usuario)

- **Archivos:** `.env` con permisos `600`. Sin código.
- **Acción:**
  - crear la clave de spot con los permisos de la tabla de arriba;
  - recrear el token de Telegram;
  - permitir `docs.kraken.com` en la configuración de red de la sesión, para leer el texto literal (o pegar en el chat las secciones de autenticación y del ejemplo de checksum).
- **Aceptación:** la clave devuelve el saldo con una consulta de solo lectura, y no tiene permiso de retirada.
- **Espera:** G0.

### WP-01. Goldens de swarm (al aprobar)

- **Archivos:** `crates/backtest/tests/golden_execution.rs`, y nuevos ficheros en `crates/backtest/tests/golden/` con prefijo `swarm_`.
- **Pruebas:** casos de swarm en bar y en tick, con `allow_short` true y false, con el modo classifier (y el legacy si el harness lo admite).
- **Aceptación:** se generan una vez con `UPDATE_GOLDEN=1 cargo test -p backtest --test golden_execution`, el diff se revisa y las ejecuciones siguientes son idénticas.
- **Dependencias:** ninguna.
- **Espera:** G0.

### WP-02. Reglas hexagonales R2 a R7 (al aprobar)

- **Archivos:** `crates/backtest/tests/architecture_contract.rs`.
- **Pruebas:**
  - R2: `adapters` no depende de `backtest`.
  - R3: los módulos de librería de `backtest` no contienen `adapters::` (solo `main.rs` y `wiring.rs`).
  - R4: `crates/ports` no contiene `Binance`, `Kraken`, `Parquet` ni `BigQuery`.
  - R5: `domain` no contiene `f32` ni `f64`, salvo en una lista explícita de módulos de estadística.
  - R7: ningún `SystemTime::now()` ni `Utc::now()` fuera de `adapters`.
- **Aceptación:** las reglas se escriben primero y fallan donde corresponde. Pasan al completar WP-03 y WP-04.
- **Espera:** G0.

### WP-03. Puertos y movimientos (al aprobar)

- **Archivos:**
  - `crates/ports/src/run_recorder.rs` (trait movido desde `crates/adapters/src/analytics.rs`).
  - `crates/ports/src/clock.rs`: `trait Clock { fn now_ms(&self) -> i64; }`.
  - `crates/adapters/src/clock.rs`: `SystemClock`.
  - `crates/backtest/src/wiring.rs` (nuevo): único sitio con implementaciones concretas.
  - `crates/backtest/src/{paper,tick_simulation,recording,parity}.rs`: reciben puertos genéricos en lugar de `adapters::`.
  - `crates/ports/src/execution.rs` y `crates/ports/src/lib.rs`: sin menciones a Binance.
- **Pruebas:** los tests de contrato de `run` y de `recording` existentes; `SimClock` en tests de tiempo.
- **Aceptación:** R2, R3, R4 y R7 pasan; los goldens de WP-01 son idénticos; `fmt`, `clippy` y tests en verde.
- **Dependencias:** WP-01, WP-02.
- **Espera:** G0.

### WP-04. Reglas puras a dominio (al aprobar)

- **Archivos:** mover `daily_watermark`, `excursion`, `fee_ledger`, `monthly_guard`, la parte contable de `execution_core`, `metrics` y `walk_forward` (si no tiene E/S) a `crates/domain/src/`. Sus tests se mueven con ellos.
- **Verificación previa:** grep de `std::fs`, `tokio`, `rayon`, `rand` y `SystemTime` en cada módulo antes de moverlo.
- **Pruebas:** los tests existentes en su nueva ubicación. Los goldens no cambian.
- **Aceptación:** `crates/domain` compila sin dependencias de E/S; goldens idénticos.
- **Dependencias:** WP-03.
- **Espera:** G0.

### WP-05. Renombrar `tape` (al aprobar; prerrequisito de la prueba B)

- **Archivos:** `crates/adapters/src/binance_parquet.rs` pasa a `crates/adapters/src/storage/tape_parquet.rs`. Nombres genéricos: `TapeReader`, `TapeWriter`. El esquema no cambia en este paso.
- **Pruebas:** round-trip de Parquet con los tests existentes.
- **Aceptación:** mismos resultados que antes; ninguna referencia a Binance en el módulo.
- **Dependencias:** WP-03.
- **Espera:** G0.

### WP-06. Telegram (al aprobar)

- **Archivos:** `crates/adapters/src/gcp/telegram.rs` pasa a `crates/adapters/src/telegram/`. Se borra `crates/adapters/src/gcp/` salvo ese archivo.
- **Pruebas:** los tests de alertas existentes; el cliente de red no se usa en tests.
- **Aceptación:** `adapters::telegram` compila; ninguna referencia a GCP.
- **Dependencias:** WP-03.
- **Espera:** G0.

### WP-07. Borrado de estrategias y del stack de portfolio (al aprobar)

- **Archivos:** lista completa en la sección 5 de la v4. Resumen:
  - las cuatro estrategias no swarm, con sus entradas en el registro;
  - `portfolio_engine.rs`, `universe_selector.rs`, `universe.rs` y los binarios de portfolio;
  - configs salvo los tres artefactos de swarm.
- **Se conserva** el camino de perpetuos (`domain/src/futures.rs` y el motor de perpetuos) hasta cerrar la prueba A, porque el control lo usa.
- **Aceptación:** `strategy_ids()` devuelve solo `swarm_consensus`; ningún archivo fuera de swarm nombra otra estrategia; tests y goldens en verde.
- **Dependencias:** WP-01, WP-03.
- **Espera:** G0.

---

**Desde aquí, todo espera a GA.**

### WP-08. Convertidor de trades de Kraken a `Trade` (espera GA)

- **Archivos:** `crates/adapters/src/kraken_spot/convert.rs`.
- **Tipos:** `KrakenTradeV2 { symbol, side, price, qty, ord_type, trade_id, timestamp }` y `KrakenTradeRest` (el formato de `Trades` público, por verificar). Ambos se convierten a `domain::Trade` con `Decimal`.
- **Pruebas:** fixtures copiados del texto literal de la documentación (sección 2). Un caso con precio con ceros finales. Un caso de timestamp en microsegundos.
- **Aceptación:** conversión exacta de precio y cantidad; ningún `f64` en el camino.
- **Dependencias:** WP-05.

### WP-09. Cliente público y backfill (espera GA)

- **Archivos:**
  - `crates/adapters/src/kraken_spot/rest_public.rs`: `AssetPairs` y `Trades`.
  - `crates/adapters/src/kraken_spot/backfill.rs`: bucle de paginación con checkpoint.
  - `crates/adapters/src/kraken_spot/error.rs`: `KrakenError` con `thiserror`.
  - Trait `HttpClient` inyectado, para que los tests no toquen la red.
- **Pruebas:**
  - serialización de respuestas con fixtures literales;
  - paginación: `last` alimenta el siguiente `since`, y se para cuando la página llega vacía o `last` no avanza;
  - reanudación tras interrupción con el checkpoint.
- **Ritmo:** pausa de 1 a 2 s entre peticiones públicas, en un limitador propio separado del contador privado.
- **Medición obligatoria antes de prometer duración:** contar las páginas del primer día y medir el tiempo real. La duración de un día es `páginas × pausa`, y el total es ese valor multiplicado por los días del periodo.
- **Salida:** `data/kraken-spot/XBTEUR/trades/AAAA-MM-DD.parquet` con `TapeWriter`.
- **Aceptación:** un día completo descargado y reanudable; medida de páginas y de tiempo registrada.
- **Dependencias:** WP-05, WP-08. Historial disponible: **no verificado**.

### WP-10. Grabadora WebSocket pública: trade y libro (espera GA; empieza en cuanto GA pase)

- **Archivos:**
  - `crates/adapters/src/kraken_spot/ws_public.rs`: `wss://ws.kraken.com/v2`, canales `trade` y `book` con profundidad 25.
  - `crates/adapters/src/kraken_spot/book.rs`: estado del libro y checksum CRC32 sobre los 10 niveles superiores (crate `crc32fast`).
  - `crates/ports/src/depth_stream.rs`: trait `DepthStream`.
- **Reglas de precisión:** precio y cantidad como texto o `Decimal`; nunca `f64`. Un nivel con cantidad cero se borra.
- **Pruebas:**
  - parseo de mensajes de snapshot y de update con fixtures literales;
  - checksum CRC32 con el ejemplo literal de la guía (sección 2);
  - desincronización: un checksum que no coincide dispara la resuscripción y el snapshot;
  - reconexión con backoff, probada con un flujo en memoria.
- **Salida:** `data/kraken-spot/XBTEUR/{trades,book}/AAAA-MM-DD.parquet`.
- **Aceptación:** 7 días seguidos sin huecos sin alertar, y todo fallo de checksum resincronizado.
- **Dependencias:** WP-05, WP-06 (alertas).

### WP-11. Dominio spot (espera GA)

- **Archivos:**
  - `crates/domain/src/instrument.rs`: `MarketType::Spot`, `Instrument` con pares EUR, `Currency`, `Money`.
  - `crates/domain/src/fees.rs`: `FeeTier { maker_rate, taker_rate }` con validación en el rango `[0, 0.05]`.
  - `crates/domain/src/risk.rs`: rechazo de ventas sin posición (`RiskError::ShortNotSupported`), apalancamiento 1, funding nulo, base de riesgo `min(capital operativo, equity EUR)`.
  - `crates/strategies/src/swarm_consensus/mod.rs`: `compatible_market_types` añade `MarketType::Spot`. Es un cambio de una línea, que se hace junto con la prueba de compatibilidad.
- **Pruebas:** venta sin posición rechazada; funding cero; redondeo al tick y al lote de AssetPairs; base de riesgo con capital operativo menor que el equity; breakers sobre la base.
- **Aceptación:** tests en verde; `backtest --spot` acepta el swarm sin atajo.
- **Dependencias:** WP-04.

### WP-12. Prueba B: recalibración y walk-forward (espera GA y los datos de WP-09/WP-10)

- **Pasos:**
  1. Calibrar con `calibrate_feeds` sobre la cinta de Kraken (`swarm_shared` nuevo). El umbral de dólar-barra se fija en EUR, a partir del ritmo de barras que tenga el diseño original.
  2. Walk-forward con `run_hpo --folds N` sobre al menos 6 meses de cinta, con embargo y DSR.
  3. Long-only, comisiones de `TradeVolume`, capital de referencia.
- **Criterio:** PnL neto OOS positivo y DSR que cumple el criterio que fija `run_hpo`. Si no pasa, se para la ejecución (GB no se abre).
- **Salida:** informe con parámetros, folds, PnL neto, comisiones, número de operaciones y DSR.
- **Dependencias:** WP-08, WP-09, WP-10, WP-11.

**Desde aquí, todo espera a GB.**

### WP-13. Firma API-Sign y REST privado (espera GB)

- **Archivos:**
  - `crates/adapters/src/kraken_spot/auth.rs`: `api_sign(path, nonce, postdata, secret)` con `ring` (HMAC-SHA512, SHA256) y `base64`.
  - `crates/adapters/src/kraken_spot/rest_private.rs`: `Balance`, `TradeVolume` (con `pair`), `AddOrder`, `CancelOrder`, `QueryOrders`, `TradesHistory` paginado.
  - `crates/adapters/src/kraken_spot/rate_limit.rs`: contador privado por clave, con el coste de cada llamada.
  - `crates/adapters/src/kraken_spot/fees.rs`: `FeeSource` que lee `TradeVolume`.
- **Decisiones:** cuerpo en formulario, y se firman exactamente esos bytes. Nonce en milisegundos con contador atómico, siempre creciente por clave.
- **Pruebas:**
  - vector de firma copiado del texto literal de la guía (sin valores en este plan);
  - nonce estrictamente creciente bajo concurrencia;
  - codificación del cuerpo;
  - mapeo de errores: firma inválida y límite de tasa;
  - todo con HTTP mockeado.
- **Aceptación:** el vector pasa; tests en verde; ninguna llamada real en tests.
- **Dependencias:** WP-11, WP-12 (GB).

### WP-14. WebSocket privado y `KrakenSpotGateway` (espera GB)

- **Archivos:**
  - `crates/adapters/src/kraken_spot/ws_private.rs`: token con `GetWebSocketsToken`, conexión a `wss://ws-auth.kraken.com/v2`, suscripción a `executions`.
  - `crates/adapters/src/kraken_spot/gateway.rs`: `KrakenSpotGateway` implementa `ExecutionVenue`.
- **Reglas:** renovar el token antes de sus 15 minutos y tras cada reconexión. Un error de caducidad obliga a pedir otro token y a resuscribirse. Un `trade_id` repetido cuenta una vez.
- **Salidas en cliente (D11):** las salidas por take profit y por stop loss las calcula el dominio y se envían como órdenes. El gateway no usa órdenes condicionales.
- **Pruebas:** mensajes de `executions` en un flujo en memoria; caducidad y resuscripción; fills duplicados; mapeo de `EntryAck`.
- **Aceptación:** ciclo completo en simulación con el flujo en memoria; en real, primero órdenes mínimas, según WP-17.
- **Dependencias:** WP-13.

### WP-15. Fidelidad de simulación (espera GB; usa los datos de WP-10)

- **Archivos:** `crates/adapters/src/simulated_venue.rs` (o su sucesor en `simulated/`).
- **Reglas:**
  - las órdenes de mercado recorren el libro grabado;
  - las órdenes límite se llenan solo cuando un trade atraviesa su nivel;
  - las comisiones salen de `FeeSource`;
  - la latencia se mide con `parity_check`.
- **Pruebas:** recorrido de libro sobre un libro sintético; cola de límite; comparación con fills reales en `parity_check`.
- **Aceptación:** `parity_check` con desviación explicada durante al menos 2 semanas de paper.
- **Dependencias:** WP-10, WP-14.

### WP-16. Operación local en Omarchy (espera GB; el ledger puede empezar antes)

- **Archivos:**
  - `crates/adapters/src/storage/sqlite_ledger.rs` (crate `rusqlite` con `bundled`): fills, comisiones, transferencias y el estado de runs.
  - Unidad de usuario `~/.config/systemd/user/atsnt.service` con `WorkingDirectory=%h/atsnt`, `EnvironmentFile=%h/atsnt/.env`, `Restart=on-failure`, `TimeoutStopSec=30s`, `KillSignal=SIGINT`.
  - Timer de usuario para la copia nocturna con `sqlite3 ".backup"`.
  - Configuración de logind (`HandleLidSwitch`, `HandleLidSwitchExternalPower`, `IdleAction` a `ignore`) y revisión de hypridle.
- **Pruebas:** reinicio tras matar el proceso; restauración de una copia en un directorio temporal.
- **Aceptación:** el servicio se reinicia; la copia se restaura; el equipo no se suspende con la tapa cerrada ni por inactividad.
- **Dependencias:** WP-14 para el servicio. El ledger puede empezar antes, con los datos de la prueba.

### WP-17. Paper y dinero real por tramos (espera GP)

- **Paper:** al menos 4 semanas con datos grabados y el simulador de WP-15. El demo de futuros no aplica (no eres apto).
- **Dinero real:** solo si el DSR y el PnL neto OOS son positivos con comisiones de spot, y la paridad está en tolerancia.
- **Capital:** se pregunta cuando GP pase. Hasta entonces queda como parámetro `operating_capital_eur`.
- **Aceptación:** dos semanas de operación real sin incidentes de conciliación.

## 4. Grafo de dependencias

```text
G0 ─► WP-00, WP-01, WP-02 ─► WP-03 ─► WP-04, WP-05, WP-06 ─► WP-07

GA ─► WP-08 ─┐
     WP-09 ──┤  (con WP-05)
     WP-10 ──┤  (empieza ya; libro no recuperable)
     WP-11 ──┘
              └─► WP-12 (prueba B) ─► GB ─► WP-13 ─► WP-14 ─► WP-16
                                       WP-15 (usa WP-10) ─┘
                                                           └─► GP ─► WP-17
```

## 5. Riesgos específicos de la implementación

| Riesgo | Impacto | Mitigación |
|---|---|---|
| El libro no se puede reconstruir hacia atrás | Alto | WP-10 empieza en cuanto pasa GA |
| Calibrar con datos de Binance da umbrales equivocados en EUR | Alto | Recalibrar en WP-12 |
| Profundidad histórica de `Trades` insuficiente | Alto | Medir antes de prometer; fuente alternativa si hace falta |
| Desajuste de codificación al firmar | Medio | Un solo formato (formulario) y vector literal |
| Checksum del libro no coincide por precisión | Medio | Texto o `Decimal`, nunca `f64`; comparar con mensajes en vivo |
| Límite de tasa más bajo del previsto | Medio | Medir con la clave real; tu nivel de verificación |
| Clave de API comprometida | Alto | Sin retirada; `.env` con permisos `600` |

## 6. Lo que necesito de ti

1. **Aprobar este plan** (G0). Si quieres cambiar algo, dime qué.
2. **Permitir `docs.kraken.com`** en la configuración de red de la sesión, siguiendo la ruta que indiqué antes. Mientras tanto, puedes pegar en el chat las secciones de autenticación (con su ejemplo) y el ejemplo de checksum del libro. Son los dos textos literales que faltan.
3. **Ejecutar la prueba A** con los comandos de la v4, y mandarme el resumen de los tres logs.
4. **Tu nivel de verificación** en Kraken, y los permisos reales de la clave de spot cuando la crees.
