# Plan de refactorización v4: ATSNT local, swarm en spot de Kraken

> **Estado:** propuesta v4. Sustituye a la v3.2 (commit `010cf57`). La v3.2 queda en el historial por si cambia la elegibilidad para futuros.
> **Implementación detallada:** [`implementacion-kraken-spot.md`](implementacion-kraken-spot.md), para aprobar. Define los paquetes de trabajo, las puertas y lo que espera a cada una.
> **Base de código:** commit `50b4ee5`. Verificado en verde antes de escribir este plan: `cargo check --workspace --all-targets`, `cargo test --workspace`, `cargo fmt --check` y `cargo clippy --all-targets -- -D warnings`.
> **Fecha:** 2026-10-30. Solo documentación: el código no ha cambiado.
> **Estado (2026-10-31):** lote 1 y lote 2 en `claude/awesome-lamport-ch9ytr` y en `main` (tras el push). Ver [`docs/ROADMAP.md`](../ROADMAP.md) y [`HANDOFF.md`](../../HANDOFF.md).

## Qué vuelve y qué sale respecto a la v3.2

**Vuelve al alcance (spot):**
- Catálogo de pares (AssetPairs) para tamaños, ticks y mínimos.
- Firma API-Sign de spot, con el vector de la guía de autenticación REST.
- WebSocket v2 de spot: canales `trade` y `book`, con checksum CRC32 sobre los 10 niveles superiores.
- Histórico por REST `Trades`: `since` en nanosegundos, 1000 por página, pausa de 1 a 2 s.
- Pares en EUR.

**Sale del alcance:**
- Perpetuos `PF_`, mark price, funding y liquidación.
- Buffer de USD, FX y colateral en EUR.
- Firma Authent, demo de futuros y el test MiFID II.

## 0. Resumen

**Objetivo.** Solo si la prueba económica pasa: un bot local que opera swarm en spot de Kraken con pares EUR, sin apalancamiento, sin Kraken Prop y sin GCP.

**Lo que este plan no hace.** No crea ventaja. La ida y vuelta en spot cuesta entre 0,65 % y 0,80 % del nocional, unas 8 a 9 veces la del clasificador en perpetuos (sección 2.6). La prueba probablemente fallará. Si falla, se para todo lo que es ejecución y el siguiente paso es investigar la estrategia.

**Orden de trabajo.**

1. **Fase 0:** higiene.
2. **Fase 1:** prueba económica spot, con el código actual y datos que descargas tú (sección 4).
3. **Fases 3 y 4:** refactor hexagonal y borrado. No dependen del resultado de la prueba.
4. **Fases 2 y 5 a 9:** grabadora, dominio spot, adaptador de Kraken, fidelidad, operación y dinero real. Solo si la prueba pasa.

## 1. Decisiones

| # | Decisión | Motivo |
|---|---|---|
| D1 | Solo `swarm_consensus`. Se eliminan las otras cuatro estrategias y el stack de portfolio y universo | Tu instrucción |
| D2 | Spot de Kraken en cuenta estándar con API | Tu cuenta es de spot y no eres apto para futuros |
| D3 | Solo largo. El artefacto clasificador `hpo_btcusdt_2026.json` ya tiene `shared_config.allow_short: false`. El legacy `hpo_swarm.json` tiene `allow_short: true` | Spot no permite vender sin tener el activo |
| D4 | Pares EUR (nombres a verificar en AssetPairs). Sin buffer de USD ni FX en margen. El FX solo se usa en informes | Spot en EUR evita todo lo que añadía el colateral de futuros |
| D5 | Entradas de mercado, como en el motor. Salidas por take profit en maker cuando la orden descansa | Coincide con `resting_maker_exits` del artefacto |
| D6 | Sin apalancamiento, sin funding y sin liquidación | Propios de spot |
| D7 | Binance sale del código tras la prueba. Sus datos se descargan una vez, solo para la prueba | Sin uso posterior |
| D8 | Parquet para trades, libro y EURUSD; SQLite para el registro y el ledger | Sin servidor ni cloud |
| D9 | Telegram, Linux (Omarchy), unidad systemd de usuario y puerto `Clock` | Sin cambios respecto a la v3.2 |
| D10 | Base de riesgo = `min(capital operativo, equity EUR)`. Sin apalancamiento, el tope de nocional es igual a la base | Capital propio; se deja como parámetro hasta que la prueba pase |
| D11 | Comisiones de la cuenta, leídas con `TradeVolume` (con `pair`). Hasta leerlas, se usa el tramo base de tu captura: 0,25 % maker / 0,40 % taker, para 0 a 50 K USD de volumen | `AssetPairs` ya no trae comisiones. Pueden bajar con volumen; no se asume ninguna bajada |
| D12 | La prueba usa datos de perpetuos de Binance como proxy de precio, con esa limitación explícita | Es lo único disponible sin Kraken |

