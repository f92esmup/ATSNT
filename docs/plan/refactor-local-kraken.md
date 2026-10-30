# Plan de refactorización: ATSNT en local, swarm sobre Kraken en EUR

> **Estado:** propuesta lista para ejecutar. Todavía no hay cambios de código.
> **Base:** commit `50b4ee5` en `claude/awesome-lamport-ch9ytr`. Antes de escribir este plan se verificó en verde: `cargo check --workspace --all-targets`, `cargo test --workspace`, `cargo fmt --check` y `cargo clippy --all-targets -- -D warnings`.
> **Fecha:** 2026-10-30.

## 0. Resumen

**Objetivo.** Un sistema que corre en tu equipo, sin GCP, que graba datos de Kraken, hace backtest, paper trading y opera swarm en EUR. Spot primero y futuros después.

**Lo que este plan no hace.** No crea ventaja. El logbook de swarm (`docs/research/01-swarm-consensus-optimization-logbook.md`) ya es negativo con comisiones de 0,05 %. Con comisiones de Kraken la fricción sube entre 4 y 8 veces (sección 2.4). Por eso la fase 1 es una prueba económica barata, con el código actual, antes de construir el adaptador.

**Orden de trabajo.**

1. **Fase 0:** higiene y secretos.
2. **Fase 1:** puerta económica con el código actual. Es la primera decisión de negocio.
3. **Fase 2:** grabadora de Kraken, en paralelo desde el primer día. Los trades se pueden pedir hacia atrás por REST; el libro L2 no.
4. **Fases 3 y 4:** consolidación hexagonal y eliminación de estrategias, protegidas por goldens de swarm.
5. **Fases 5 a 7:** dominio EUR, spot y futuros; adaptador Kraken; fidelidad de simulación.
6. **Fases 8 y 9:** operación local y paso a capital real por tramos.

## 1. Decisiones por defecto

Tomo estas decisiones para avanzar. Si quieres otra, dímelo y ajusto las fases afectadas.

| # | Decisión | Motivo | Cómo cambiarla |
|---|---|---|---|
| D1 | Solo `swarm_consensus`. Se eliminan `dollar_bars_cusum`, `order_flow_imbalance`, `langevin_potential`, `quantum_well_swing` y el stack de portfolio y universo | Tu instrucción. El VM con el bot ya no existe, así que no hay despliegue que proteger | Volver a registrar una estrategia en `registry.rs` |
| D2 | Spot antes que futuros | Kraken Futures es otra API (`futures.kraken.com`, WebSocket v1) y su disponibilidad depende del país | Cambiar el orden de las fases 5 y 6 |
| D3 | Spot solo largo | Sin margen no hay venta en corto | Modelar margen (fuera de alcance) |
| D4 | Swarm en modo `classifier` para paper y live | El modo legacy emite `Decision::Target` y el registro lo rechaza (`crates/backtest/src/registry.rs:138`) | Ninguna: es una restricción actual del código |
| D5 | Binance deja de ser fuente de datos y de ejecución al cerrar la fase 1 | Su único uso restante es el histórico público de la fase 1 | Mantener un descargador público si quieres comparar venues |
| D6 | Parquet para trades y libro; SQLite para el registro de runs | Sin servidor ni cloud. SQLite cubre lo que hoy hace BigQuery | DuckDB para análisis, sin cambiar el diseño |
| D7 | Telegram se mantiene | No es GCP y ya es un puerto | — |
| D8 | Linux con unidad systemd de usuario | El repo ya trae `docs/deployment/*.service`, `journald.conf` y envoltorios `systemd-inhibit` | Si usas macOS, launchd; las fases no cambian |
| D9 | Se añade el puerto `Clock` | AGENTS.md lo exige; el roadmap lo descartó. Se corrige el roadmap | — |
| D10 | Capital inicial en EUR, en configuración (500 EUR de partida) | Es tu presupuesto. Ninguna cifra en USDT en el código | — |
| D11 | Salidas gestionadas por el cliente (en dominio) | No he verificado órdenes condicionales en AddOrder | Usar órdenes condicionales si la documentación lo confirma |