## 2. Hechos verificados

### 2.1 Cuenta

- Tienes spot activo y no eres apto para futuros.
- Según la política general de Kraken, las cuentas estándar pueden usar API. La página de API de Kraken Pro no se pudo leer desde aquí, así que lo que falta comprobar es si la clave de spot ve tu saldo.

### 2.2 Kraken Prop: fuera del plan

Según la página de Kraken Prop que citaste, las cuentas de evaluación y fondeadas no admiten API en ninguna fase, y las órdenes se introducen a mano. No he podido comprobarlo desde aquí.

### 2.3 Comisiones

- Spot, tramo base (captura del usuario): 0,25 % maker / 0,40 % taker, para 0 a 50 K USD de volumen.
- Las fuentes web daban 0,40 % / 0,80 % con un escalón a 2,5 K USD. No coinciden con tu captura, que es la de tu cuenta.
- El plan v3.2 usaba esas cifras y queda corregido aquí.

### 2.4 Modos del swarm

| Modo | Decisión | Cortos | Artefacto | Paper y live |
|---|---|---|---|---|
| Legacy | `Decision::Target` | `allow_short: true` en `hpo_swarm.json` | `configs/hpo_swarm.json` | Rechazado por el registro |
| Classifier | `Decision::Entry` | `allow_short: false` en `hpo_btcusdt_2026.json` | `configs/hpo_btcusdt_2026.json` | Aceptado |

El artefacto clasificador ya es solo largo. Pasar a largo no le quita señales. Lo que medirá la prueba es cuántas señales cortas generaba el modo con cortos.

### 2.5 Compatibilidad de spot: bloqueo de código

- `SwarmConsensusStrategy::compatible_market_types()` devuelve solo `UsdMPerpetual` (`crates/strategies/src/swarm_consensus/mod.rs:44-46`).
- Por eso `backtest --spot` se detiene con "does not support Spot" (`crates/backtest/src/main.rs`, comprobación de `supports_market_type`).
- La prueba evita ese camino: usa el artefacto perpetuo con apalancamiento 1, funding 0 y mantenimiento 0. Para perpetuos, el motor solo exige que el mantenimiento esté entre 0 y 1 (`crates/backtest/src/config.rs`, validación), así que 0 pasa.
- Es un atajo: el motor sigue la ruta de perpetuos. Declarar spot en la estrategia es un cambio de una línea, que se hará solo si la prueba pasa.

### 2.6 Economía de la operación

Con entradas de mercado, como en el motor:

| Tramo de la operación | Perpetuo, clasificador | Spot Kraken (captura) |
|---|---|---|
| Entrada (siempre taker) | 0,05 % | 0,40 % |
| Salida por take profit (maker si descansa) | 0,02 % | 0,25 % |
| Salida por stop loss (taker) | 0,05 % | 0,40 % |
| **Ida y vuelta por take profit** | **0,07 %** | **0,65 %** |
| **Ida y vuelta por stop loss** | **0,10 %** | **0,80 %** |

El coste es 9 veces el perpetuo en take profit y 8 veces en stop loss.

### 2.7 Datos

- El descargador `fetch_data` solo baja perpetuos: `data/futures/um` (`crates/adapters/src/binance_fetcher.rs:35-40`).
- `data.binance.vision` respondió 403 desde este entorno. La descarga la ejecutas tú en tu equipo.
- Para spot real hace falta una ruta nueva de descarga. Antes de escribirla hay que verificar el formato: si los archivos spot usan marcas de tiempo en microsegundos (verificar en la documentación de Binance; no lo he confirmado).

### 2.8 Fidelidad en spot

- En spot, una orden límite de salida solo se llena si un trade atraviesa su nivel. La variante de salida maker de la prueba asume que el take profit se llena en cuanto el precio toca el nivel, así que es optimista.
- Hoy el motor no modela cola: esto se corrige en la fase 7. Hasta entonces, una prueba positiva en la variante maker no basta.

## 3. Arquitectura objetivo

```text
crates/
├── domain/      Puro, sin E/S. Instrument con MarketType::Spot, Money en EUR, FeeTier,
│                órdenes y su máquina de estados, posición, riesgo (long-only, sin apalancamiento,
│                base de riesgo), breakers en EUR, analítica (Monte Carlo con RNG determinista).
├── strategies/  swarm_consensus (declarará Spot solo cuando la prueba pase), su contrato y un registro con una entrada.
├── ports/       MarketDataStream, AsyncMarketDataStream, DepthStream*, TradeTapeWriter*, Clock*,
│                InstrumentCatalog*, FeeSource*, ExecutionVenue, ExitGateway, trait de persistencia
│                (movido), AlertNotifier.                                              (* = nuevo)
├── adapters/    kraken_spot/  REST público (AssetPairs, Trades), REST privado (API-Sign), WebSocket v2
│                              (trade, book con CRC32), WebSocket privado, gateway, grabadora
│                storage/      Parquet de trades y libro; SQLite del registro y del ledger
│                telegram/     (movido desde gcp/)
│                simulated/    SimulatedVenue con libro y cola de limit
└── backtest/    (se renombra a `app` al final) TradingSession, driver, simuladores y modos.
                 Solo `main.rs` y `wiring.rs` nombran adapters.
```

Reglas que pasan a ser tests en `crates/backtest/tests/architecture_contract.rs`:

- **R1.** `domain`, `strategies` y `ports` no dependen de crates de E/S. (Ya existe.)
- **R2.** `adapters` no depende de la capa de aplicación.
- **R3.** Los módulos de librería de la aplicación no importan `adapters::`.
- **R4.** `ports` no nombra exchanges ni formatos.
- **R5.** `domain` no usa `f32` ni `f64`, salvo en los módulos de estadística que se listen.
- **R6.** Solo `strategies::registry` nombra estrategias. (Ya existe.)
- **R7.** Ningún `SystemTime::now()` ni `Utc::now()` fuera de `adapters`.

## 4. Fases

Tamaño relativo: S, M o L. Cada fase tiene criterio de salida.

### Fase 0: higiene (S)

- **Hecho:** claves de Binance revocadas. Acceso a GitHub. Sistema operativo: Linux (Omarchy).
- **Pendiente:**
  - Recrear el token de Telegram en `.env` con permisos `600`.
  - Crear una clave de API de spot **sin permiso de retirada**, solo con consulta y trading.
  - Confirmar en tu cuenta que la clave ve el saldo de spot.

**Salida:** `.env` creado y clave de spot verificada.

### Fase 1: prueba económica spot (S de código, horas de cómputo)

**Pregunta:** ¿el swarm solo largo gana algo después de comisiones de spot?