## 2. Hechos que condicionan el plan

### 2.1 GCP y datos

- El proyecto se eliminó. Google permite restaurar un proyecto eliminado durante 30 días con `gcloud projects undelete` ([documentación](https://docs.cloud.google.com/resource-manager/docs/delete-restore-projects)). No lo necesitamos para este plan.
- Lo que se pierde es el historial de runs de BigQuery y cualquier artefacto de GCS que no se descargó a `configs/`. Lo que importa sigue en git: `configs/hpo_swarm.json`, `configs/hpo_btcusdt_2026.json`, `configs/swarm_shared.json` y los logbooks de `docs/research/`.
- Los datos históricos de Binance son públicos: `fetch_data` los vuelve a descargar.

### 2.2 Secretos

- Las claves de API de Binance siguen activas aunque Secret Manager ya no exista. **Revócalas en la consola de Binance** si ya no las usas.
- El token de Telegram estaba en Secret Manager. Recréalo con BotFather y guárdalo en `.env` con permisos `600`.

### 2.3 Swarm tiene dos modos, y solo uno se puede operar

| Modo | Config | Decisión que emite | Artefacto de referencia | Paper y live |
|---|---|---|---|---|
| Legacy | `classifier_mode: false` (valor por defecto) | `Decision::Target`, niveles de -3 a 3 | `configs/hpo_swarm.json` (no tiene la clave, así que carga en legacy) | Rechazado por el registro |
| Classifier | `classifier_mode: true` | `Decision::Entry`, barrera triple | `configs/hpo_btcusdt_2026.json` | Aceptado |

El candidato para operar es el modo classifier. `hpo_swarm.json` sirve como referencia del backtester.

### 2.4 Comisiones: el punto que decide el plan

El logbook (`docs/research/01-…`) usó comisión taker de 0,05 % y slippage de 0,05 %. Resultado en 271 operaciones: PnL bruto −40,66 USD, fricción 91,69 USD y PnL neto −85,89 USD.

Comisiones de Kraken spot que aparecen en las fuentes consultadas. **No coinciden entre sí; verificar en tu cuenta:**

- Tramo base de 2026: 0,40 % maker y 0,80 % taker ([comparativa](https://www.kraken.com/learn/lowest-fee-crypto-exchange), [tarifas Kraken Pro](https://www.kraken.com/pro/fees)).
- Páginas más antiguas: 0,25 % maker y 0,40 % taker.
- Desde el 9 de julio de 2026 el nivel se fija por volumen de 30 días **o** por activos en la plataforma, el que dé mejor tarifa ([cambio de niveles](https://support.kraken.com/articles/cross-platform-fee-tier-changes)).

**Estimación de orden de magnitud** (no es un backtest; la fase 1 lo confirma). Suponiendo 0,05 % de slippage por lado y que la fricción registrada crece con la comisión taker:

- Con 0,40 % taker: (0,40 + 0,05) / (0,05 + 0,05) = 4,5 veces la fricción. Son unos 410 USD frente a 92 USD.
- Con 0,80 % taker: (0,80 + 0,05) / 0,10 = 8,5 veces. Son unos 780 USD.

Todo ello frente a un PnL bruto de −41 USD. Con estas cifras, swarm no es rentable en spot sin mejorar mucho la señal o sin entradas maker.

### 2.5 Kraken no existe en el código

No hay ninguna referencia a Kraken. El adaptador de datos, de firma y de ejecución es Binance USD-M. Todo lo de Kraken se construye desde cero.

### 2.6 Violaciones hexagonales (verificadas con grep)

| Dónde | Problema | Acción |
|---|---|---|
| `backtest/src/{paper,tick_simulation,recording,parity}.rs` | Importan `adapters::` desde la capa de aplicación | Dependen de puertos; el cableado va a `wiring.rs` (fase 3) |
| `backtest/src/{sinks,artifact}.rs` | Sinks de BigQuery y GCS | Se eliminan (fases 3 y 4) |
| `adapters/src/analytics.rs` | Define el trait de persistencia en la capa de adaptadores | Se mueve a `ports/` (fase 3) |
| `ports/src/execution.rs`, `ports/src/lib.rs` | Nombran Binance en comentarios | Se corrige (fase 3) |
| `domain/src/futures.rs` | Lógica pura correcta, pero el intervalo de funding está fijado en 8 h | Se parametriza (fase 3) |
| `backtest/src/{daily_watermark,excursion,fee_ledger,monthly_guard,execution_core,metrics,walk_forward,price_event}.rs` | Reglas puras dentro de la capa de aplicación | Se mueven a `domain` si el grep confirma que no usan E/S (fase 3) |
| `backtest/src/monte_carlo.rs` | El grep de E/S lo marca por `rayon` | Separar: el núcleo estadístico va a `domain` con RNG determinista; el paralelismo queda en la aplicación |
| `backtest/src/{driver,session}.rs` | Orquestación | **Se quedan en la capa de aplicación.** No son dominio |
| `backtest/src/live_exits.rs` | Salidas gestionadas por el exchange, específicas de Binance | Se revisa con la decisión D11 (fase 6) |

### 2.7 Unidades de los campos de comisión

Los campos `*_pct` son fracciones: `taker_fee_pct: 0.0005` equivale a 0,05 %. Un 0,40 % escrito como `0.40` serían 40 %. Se renombran a `*_rate`, se validan al cargar en el rango `[0, 0.05]` y se migran los artefactos que se conservan.

## 3. Arquitectura objetivo

```text
crates/
├── domain/      Puro, sin E/S. Instrumentos (spot y futuros), moneda, tarifas, órdenes y
│                su máquina de estados, posición, riesgo, contabilidad (comisiones, drawdown
│                diario y mensual), analítica (métricas, Monte Carlo con RNG determinista),
│                funding con intervalo parametrizado.
├── strategies/  Solo swarm_consensus, su contrato de pruebas y un registro con una entrada.
├── ports/       MarketDataStream, AsyncMarketDataStream, DepthStream*, TradeTapeWriter*,
│                Clock*, ExecutionVenue, ExitGateway, InstrumentCatalog*, FeeSource*,
│                trait de persistencia de runs (movido), AlertNotifier, FxRateSource*
│                (solo futuros en USD).                                  (* = nuevo)
├── adapters/    kraken/    REST, WebSocket, firma, catálogo, gateway, grabadora
│                storage/   Parquet de trades y libro; SQLite del registro de runs
│                telegram/  (movido desde gcp/)
│                simulated/ SimulatedVenue y libro simulado
└── backtest/    (se renombra a `app` al final) TradingSession, driver, simuladores y modos
                 backtest, hpo, paper, live y record. Solo `main.rs` y `wiring.rs` nombran adapters.
```

Reglas que pasan a ser tests en `crates/backtest/tests/architecture_contract.rs`:

- **R1.** `domain`, `strategies` y `ports` no dependen de crates de E/S. (Ya existe.)
- **R2.** `adapters` no depende de la capa de aplicación.
- **R3.** Los módulos de librería de la aplicación no importan `adapters::`. Solo `main.rs` y `wiring.rs`.
- **R4.** `ports` no nombra exchanges ni formatos: nada de `Binance`, `Kraken`, `Parquet` ni `BigQuery`.
- **R5.** `domain` no usa `f32` ni `f64`, salvo en los módulos de estadística que se listen explícitamente.
- **R6.** Solo `strategies::registry` nombra estrategias. (Ya existe.)
- **R7.** Ningún `SystemTime::now()` ni `Utc::now()` fuera de `adapters`. El tiempo llega por `Clock`.

## 4. Fases

Tamaño relativo: S, M o L. Cada fase tiene criterio de salida y no se pasa a la siguiente sin cumplirlo.

### Fase 0: higiene (S)

- Revocar las claves de Binance y recrear el token de Telegram (sección 2.2).
- Crear `.env` con permisos `600`, con las claves de Kraken y las variables de Telegram que use el código.
- Confirmar el sistema operativo. Si no es Linux, decirlo antes de la fase 8.

**Salida:** `.env` local creado y claves de Binance revocadas.

### Fase 1: puerta económica con el código actual (S de código, horas de cómputo)

**Pregunta:** ¿swarm sobrevive a las comisiones de Kraken? Se responde antes de construir nada de Kraken.

1. Descargar BTCUSDT de 2026 (varios meses) con `fetch_data` (README, paso 1). Binance es un proxy: sirve para medir la sensibilidad a comisiones, no el precio de Kraken. El artefacto es de perpetuos, pero lo que importa aquí es la sensibilidad a comisiones.
2. Crear copias de `configs/hpo_btcusdt_2026.json` que cambien solo `execution_context`. Poner `funding_rate_pct` a `0` en las copias, porque en spot no hay funding.

| Escenario | `maker_fee_pct` | `taker_fee_pct` |
|---|---|---|
| A. Control (configuración actual) | 0.0002 | 0.0005 |
| B. Kraken tramo antiguo | 0.0025 | 0.0040 |
| C. Kraken tramo base de 2026 | 0.0040 | 0.0080 |

3. Ejecutar cada escenario: `cargo run --release -p backtest -- --data <parquet> --config <copia>`. Anotar el PnL neto, las comisiones totales, el Sortino y la fracción de señales cortas.
4. Repetir con `configs/hpo_swarm.json` solo como referencia del backtester.

**Criterio:**

- **Pasa** si en el escenario C el PnL neto OOS es positivo y el DSR cumple el criterio del proyecto. El umbral lo fija `run_hpo`; verificarlo en el código antes de decidir.
- **No pasa.** No se construye la ejecución real con swarm. El refactor sigue, porque elimina código sin uso, pero la siguiente decisión es de investigación de estrategia, no de infraestructura.
- **Medir siempre la fracción de señales cortas.** Si es alta, spot solo largo (D3) no puede expresar la estrategia.

**Salida:** tabla de escenarios completa y decisión tomada.

### Fase 2: grabadora de Kraken (M), en paralelo desde el día 1

**Por qué va primero:** los trades se pueden pedir por REST hacia atrás; el libro L2 no. Sin grabación propia no hay calibración del libro ni validación de fills. Cada día sin grabar es un día de datos perdido.

Alcance:

- Puertos mínimos que necesita la grabadora: `DepthStream`, `TradeTapeWriter` y `Clock`.
- `adapters/src/kraken/ws_public.rs`: WebSocket v2 en `wss://ws.kraken.com/v2`, canales `trade` y `book` con profundidad 25. El checksum CRC32 de los 10 niveles superiores valida el libro. Si falla, se vuelve a suscribir ([canal book](https://docs.kraken.com/api/docs/websocket-v2/book), [guía de checksum](https://docs.kraken.com/api/docs/guides/spot-ws-book-v2/)).
- `adapters/src/kraken/catalog.rs`: AssetPairs para tick, mínimos y nombres de par. Verificar si v2 usa `XBT/EUR` o `BTC/EUR`.
- Persistencia en Parquet por día UTC: `data/kraken/<par>/trades/AAAA-MM-DD.parquet` y `data/kraken/<par>/book/AAAA-MM-DD.parquet`, con un snapshot cada 60 s más los deltas.
- Reconexión con backoff exponencial, heartbeat y rotación de ficheros a medianoche UTC.
- Salud cada hora (mensajes por segundo, huecos, reconexiones, fallos de checksum). Alerta por Telegram si hay un hueco de más de 60 s.
- Binario temporal `kraken_record`. Pasa a `atsnt record` en la fase 10.

**Salida:** 7 días seguidos sin huecos sin alertar, y todo fallo de checksum resincronizado.

### Fase 3: consolidación hexagonal con goldens de swarm (L)

Orden obligatorio: primero los goldens, después los movimientos.

- **3.1 Goldens de swarm.** Añadir casos de swarm (bar y tick, en modo classifier y en legacy) en `crates/backtest/tests/golden_execution.rs`. Generarlos con `UPDATE_GOLDEN=1 cargo test -p backtest --test golden_execution`, revisar el diff y commitear. Hoy los goldens de estrategia son de cusum y ofi; esos se borran en la fase 4. Los de registro se conservan.
- **3.2 Reglas R2 a R7** en `architecture_contract.rs`. Fallarán hasta completar 3.3 a 3.8.
- **3.3 Persistencia.** Mover el trait de persistencia de `adapters/src/analytics.rs` a `ports/src/`. `recording.rs` queda como traducción del dominio al puerto.
- **3.4 Sin `adapters::` en la librería.** `paper.rs`, `tick_simulation.rs` y `parity.rs` reciben puertos genéricos. El cableado concreto se mueve a `wiring.rs`.
- **3.5 Reglas puras al dominio.** `daily_watermark`, `excursion`, `fee_ledger`, `monthly_guard`, `execution_core`, `metrics` y `walk_forward` si no tiene E/S. Antes de mover cada módulo, comprobar con grep que no contiene `std::fs`, `tokio`, `rayon`, `rand` ni `SystemTime`.
- **3.6 Funding.** `FundingSchedule::new(intervalo, tasa)`. La constante de 8 h queda solo en el perfil histórico de Binance, que se borra en la fase 4.
- **3.7 Clock.** `SystemClock` en `adapters`; `SimClock` para simulación y tests. Corregir `docs/ROADMAP.md`, que dice que el puerto no hace falta.
- **3.8 Puertos sin nombres de exchange.** Quitar las menciones de Binance en `ports/src/execution.rs` (`order_id`, documentación de `ExecutionVenue`) y en `ports/src/lib.rs`.
- **3.9 Renombrar `tape`.** `binance_parquet.rs` pasa a `storage/tape_parquet.rs`, genérico, con el mismo esquema.

**Salida:** `fmt`, `clippy -D warnings` y tests en verde. Goldens idénticos bit a bit. R2 a R7 pasan.

### Fase 4: eliminar estrategias y el stack de portfolio (M)

La lista completa está en la sección 5. Incluye el código de Binance que ya no se usa: gateway, streams, firma, fixtures y el descargador histórico, que solo sirvió para la fase 1.

**Salida:** `strategy_ids()` devuelve solo `swarm_consensus`. Ningún archivo fuera de swarm nombra otra estrategia (grep vacío). Tests en verde.

### Fase 5: dominio de instrumento, EUR, spot y futuros (M)

- `domain/src/instrument.rs`: `MarketType { Spot, LinearFuture }`, `Currency` (newtype con `EUR`, `USD`, `USDT`, …) e `Instrument` con tick, paso de lote, cantidad mínima y coste mínimo. Todo en `Decimal`.
- `domain/src/fees.rs`: `FeeTier { maker_rate, taker_rate }` como datos, con validación de rango.
- **Spot.** `RiskPolicy` rechaza una venta sin posición (`RiskError::ShortNotSupported`). Apalancamiento fijo en 1. Funding nulo.
- **Futuros.** `FundingSchedule` con intervalo configurable. Las páginas regionales de Kraken Pro muestran funding horario para Reino Unido y Europa ([página](https://www.kraken.com/gb/pro/perps/crypto-perpetuals/funding-rates)); verificarlo en la demo. Los perpetuos lineales son los `PF_`; confirmar el símbolo en el listado de instrumentos.
- **EUR.** Capital, pérdidas y breakers en EUR. En futuros con liquidación en USD, `FxRateSource` convierte. En spot EUR no hace falta conversión.
- Renombrar `*_pct` a `*_rate` (sección 2.7) y migrar los artefactos que se conservan.

**Pruebas:** venta en corto rechazada en spot; funding nulo en spot; redondeo al tick y al lote; conversión a EUR determinista con tasa fija.

**Salida:** tests de cada regla en verde. La configuración nueva se valida al cargar.

### Fase 6: adaptador Kraken spot (L)

- **6.1 Catálogo.** AssetPairs se convierte en `Instrument`. Antes de programar, verificar en la referencia de AssetPairs los nombres exactos de los campos de tick, mínimo de orden, mínimo de coste y decimales.
- **6.2 Histórico por REST.** `Trades` con `since` en nanosegundos y `count` de hasta 1000. Se avanza con `last`. Pausa de 1 a 2 s entre peticiones según la [guía de datos históricos](https://docs.kraken.com/exchange/guides/general/historical-data). La misma guía tiene además una recomendación de pausa más corta; usar el valor mayor. Guardar un checkpoint con el último `last` para reanudar. Medir cuántas horas tarda un mes. Si Kraken ofrece archivos completos de trades son preferibles; no lo he confirmado.
- **6.3 Firma.** `API-Sign` = base64( HMAC-SHA512( clave = base64decode(secret), mensaje = ruta + SHA256(nonce + postdata) ) ). La ruta empieza en `/0/private` e incluye la query. El nonce debe crecer por clave: milisegundos más un contador protegido. Usar `ring`, que soporta HMAC-SHA512. Vector de prueba: la muestra con firma esperada de la [guía de autenticación REST](https://docs.kraken.com/api/docs/guides/spot-rest-auth/), copiada como fixture.
- **6.4 REST privado.** Balance, TradeVolume (tarifa real de tu nivel), AddOrder, CancelOrder, ClosedOrders o QueryOrders, y TradesHistory paginado para conciliar.
- **6.5 WebSocket privado.** Token de 15 minutos con `GetWebSocketsToken` y canal de ejecuciones ([autenticación WebSocket](https://docs.kraken.com/exchange/guides/websockets/authentication)).
- **6.6 `KrakenGateway`** implementa `ExecutionVenue` (`submit_entry`, `submit_order`) y entra en el `TradingSession` existente. `live_trading.rs` se reescribe con él.
- **6.7 Límites de tasa.** No he encontrado cifras cerradas para todos los endpoints. Medirlos con tu cuenta y limitar por endpoint con un cubo de fichas conservador. Respetar la pausa de 1 a 2 s de 6.2.
- **6.8 Pruebas.** HTTP y WebSocket mockeados: AGENTS.md prohíbe la red en tests. No he localizado un sandbox público de spot. Si no existe, la validación es paper con la grabadora y órdenes reales mínimas.

**Salida:** tests de firma y de checksum en verde con los vectores oficiales. Paper completo usando los datos de la fase 2.

### Fase 7: fidelidad de simulación (L)

AGENTS.md exige fills realistas. Hoy el simulador no tiene libro de órdenes (ver los "known gaps" del roadmap).

- `SimulatedVenue`: las market orders recorren el libro L2 grabado. Las limit orders usan un modelo de cola y solo se ejecutan cuando un trade atraviesa el nivel.
- Latencia medida con `parity_check` sobre órdenes reales mínimas.
- `FeeSource` lee la tarifa de la cuenta, y la simulación y la ejecución usan la misma.
- Cierra los gaps de precio de marca y de spread que documenta el roadmap.

**Salida:** `parity_check` con desviación explicada durante al menos 2 semanas de paper.

### Fase 8: operación local (M)

Estructura en tu equipo:

```text
~/atsnt/
├── .env                 (600) claves de Kraken y de Telegram
├── configs/             artefactos versionados en git
├── data/kraken/…        Parquet de trades y libro
├── state/atsnt.sqlite   registro de runs (WAL)
└── backup/              copias nocturnas
```

- **Registro.** SQLite con `runs`, `run_metrics`, `trades` y `run_series`, con la misma forma que el contrato actual, y vistas `v_runs`, `v_trades` y `v_equity`. Reescribir `docs/architecture/13-bi-data-contract.md`, que hoy describe BigQuery.
- **Servicio.** Unidad de usuario `~/.config/systemd/user/atsnt.service` con `Restart=on-failure` y `KillSignal=SIGINT`, como la actual. `loginctl enable-linger` para que arranque sin sesión abierta. Desactivar la suspensión del equipo; requiere permisos de administrador y depende de tu sistema operativo.
- **Copias.** `sqlite3 state/atsnt.sqlite ".backup …"` cada noche, y copia de `data/` a un disco externo. Medir el tamaño tras la fase 2.
- **Alertas.** Telegram se mantiene y se mueve a `adapters/telegram/`.
- **Informes.** El subcomando `atsnt report` sustituye las consultas de BigQuery.
- **Borrar.** `Dockerfile`, `.dockerignore`, `scripts/gcp/`, `docs/architecture/10-gcp-cloud-architecture.md`, las variables `GCP_*` y los flags `--gcp-bigquery`, `--strict-sinks` y `--upload-winner`.

**Salida:** el servicio se reinicia tras matar el proceso. Una copia nocturna se restaura en una prueba. Una parada genera alerta.

### Fase 9: paper y capital real por tramos (calendario, no código)

- Paper en Kraken durante al menos 4 semanas, con los datos de la fase 2.
- Pasar a real solo si el DSR cumple, el PnL neto OOS es positivo con comisiones reales y la paridad está dentro de tolerancia.
- Tramo inicial en el mínimo de orden del par (dato de AssetPairs). Escalar según `docs/capital-scaling.md`, reescrito en EUR. Breakers diario y mensual activos.

**Salida:** dos semanas de operación real sin incidentes de conciliación.

### Fase 10: limpieza final (S, opcional)

- Consolidar binarios en `atsnt` con subcomandos: `record`, `fetch`, `calibrate`, `backtest`, `hpo`, `prune`, `paper`, `live`, `parity` y `report`.
- Renombrar el crate `backtest` a `app`.

## 5. Inventario de borrado

| Ruta | Acción | Fase |
|---|---|---|
| `crates/strategies/src/{dollar_bars_cusum,order_flow_imbalance,langevin_potential,quantum_well_swing}.rs` | Borrar, con sus `pub mod`, `pub use` y entradas del registro | 4 |
| `crates/domain/src/universe.rs` | Borrar | 4 |
| `crates/backtest/src/{portfolio_engine,universe_selector,sinks}.rs` | Borrar | 4 |
| `crates/backtest/src/artifact.rs` | Quitar la parte de GCS y conservar el cargador de configuración | 3 |
| `crates/backtest/src/bin/{run_portfolio,run_portfolio_hpo,paper_trading_portfolio,run_universe_portfolio,testnet_exit_check}.rs` | Borrar | 4 |
| `crates/backtest/src/bin/generate_pipeline_test_data.rs` | Borrar si solo lo usan los scripts borrados | 4 |
| `crates/adapters/src/binance_*.rs`, `binance_gateway/`, `binance_fetcher/`, `fetch_plan.rs` | Borrar. `binance_parquet.rs` se renombra antes (3.9) | 4 |
| `crates/adapters/src/bin/{fetch_data,stream_trades,live_gateway}.rs` | Borrar; sustituidos por `kraken_record` y el gateway de Kraken | 4 |
| `crates/adapters/tests/fixtures/binance_private_events/` | Borrar | 4 |
| `crates/backtest/src/bin/live_trading.rs` | Reescribir con `KrakenGateway` | 6 |
| `crates/adapters/src/gcp/*` salvo `telegram.rs` | Borrar; mover `telegram.rs` a `adapters/src/telegram/` | 3 |
| `crates/adapters/src/analytics.rs` | Mover el trait a `ports/` | 3 |
| `crates/backtest/tests/golden/*cusum*`, `*ofi*` y `hpo_cusum_ranking.json`, con sus casos | Borrar | 4 |
| Tests que prueban código borrado (`strict_sinks_cli.rs`, partes de `recording_contract.rs`) | Borrar, o reescribir con su código | 4 |
| `configs/` salvo `hpo_swarm.json`, `hpo_btcusdt_2026.json` y `swarm_shared.json` | Borrar (daytrading, langevin, quantum, matrix, portfolio, universe y execution_*). Añadir `kraken_spot_eur.json` | 4 y 5 |
| `configs/hpo_results.json` | Revisar su contenido antes de decidir | 4 |
| `scripts/gcp/`, `scripts/run_production_pipeline.sh`, `scripts/run_joint_portfolio_pipeline.sh`, `scripts/tests/` | Borrar; los sustituye `atsnt` | 8 |
| `Dockerfile`, `.dockerignore` | Borrar | 8 |
| `docs/strategy/{01,02,04,05}*.md`, `docs/research/02-*`, `SWARM_OPTIMIZATION_ROADMAP.md` | Mover a `docs/archive/` | 4 |
| `docs/architecture/10-gcp-cloud-architecture.md` | Borrar | 8 |
| `docs/architecture/{07,08,13}*.md`, `docs/capital-scaling.md`, `docs/deployment/atsnt.service`, `README.md`, `AGENTS.md`, `docs/ROADMAP.md`, `.env.example` | Reescribir | 3, 5 y 8 |
| `odd/tasks/` | Conservar como historial | — |

## 6. Pruebas y puertas por fase

| Fase | Pruebas nuevas | Puerta de salida |
|---|---|---|
| 1 | Ninguna; es medición | Tabla de escenarios completa |
| 2 | Parseo de mensajes WebSocket con fixtures; checksum CRC32 con el vector de la guía | 7 días sin huecos sin alertar |
| 3 | Goldens de swarm; reglas R2 a R7 | Goldens idénticos bit a bit |
| 4 | `strategy_ids()`; grep de nombres de estrategias | Tests en verde y sin referencias residuales |
| 5 | Short en spot, funding en spot, tick y lote, conversión a EUR | Cada regla con test |
| 6 | Firma con vector oficial; paginación con HTTP mockeado | Paper completo con datos reales |
| 7 | Fills de market contra libro sintético; cola de limit | `parity_check` dentro de tolerancia |
| 8 | Reinicio tras matar el proceso; restauración de una copia | Alerta al parar |
| 9 | — | Criterios de la fase 9 |

En todas las fases: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` y `cargo test --workspace`. Ningún test toca la red.

## 7. Riesgos

| Riesgo | Impacto | Mitigación |
|---|---|---|
| Swarm no es rentable con las comisiones de Kraken | Alto: no hay producto | Fase 1 antes de construir. Sin OOS positivo y DSR, no hay dinero real |
| La tarifa de la cuenta es distinta de la supuesta | Medio | `FeeSource` la lee de la cuenta; alerta si cambia |
| La grabadora se para (equipo dormido, red, disco) | Alto: se pierden datos del libro | Alertas de hueco, copias y servicio con reinicio |
| El libro se desincroniza | Medio | Checksum y resincronización con snapshot |
| El backfill por REST es demasiado lento | Medio | Grabadora primero; medir y priorizar los meses recientes |
| El refactor cambia el comportamiento | Alto | Goldens antes de mover |
| Clave API comprometida | Alto | `.env` con permisos `600`; sin permiso de retirada; restricción por IP si la cuenta lo permite |
| Cambios en la API de Kraken | Bajo o medio | Vectores oficiales en tests; revisar el changelog antes de cada despliegue |
| Señales cortas que spot no puede expresar | Medio | Medirlo en la fase 1 |

## 8. Fuentes

- Restaurar proyectos de GCP: https://docs.cloud.google.com/resource-manager/docs/delete-restore-projects
- Comisiones: https://support.kraken.com/articles/cross-platform-fee-tier-changes · https://www.kraken.com/pro/fees · https://www.kraken.com/learn/lowest-fee-crypto-exchange
- Histórico y Trades REST: https://docs.kraken.com/exchange/guides/general/historical-data · https://docs.kraken.com/api/docs/rest-api/get-recent-trades/
- WebSocket v2, canales trade y book: https://docs.kraken.com/api/docs/websocket-v2/trade · https://docs.kraken.com/api/docs/websocket-v2/book · https://docs.kraken.com/api/docs/guides/spot-ws-book-v2/
- Autenticación: https://docs.kraken.com/api/docs/guides/spot-rest-auth/ · https://docs.kraken.com/exchange/guides/websockets/authentication
- Funding, página regional: https://www.kraken.com/gb/pro/perps/crypto-perpetuals/funding-rates

Todas las cifras de Kraken de este documento proceden de búsquedas y deben verificarse en tu cuenta y en la documentación vigente antes de usarse.