**Qué cambia respecto a la versión anterior de esta fase.** Un mes no basta. La guarda de calibración del CLI (`main.rs:329`) exige datos posteriores al `calibration_end_ms` del artefacto (31-ene-2026), y el swarm necesita historia para decidir. La prueba usa ocho meses mensuales, de feb a sep de 2026, sobre un directorio de cinta dedicado. Los cortacircuitos del artefacto (diario 5 %, mensual 15 %) se desactivan en las variantes para medir la ventana completa; la referencia con los cortacircuitos del artefacto se corre aparte. Es una **refutación**: el artefacto probablemente ya ha visto esa ventana, así que un resultado positivo no pasa la prueba.

**Paso 0. Datos (en tu equipo).** Descarga los ocho meses de BTCUSDT perpetuos (proxy de precio) con el descargador del repo:

```bash
cd ~/atsnt
for m in 2 3 4 5 6 7 8 9; do
  cargo run --release -p adapters --bin fetch_data -- --symbol BTCUSDT --year 2026 --month "$m"
done
```

Cada mes queda como `data/historical/BTCUSDT/BTCUSDT-aggTrades-2026-MM.parquet`.

**Paso 1. Preflight con febrero.** Antes de la ventana completa, mide el ritmo de operaciones con un solo mes:

```bash
cd ~/atsnt
scripts/gate_a.sh --months "02"
```

Si hay cero operaciones, no sigas: el swarm necesita más historia en este tape y hay que entenderlo primero. Con el número de operaciones de este preflight se confirma o se ajusta el umbral de 50 operaciones del criterio.

**Paso 2. Ventana completa.**

```bash
cd ~/atsnt
scripts/gate_a.sh
```

El script enlaza los ocho meses en `~/atsnt-gate/tape-02-03-04-05-06-07-08-09/`, escribe las configuraciones en `~/atsnt-gate/` (las tres variantes con cortacircuitos en 1 y la referencia con los del artefacto), corre las cuatro simulaciones con `--capital 5000` y deja el resumen en `~/atsnt-gate/summary.txt`. `--capital 5000` está en USD, la moneda del backtester: es un proxy de tamaño, no tu saldo en EUR.

**Criterio** (sobre `spot_solo_largo_salida_maker`, neto con comisiones):

- **NO PASA:** neto ≤ 0 con al menos 50 operaciones. Se para: no se construye la ruta de Kraken y se investiga la estrategia.
- **NO CONCLUYENTE:** menos de 50 operaciones. Se decide juntos.
- **PASA LA PANTALLA:** neto > 0 con al menos 50 operaciones. No autoriza ejecución: la ventana puede estar contaminada, y la prueba B (12 meses de Kraken, walk-forward y DSR) sigue siendo obligatoria.
- **Informativo:** la variante taker, el control con cortos (cuántas operaciones cortas genera el modo con cortos) y la referencia con los cortacircuitos del artefacto, que indica en qué mes saltarían.

**Salida:** tabla de las cuatro simulaciones y veredicto (`summary.txt`).

### Fase 2: grabadora de Kraken spot (M), solo si la fase 1 pasa

- `adapters/src/kraken_spot/ws_public.rs`: WebSocket v2 en `wss://ws.kraken.com/v2`, canales `trade` y `book` con profundidad 25.
- **Checksum CRC32** sobre los 10 niveles superiores. Si falla, volver a suscribirse y resincronizar con snapshot.
- Persistencia en Parquet por día UTC: `data/kraken-spot/<par>/{trades,book}/AAAA-MM-DD.parquet`.
- Reconexión con backoff, heartbeat y rotación a medianoche UTC.
- Alerta por Telegram si hay un hueco de más de 60 s.
- Opcional, en paralelo desde ya: grabar solo trades por REST `Trades` (sección 6.2). Es barato y sirve para investigación aunque la prueba falle.

**Salida:** 7 días seguidos sin huecos sin alertar.

### Fase 3: consolidación hexagonal con goldens de swarm (L)

No depende del resultado de la prueba. El código sin uso sale igual.

- **3.1 Goldens.** Casos de swarm con `allow_short` true y false, en bar y tick. Generar con `UPDATE_GOLDEN=1 cargo test -p backtest --test golden_execution` y revisar el diff.
- **3.2 Reglas R2 a R7** en `architecture_contract.rs`.
- **3.3 Persistencia.** El trait de `adapters/src/analytics.rs` pasa a `ports/`.
- **3.4 Sin `adapters::` en la librería.** `paper.rs`, `tick_simulation.rs`, `recording.rs` y `parity.rs` reciben puertos.
- **3.5 Reglas puras a dominio.** `daily_watermark`, `excursion`, `fee_ledger`, `monthly_guard`, `execution_core`, `metrics` y `walk_forward`, tras comprobar con grep que no tienen E/S.
- **3.6 Clock.** `SystemClock` en `adapters`; `SimClock` para simulación y tests. Corregir `docs/ROADMAP.md`.
- **3.7 Sin nombres de exchange en `ports`.** Quitar las menciones de Binance en `ports/src/execution.rs` y `ports/src/lib.rs`.
- **3.8 Renombrar `tape`.** `binance_parquet.rs` pasa a `storage/tape_parquet.rs`.
- **3.9 Divisas explícitas.** `Currency` y `Money` en `domain`.

**Salida:** `fmt`, `clippy -D warnings` y tests en verde. Goldens idénticos. R2 a R7 pasan.

### Fase 4: borrado (M)

- Se borran las cuatro estrategias no swarm, el stack de portfolio y universo, y los binarios que dependen de ellos (sección 5).
- Se conserva el camino de perpetuos (`domain/src/futures.rs` y el motor) hasta cerrar la prueba, porque el control lo usa. Después se puede borrar.
- Se borra el código de Binance que no sirve para la prueba: gateway, streams y firma.

**Salida:** `strategy_ids()` devuelve solo `swarm_consensus`. Tests en verde.

### Fase 5: dominio spot (M), solo si la fase 1 pasa

- `Instrument` con `MarketType::Spot`, pares EUR, tick, paso y mínimos de AssetPairs.
- `RiskPolicy` rechaza ventas sin posición (`RiskError::ShortNotSupported`), apalancamiento 1 y funding nulo.
- Base de riesgo `min(capital operativo, equity EUR)`. Riesgo por operación de 0,5 % a 1 %, como en los artefactos actuales (fijar por simulación). Breakers diario y mensual sobre la base.
- `FeeTier` como datos, con validación de rango. Las comisiones se leen de la cuenta (`FeeSource`).

**Pruebas:** venta en corto rechazada; funding nulo; redondeo al tick y al lote; base de riesgo con capital operativo menor que el equity; breakers sobre la base.

**Salida:** tests de cada regla en verde.

### Fase 6: adaptador de Kraken spot (L), solo si la fase 1 pasa

- **6.1 Catálogo.** AssetPairs → `Instrument`. Verificar en la referencia los nombres de campo de tick, mínimo de orden y decimales.
- **6.2 Histórico.** `Trades` con `since` en nanosegundos y `count` hasta 1000, avanzando con `last`. Pausa de 1 a 2 s según la [guía de datos históricos](https://docs.kraken.com/exchange/guides/general/historical-data). Checkpoint con el último `last`.
- **6.3 Firma API-Sign.** `API-Sign` = base64( HMAC-SHA512( clave = base64decode(secret), mensaje = ruta + SHA256(nonce + postdata) ) ). La ruta empieza en `/0/private`. El nonce crece por clave. Usar `ring`. El vector de prueba sale de la muestra con firma esperada de la [guía de autenticación REST](https://docs.kraken.com/api/docs/guides/spot-rest-auth/), copiada como fixture.
- **6.4 REST privado.** Balance, TradeVolume (tarifa real), AddOrder, CancelOrder, QueryOrders y TradesHistory paginado.
- **6.5 WebSocket privado.** Token de 15 minutos con `GetWebSocketsToken`. Canal de ejecuciones ([autenticación WebSocket](https://docs.kraken.com/exchange/guides/websockets/authentication)).
- **6.6 `KrakenSpotGateway`** implementa `ExecutionVenue`. `live_trading.rs` se reescribe con él.
- **6.7 Límites de tasa.** Medirlos y limitar por endpoint con un cubo de fichas conservador.
- **6.8 Pruebas.** HTTP y WebSocket mockeados. Ningún test toca la red.

**Salida:** tests de firma y de checksum en verde con los vectores oficiales.

### Fase 7: fidelidad de simulación (L), solo si la fase 1 pasa

- **Market orders** recorren el libro grabado.
- **Limit orders** se ejecutan solo cuando un trade atraviesa el nivel (regla de cola).
- **Comisiones** de la cuenta, con `FeeSource`.
- **Latencia** medida con `parity_check` sobre órdenes reales mínimas.

**Salida:** `parity_check` con desviación explicada durante al menos 2 semanas de paper.

### Fase 8: operación en Omarchy (M), solo si la fase 1 pasa

```text
~/atsnt/
├── .env                 (600) claves de Kraken spot y Telegram
├── bin/                 binario compilado
├── configs/             artefactos versionados en git
├── data/kraken-spot/    Parquet de trades y libro
├── state/atsnt.sqlite   registro de runs y ledger (WAL)
└── backup/              copias nocturnas
```

- **Servicio:** unidad de usuario `~/.config/systemd/user/atsnt.service` con `WorkingDirectory=%h/atsnt`, `EnvironmentFile=%h/atsnt/.env`, `Restart=on-failure`, `TimeoutStopSec=30s` y `KillSignal=SIGINT`. `loginctl enable-linger $USER` para arrancar sin sesión.
- **Que el equipo no se suspenda:** en `/etc/systemd/logind.conf.d/atsnt.conf` (requiere administrador), `HandleLidSwitch=ignore`, `HandleLidSwitchExternalPower=ignore` e `IdleAction=ignore`. Si usas hypridle, quita su listener de suspensión (verificar en tu configuración).
- **Copias:** un timer de usuario con `sqlite3 state/atsnt.sqlite ".backup …"` cada noche y copia de `data/` a un disco externo.
- **Ledger:** cada fill, comisión y transferencia queda registrado. Lo necesitarás para tus declaraciones en España.
- **Alertas:** Telegram, movido a `adapters/telegram/`.

**Salida:** el servicio se reinicia tras matar el proceso. Una copia se restaura en prueba. El equipo no se suspende.

### Fase 9: paper y dinero real por tramos (calendario)

- Paper durante al menos 4 semanas, con datos grabados.
- Dinero real solo si el DSR y el PnL neto OOS son positivos con comisiones de spot.
- Capital operativo: se recomienda empezar con una parte pequeña del capital disponible, durante 4 semanas. La decisión del importe es tuya y no se pregunta antes de que la prueba pase.

## 5. Inventario de borrado

| Ruta | Acción | Fase |
|---|---|---|
| `crates/strategies/src/{dollar_bars_cusum,order_flow_imbalance,langevin_potential,quantum_well_swing}.rs` | Borrar, con sus `pub mod`, `pub use` y entradas del registro | 4 |
| `crates/domain/src/universe.rs`, `crates/backtest/src/{portfolio_engine,universe_selector,sinks}.rs` | Borrar | 4 |
| `crates/backtest/src/bin/{run_portfolio,run_portfolio_hpo,paper_trading_portfolio,run_universe_portfolio,testnet_exit_check}.rs` | Borrar | 4 |
| `crates/adapters/src/binance_*.rs`, `binance_gateway/`, `binance_fetcher/`, `fetch_plan.rs` | Borrar tras la prueba. `binance_parquet.rs` se renombra antes (3.8) | 4 |
| `crates/adapters/src/bin/{stream_trades,live_gateway}.rs` | Borrar; sustituidos por la grabadora y el gateway de Kraken | 4 |
| `crates/adapters/tests/fixtures/binance_private_events/` | Borrar | 4 |
| `crates/adapters/src/gcp/*` salvo `telegram.rs` | Borrar; mover `telegram.rs` a `adapters/src/telegram/` | 3 |
| `configs/` salvo `hpo_btcusdt_2026.json`, `hpo_swarm.json` y `swarm_shared.json` | Borrar el resto. Añadir `kraken_spot_eur.json` en la fase 5 | 4 y 5 |
| `Dockerfile`, `.dockerignore`, `scripts/gcp/`, `scripts/run_production_pipeline.sh`, `scripts/run_joint_portfolio_pipeline.sh`, `scripts/tests/` | Borrar | 8 |
| `docs/architecture/10-gcp-cloud-architecture.md` | Borrar | 8 |
| `docs/strategy/{01,02,04,05}*.md`, `docs/research/02-*`, `SWARM_OPTIMIZATION_ROADMAP.md` | Mover a `docs/archive/` | 4 |
| `docs/architecture/{07,08,13}*.md`, `docs/capital-scaling.md`, `docs/deployment/atsnt.service`, `README.md`, `AGENTS.md`, `docs/ROADMAP.md`, `.env.example` | Reescribir | 3, 5 y 8 |
| `odd/tasks/` | Conservar como historial | — |

## 6. Pruebas y puertas

| Fase | Pruebas | Puerta de salida |
|---|---|---|
| 1 | Las tres variantes, con los logs | Decisión registrada |
| 2 | Parseo de WebSocket con fixtures; checksum CRC32 con el vector de la guía | 7 días sin huecos |
| 3 | Goldens de swarm con `allow_short` true y false; R2 a R7 | Goldens idénticos |
| 4 | `strategy_ids()`; grep de nombres | Tests en verde |
| 5 | Venta en corto rechazada; redondeo; base de riesgo; breakers | Cada regla con test |
| 6 | Firma con vector oficial; paginación con HTTP mockeado | Vectores en verde |
| 7 | Fills de market contra libro sintético; cola de limit | `parity_check` dentro de tolerancia |
| 8 | Reinicio tras matar el proceso; restauración de copia | Alerta al parar |
| 9 | — | Criterios de la fase 9 |

En todas las fases: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` y `cargo test --workspace`.

## 7. Riesgos

| Riesgo | Impacto | Mitigación |
|---|---|---|
| El swarm solo largo no gana después de comisiones de spot | Alto | Fase 1 antes de cualquier ejecución |
| Comisiones de spot de 0,25 % / 0,40 % | Alto | Salidas maker cuando descansan; volumen; medir cada operación |
| Con solo largos, pocas señales | Medio | La variante de control lo mide |
| La prueba usa datos de perpetuos | Medio | Limitación explícita; repetir con datos de Kraken antes de decidir |
| Clave de API comprometida | Alto | Sin permiso de retirada; `.env` con permisos `600` |
| Registro fiscal incompleto | Medio | Ledger con cada fill, comisión y transferencia desde el primer día |
| El equipo se suspende y se pierden datos | Alto | Configuración de logind y de hypridle; alertas de hueco |

## 8. Fuentes

- Comisiones spot (captura del usuario): tramo base de Kraken Pro, 0,25 % maker / 0,40 % taker.
- Datos históricos y Trades REST: https://docs.kraken.com/exchange/guides/general/historical-data · https://docs.kraken.com/api/docs/rest-api/get-recent-trades/
- WebSocket v2 spot, canales trade y book: https://docs.kraken.com/api/docs/websocket-v2/trade · https://docs.kraken.com/api/docs/websocket-v2/book · https://docs.kraken.com/api/docs/guides/spot-ws-book-v2/
- Autenticación spot: https://docs.kraken.com/api/docs/guides/spot-rest-auth/ · https://docs.kraken.com/exchange/guides/websockets/authentication
- Restaurar proyectos de GCP: https://docs.cloud.google.com/resource-manager/docs/delete-restore-projects

Las cifras de Kraken de este documento deben verificarse en tu cuenta y en la documentación vigente antes de usarse.
