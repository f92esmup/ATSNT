# HANDOFF: estado y siguientes pasos de ATSNT

Fecha de cierre: 2026-10-31. Este documento es para la siguiente IA (o persona) que continúe el
trabajo sin acceso al chat donde se tomaron las decisiones. Es autocontenido: el plan aprobado vive
fuera del repositorio y aquí queda resumido.

## 0. Cómo usar este documento

- Si solo lees una sección, lee la **§5 (siguientes pasos)**.
- Antes de tocar nada, lee la **§3 (política de ramas)** y la **§8 (reglas de trabajo)**.
- Las cifras de la **§1** y la **§9** se midieron el 2026-10-31 sobre el commit `e915cb7`. Vuelve a
  medirlas antes de afirmarlas.

## 1. Estado actual

### 1.1 Repositorio

- Rama de trabajo: `claude/awesome-lamport-ch9ytr`. Tras el push de este documento, `main` apunta al
  mismo commit (ver §3).
- Commit de código verificado: `e915cb7`. El commit de este documento es posterior y solo añade
  documentación.
- Verificación al cierre (sobre `e915cb7`):
  - `cargo fmt --all -- --check`: limpio.
  - `cargo clippy --workspace --all-targets -- -D warnings`: limpio.
  - `cargo test --workspace`: 802 pruebas en verde, 0 fallos.
  - Los goldens (`crates/backtest/tests/golden/`) y `AGENTS.md` no han cambiado desde `4e02bef`.
- Toolchain: rustc y cargo 1.97.0.

### 1.2 Qué existe

- Motor de backtest de una sola estrategia: `swarm_consensus`, la única registrada.
- Arquitectura hexagonal: puertos en `crates/ports`; adaptadores en `crates/adapters` (cinta Parquet,
  registro de runs en SQLite, venue simulado, Telegram y, todavía, Binance).
- Registro de runs en SQLite: `--record <PATH>` en `backtest` y `run_hpo`. Sin el flag no se escribe
  nada.
- Script de la prueba económica A: `scripts/gate_a.sh`.
- Documentación vigente y archivo histórico (§7.6).

### 1.3 Qué no existe en `main`

- **No hay datos.** El repositorio no tiene `data/`. Los archivos mensuales de BTCUSDT los descarga
  el usuario con `fetch_data`.
- **No hay cliente de Kraken en `main`.** Sí existe uno en la rama remota
  `feat/kraken-eur-spot-portfolio`, que no fusiona limpio con el refactor (§3 y §11).
- **No hay claves** de ningún exchange en el repositorio, ni deben añadirse.
- **No hay ejecución en Kraken.** `paper_trading` y `live_trading` hablan con Binance. `live_trading`
  envía órdenes reales si recibe claves: no lo ejecutes con claves reales.

### 1.4 Fases y compuertas

| Compuerta | Estado | Quién la ejecuta |
|---|---|---|
| G0: plan aprobado | Hecho | — |
| GA: prueba económica (refutación) | Pendiente | El usuario, en su equipo, con `scripts/gate_a.sh` |
| GB: 12 meses de Kraken, walk-forward y DSR (WP-08 a WP-12) | No empezada | Usuario y agente, tras GA |
| GP: paper de 4 semanas y dinero real por tramos (WP-13 a WP-17) | No empezada | Usuario y agente, tras GB |

Fases de la v4 (`docs/plan/refactor-local-kraken.md` §4):

- Fase 0 (higiene): pendiente del usuario (WP-00).
- Fase 1 (prueba económica): pendiente (GA).
- Fase 2 (grabadora de Kraken): no empezada. Empieza en cuanto GA pase.
- Fase 3 (consolidación hexagonal): hecha, salvo 3.9 (Money y Currency, aplazado a WP-11), SimClock
  (no añadido) y `walk_forward.rs` (se queda en backtest).
- Fase 4 (borrado): parte hecha. Pendiente: la ruta de ejecución de Binance, que se borra después de
  GA (§5.2).
- Fases 5 a 9: no empezadas.

## 2. Historial de commits

Base: `50b4ee5`, que era el commit de `main` antes del push de este trabajo.

**Planificación (7):**

- `f1bfaf0` docs(plan): add local Kraken EUR refactoring plan
- `c4b04e7` docs(plan): rewrite plan for Kraken Futures perpetuals (v2)
- `9f9a780` docs(plan): update plan to EUR collateral, 5,000 EUR capital and Omarchy (v3)
- `fcd9bbd` docs(plan): v3.1 single risk base, FX split, artifact check
- `010cf57` docs(plan): correct spot fee tier from the account fee schedule (v3.2)
- `1048c63` docs(plan): v4, spot-first plan gated by a local economics test
- `9f021c6` docs(plan): add spot implementation plan as work packages for approval

**Lote 1 (16), refactor en nueve pasos:**

- `6215aa5` test(backtest): pin swarm execution on both routes with goldens
- `9d0fd0a` test(architecture): cut production code at the test module and forbid floats in domain
- `69b1f16` refactor(ports): move the run recorder port, add a clock port, drop vendor names
- `0c0ad9e` refactor(backtest): take instants as parameters, read the clock only in composition
- `2deb494` refactor(backtest): run the tick simulation and parity on a tape venue port
- `e4c34ba` refactor(domain): move the pure accounting rules into domain::accounting
- `86809bc` refactor(domain): move the fill arithmetic out of the execution core
- `87b645e` refactor(adapters): rename the Binance tape store to a generic tape module
- `ca7af74` chore(backtest): remove the portfolio, universe and testnet binaries
- `b78913f` refactor(backtest): make the paper session take its strategy, default to swarm
- `37b2929` refactor(backtest): drop the BigQuery and GCS sinks from the kept binaries
- `84ecf15` refactor(adapters): move Telegram out of the GCP module and drop the cloud adapters
- `e470238` refactor: remove the universe and portfolio stack
- `fa5b384` refactor: remove the non-swarm strategies
- `2887298` chore: retire the Dockerfile and the non-swarm configs
- `4e02bef` test(backtest): pin the swarm's recording rows on the 90-day tape

**Lote 2 (8), limpieza, compuerta A y registro:**

- `69c3312` chore(gate): make gate A runnable with a falsification rule
- `e7961df` chore(config): remove the non-swarm configs and the Cloud Batch ignore rule
- `7bb0788` refactor(domain): move the performance metrics into the domain crate
- `7661e35` chore(lint): deny unwrap and expect in production code
- `7ca9232` feat(storage): record run analytics in a local SQLite store
- `c920be6` chore(docs): retire the GCP scripts and archive the removed strategies' docs
- `032c57b` docs: rewrite the README, roadmap and capital guide for the current system
- `e915cb7` docs: point the architecture guides at the current code, archive the deployment units

**Documento de traspaso:** el commit que añade este `HANDOFF.md` y corrige los textos de estado.

## 3. Política de ramas (leer antes de publicar)

- Tras el push de este documento, `main` y `claude/awesome-lamport-ch9ytr` apuntan al mismo commit.
- El usuario autorizó el push a `main` de forma puntual, para este push. **No es un permiso general.**
- El trabajo nuevo va en una rama. Cada push a `main` necesita una confirmación nueva del usuario.
- Sin `--force`. Sin pull requests salvo que el usuario los pida.

Ramas remotas existentes (`git ls-remote --heads origin`):

- `main`: incluye el refactor después de este push.
- `claude/awesome-lamport-ch9ytr`: rama de trabajo de los lotes 1 y 2.
- `feat/kraken-eur-spot-portfolio` (`b35e929`): cinco commits de un cliente de Kraken de una etapa
  anterior. **No fusiona limpio con `main`**: 7 ficheros en conflicto, todos eliminados por el
  refactor. Decisión pendiente (§11).
- `feat/dynamic-topk-universe`: en `50b4ee5`, sin commits propios.

## 4. Decisiones fijas (no reabrir sin el usuario)

| Decisión | Fuente |
|---|---|
| Solo `swarm_consensus`. Las demás estrategias y el stack de portfolio se retiraron | Instrucción del usuario; v4 D1 |
| Venue objetivo: Kraken spot, EUR, solo largo, sin apalancamiento, sin funding | v4 D2, D3, D4, D6 |
| Kraken Prop: órdenes manuales, fuera del sistema | Respuesta del usuario sobre la página de Kraken Prop |
| Capital de referencia 5.000 € (`operating_capital_eur`, se fija en GP) | Respuesta del usuario; `docs/capital-scaling.md` |
| Comisiones base 0,25 % maker / 0,40 % taker (captura del usuario). Se leerán de `TradeVolume` al llegar a WP-13 | v4 D11 |
| GA es una refutación: ventana feb–sep 2026 sobre proxy de perpetuos; umbral propuesto de 50 operaciones | Respuesta del usuario sobre la ventana; plan aprobado |
| GB exige 12 meses de cinta de Kraken con walk-forward y DSR | Respuesta del usuario |
| El registro de runs es SQLite (`--record`); el ledger de fills va en WP-16 | v4 D8; implementación WP-16 |
| Telegram vía `TELEGRAM_BOT_TOKEN` y `TELEGRAM_CHAT_ID` | `.env.example`; `crates/adapters/src/telegram/mod.rs` |
| La ejecución de Binance sale del código después de GA, tenga o no éxito GA. El descargador se queda hasta WP-09 | Plan aprobado; v4 D7 |
| Sin `unwrap` ni `expect` fuera de tests (clippy lo fuerza) | `AGENTS.md` |

## 5. Siguientes pasos (en orden)

### 5.1 Usuario (antes de cualquier trabajo de agente)

1. **WP-00.** Crear la clave de Kraken spot con permisos de consulta, de órdenes y de WebSocket, y
   **sin retirada**. Comprobar que ve el saldo. Crear `.env` con permisos 600 (ver `.env.example`),
   con el token de Telegram nuevo.
2. **Red del entorno.** Permitir `docs.kraken.com`: desde el menú del entorno, Edit, Network access;
   añadir el host en Allowed domains con «Allow package managers» marcado, o ampliar el acceso. Guía:
   https://code.claude.com/docs/en/cloud-environments#network-access. Sin esto no se pueden leer los
   textos literales de firma y de checksum de Kraken.
3. **Prueba A, preflight** (en el equipo del usuario, con el repositorio clonado). Si faltan los
   archivos, descárgalos:
   ```bash
   for m in 2 3 4 5 6 7 8 9; do
     cargo run --release -p adapters --bin fetch_data -- --symbol BTCUSDT --year 2026 --month "$m"
   done
   ```
   Después, la pasada de febrero:
   ```bash
   scripts/gate_a.sh --months "02"
   ```
   Anotar operaciones, rechazos y tiempo.
4. **Prueba A, ventana completa:**
   ```bash
   scripts/gate_a.sh
   ```
   Enviar `~/atsnt-gate/summary.txt` y el log de la referencia
   (`~/atsnt-gate/referencia_maker_con_cortacircuitos.log`).
5. **Umbral.** Confirmar o cambiar el umbral de 50 operaciones tras el preflight.

### 5.2 Agente, según el veredicto de GA

El script decide con esta regla sobre `spot_solo_largo_salida_maker` (neto con comisiones, con los
cortacircuitos en 1):

- **NO PASA:** neto ≤ 0 con al menos 50 operaciones.
- **NO CONCLUYENTE:** menos de 50 operaciones.
- **PASA LA PANTALLA:** neto > 0 con al menos 50 operaciones. No autoriza ejecución: la ventana puede
  estar contaminada (§10, punto 1).

**Si NO PASA — L3N:**

- Borrar la ruta de ejecución y streaming de Binance: `crates/adapters/src/binance_gateway.rs` y
  `binance_gateway/`, `binance_auth.rs` (si solo lo usan el gateway y los streams),
  `binance_user_stream.rs`, `binance_private_stream.rs`, `binance_ws.rs`, `binance_recent_trades.rs`,
  `crates/backtest/src/bin/live_trading.rs`, `crates/backtest/src/bin/paper_trading.rs`,
  `crates/backtest/src/bin/parity_check.rs` (solo compara fills de ejecución en vivo) y
  `crates/adapters/tests/fixtures/binance_private_events/`, si existe.
- **Conservar** el descargador: `fetch_data`, `binance_fetcher`, `fetch_plan`, `binance_csv`,
  `binance_funding` y `test_http.rs`. La investigación de estrategia necesita historia.
- Quitar `--bnb-discount` de `backtest` y `run_hpo`. `--spot` se mantiene hasta WP-11.
- Archivar `docs/architecture/06` y `08`.
- Decidir con el usuario qué pasa con la rama de Kraken (§11).
- Pendiente de decisión: el camino de perpetuos (`crates/domain/src/futures.rs` y el motor de
  perpetuos).

**Si PASA LA PANTALLA — L3G:**

- WP-10 (grabadora WebSocket pública de Kraken: canales `trade` y `book`, con checksum CRC32) empieza
  ya. El libro no se reconstruye después. Requiere `docs.kraken.com` accesible (§5.1, punto 2).
- WP-10 necesita grabación sin huecos desde el primer día para llegar a 12 meses. Por eso la unidad
  de usuario de systemd y la configuración de logind para que el equipo no suspenda se adelantan de
  WP-16. La de logind requiere administrador.
- WP-08 (convertidor de trades de Kraken a `Trade`) y WP-09 (cliente REST público y backfill): medir
  profundidad y tiempo por día **antes** de prometer los 12 meses.
- WP-11 (dominio spot): `MarketType::Spot`, `Currency` y `Money` (v4 3.9), `FeeTier`, rechazo de
  ventas sin posición, base de riesgo.
- El descargador de Binance se borra cuando WP-09 lo sustituya. Paper y live, cuando WP-14 y WP-15 los
  reemplacen.

**Si NO CONCLUYENTE:** decidir juntos (menos de 50 operaciones).

### 5.3 Después: GB y GP

- **GB (WP-12):** `calibrate_feeds` sobre Kraken; `run_hpo` walk-forward con embargo y DSR;
  long-only; comisiones de `TradeVolume`; capital de referencia. Criterio: neto OOS > 0 y DSR según
  `run_hpo`. Si no pasa, no se construye ejecución.
- **WP-13 a WP-17:** firma API-Sign y REST privado; WebSocket privado y gateway de Kraken; simulación
  con cola de límite y libro; operación local (ledger en SQLite, copias, servicio); paper de cuatro
  semanas y dinero real por tramos. Detalle en `docs/plan/implementacion-kraken-spot.md`.

## 6. Desviaciones respecto al plan

### 6.1 Lote 1 (plan aprobado, nueve pasos)

1. El paso 7 empezó borrando binarios (7a): `run_portfolio_hpo`, `paper_trading_portfolio`,
   `testnet_exit_check` y `live_gateway` dependían de GCP y estaban previstos en el paso 8. Con el
   orden original, un commit intermedio no compilaba.
2. `SinkFailures`, `StrictSinkError`, `artifact_object_name` y `artifact_run_suffix` se borraron en
   lugar de conservarse. Sin BigQuery ni GCS no tenían usuarios.
3. El golden HPO (`hpo_cusum_ranking`, `recording_hpo_rows`) se borró: el ranking dependía de barras y
   el swarm no las consume. `record_hpo` queda cubierto por tests unitarios.
4. El test de cobertura de salidas se eliminó. TimeBarrier, MonthlyCircuitBreaker, DailyHalt y
   OrderSizeExceeded siguen cubiertos por tests del motor; los goldens cubren StopLoss, TakeProfit,
   TargetExit, TargetFlip y EndOfSimulation.
5. `PortfolioRiskPolicy` se borró de `domain/risk.rs` (no estaba en el plan).
6. `run_backtest_slice` y su variante con trades se borraron (sin llamadores).
7. Tests: `param_space_contract` se borró; `hpo_fold_metrics` pasó a test unitario de `optimization`
   con `MockStrategy`; se borraron `strict_sinks_cli`, `sink_contract` y los tests de esquema de
   BigQuery; se quitó el caso de `swarm_runners` que comprobaba el rechazo de flags de trades, porque
   su premisa ya no existe.
8. `metrics.rs` se quedó en backtest en el lote 1, contra lo que decía el plan. Se movió a domain en
   el lote 2.
9. Dos aciertos intencionados del grep del paso 8: `BigQuery` en la lista de nombres prohibidos de
   `ports` y el test de OFI en `domain` (nombra una feature de analítica, no la estrategia).
10. Configs: en el lote 1 se conservaron `hpo_results.json` y `execution_*.json`; el lote 2 los borró.
11. Las claves de Binance se leen de variables de entorno o de `.env`. No hay fallback a Secret
    Manager.
12. Trailers: sin nombre de modelo (conflicto registrado en §8.1).
13. El sandbox se quedó sin disco a mitad del trabajo. Se borraron caché incremental y ejecutables de
    test antiguos de `target/`, que son regenerables. No se tocaron datos ni fuentes.

### 6.2 Lote 2

1. `docs/deployment/` se archivó (`docs/archive/deployment/`) en lugar de llevar banner: sus unidades
   citan un binario retirado.
2. `docs/architecture/13-bi-data-contract.md` pasó a `13-run-registry.md`: ya no es un contrato de BI.
3. El mensaje de cortacircuitos con hora existe solo en `main.rs`. `live_trading.rs` conserva el texto
   antiguo.
4. `metrics.rs` se movió a `domain` (§6.1, punto 8).
5. La prueba A tiene cuatro simulaciones, no tres: tres variantes con cortacircuitos en 1 y la
   referencia con los del artefacto.
6. El registro usa un enum `Recorder` en `wiring.rs`, porque el puerto `RunRecorder` ya no es
   dyn-compatible (sus métodos devuelven `impl Future + Send`).
7. Los tests pasaron de 800 a 796 al borrar `execution_config_contract.rs` (cuatro tests), y a 802 al
   añadir cinco tests del adaptador SQLite y uno de ida y vuelta.
8. `SimClock` no se añadió (sin consumidores). Money y Currency (fase 3.9 de la v4) se aplazan a WP-11.
   `walk_forward.rs` se queda en backtest (usa f64 y `BacktestError`).

## 7. Mapa del código y de la documentación

### 7.1 Crates

- `domain`: reglas puras (órdenes, posiciones, riesgo, contabilidad, métricas). Sin E/S ni floats.
- `ports`: contratos (datos de mercado, ejecución, reloj, registro de runs, alertas). Sin nombres de
  proveedor.
- `strategies`: `swarm_consensus` y su registro. La macro `register_strategies!` es el único sitio que
  nombra estrategias.
- `adapters`: cinta Parquet (`storage/tape_parquet.rs`), registro SQLite (`storage/sqlite_runs.rs`),
  venue simulado, Telegram, reloj del sistema y, por ahora, Binance.
- `backtest`: simuladores, optimizador, Monte Carlo, sesiones y binarios. La librería no nombra
  adaptadores: `main.rs` y `wiring.rs` componen.

### 7.2 Binarios

| Binario | Crate | Estado y destino |
|---|---|---|
| `backtest` | backtest | Se queda. `--record <PATH>` |
| `run_hpo` | backtest | Se queda. `--record <PATH>` |
| `calibrate_feeds` | backtest | Se queda (la usa GB) |
| `prune_swarm` | backtest | Se queda |
| `parity_check` | backtest | Binance. Se reescribe en WP-15 (o se borra en L3N) |
| `paper_trading` | backtest | Binance. Se reemplaza en WP-15 |
| `live_trading` | backtest | Binance. Se reemplaza en WP-14. **No usar con claves reales** |
| `fetch_data` | adapters | Descargador de Binance. Se queda hasta WP-09 |

### 7.3 Configs (`configs/`)

- `hpo_btcusdt_2026.json`: artefacto clasificador del swarm (`shared_config.allow_short = false`,
  `calibration_end_ms` = 2026-01-31T23:59:59.730Z). Base de la prueba A.
- `hpo_swarm.json`: artefacto legado del swarm con cortos (`allow_short = true`).
- `swarm_shared.json`: configuración compartida del swarm (umbrales de feeds).

### 7.4 Scripts

- `scripts/gate_a.sh`: enlaza los meses en `~/atsnt-gate/tape-<meses>/`, genera las configuraciones del
  gate, corre cuatro simulaciones y escribe `summary.txt`. Opciones: `--months`, `--source`, `--out`,
  `--capital` y `--dry-run`. Requiere python3 y `realpath` de GNU.

### 7.5 Tests clave

- `crates/backtest/tests/architecture_contract.rs`: reglas R1 a R7 (dependencias, sin E/S en el núcleo,
  sin floats en `domain`, puertos sin nombres de proveedor, reloj solo en adaptadores y composición,
  sin adaptadores en la librería, despacho de estrategias por registro).
- `golden_swarm.rs` y `golden_execution.rs`. Con `UPDATE_GOLDEN=1` se regeneran; el diff se revisa
  antes de commitear.
- `golden_execution.rs` incluye la ida y vuelta del registro SQLite
  (`backtest_recording_rows_round_trip_through_the_sqlite_store`).
- `recording_contract.rs`, `metrics_contract.rs`, `trade_excursion.rs`, `run_contract.rs`,
  `swarm_runners.rs`, entre otros.

### 7.6 Documentación

- Vigente: `README.md`, `docs/ROADMAP.md`, `docs/capital-scaling.md`,
  `docs/architecture/13-run-registry.md`, `docs/plan/*` (v4 e implementación),
  `docs/strategy/03-swarm-consensus.md`, `docs/research/01-swarm-consensus-optimization-logbook.md`.
- Con banner de estado, porque su texto es histórico en parte: `docs/architecture/03`, `05`, `06`,
  `08`. `04` conserva referencias históricas a CUSUM y BigQuery.
- Archivo (`docs/archive/`): estrategias retiradas, universo de portfolio, diseño de dashboard que no
  se implementó, unidades de systemd antiguas y el roadmap previo.
- `odd/`: histórico de tareas, con un README que lo advierte.

## 8. Reglas de trabajo

### 8.1 Git

- Un commit por paso. Mensajes en inglés, estilo convencional (`feat(...)`, `refactor(...)`,
  `chore(...)`, `docs: ...`), como en el historial.
- Trailers obligatorios:
  - `Co-Authored-By: Claude <noreply@anthropic.com>`
  - `Claude-Session: https://claude.ai/code/session_01U3r4mFA8RQ1Dr5WhZunKdp`
  - Sin nombre de modelo en el commit. Las instrucciones del sistema lo prohíben; la nota de
    atribución lo pedía. El conflicto queda registrado aquí; el usuario decide si cambia la regla.
- Antes de cada commit, con `CARGO_INCREMENTAL=0` si hay poco disco:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
- Los goldens no se regeneran sin revisar el diff. `AGENTS.md` no se toca sin aprobación del usuario.
- Publicar solo con confirmación del usuario (§3). Sin `--force`. Sin PR sin petición.
- Estado del historial: `git log --oneline 50b4ee5..HEAD`.

### 8.2 Código (AGENTS.md)

- Sin `unwrap()` ni `expect()` fuera de tests. Cada raíz de crate deniega `clippy::unwrap_used` y
  `clippy::expect_used` cuando no compila para tests.
- `domain` sin `f32` ni `f64` (regla R5). Dinero y cantidades siempre en `rust_decimal::Decimal`.
- Las reglas R1 a R7 viven en `architecture_contract.rs`. Si una regla bloquea algo, se discute; no se
  desactiva en silencio.
- Los tests no llaman a la red. Los adaptadores se prueban con datos en memoria o con `tempfile`.

### 8.3 Seguridad

- Ningún secreto en el repositorio. `.env` está en `.gitignore`. `.env.example` solo lleva marcadores.
- No ejecutar `live_trading` ni `paper_trading` con claves reales: siguen hablando con Binance.
- Antes de publicar: escanear el contenido en busca de claves y tokens (patrones de AWS, claves PEM,
  tokens de Slack y asignaciones largas de `api_key`, `secret` o `token`).

## 9. Entorno

- Disco del sandbox en el momento de escribir: 252 GB, 13 GB libres (69 % usado). `target/` pesa 18 GB.
  Mídelo antes de cada sesión.
- Para ahorrar disco: `CARGO_INCREMENTAL=0`. Si falta espacio, borrar ejecutables de test antiguos de
  `target/debug/deps` (son regenerables). No tocar fuentes ni datos.
- Red: `docs.kraken.com` está bloqueado hasta que el usuario lo permita (§5.1, punto 2).
  `index.crates.io` responde, así que crates.io es accesible. Según la v4 (§2.7), `data.binance.vision`
  respondió 403 desde este entorno; los archivos los descarga el usuario.
- Herramientas: python3 (`gate_a.sh`), `cc` (SQLite embebido), `realpath` de GNU (`gate_a.sh`).
- rustc y cargo 1.97.0.
- `cargo fmt --all` reescribe ficheros. Tras formatear, vuelve a leerlos antes de editarlos.

## 10. Hallazgos que cambian supuestos

1. **Guarda de calibración.** `main.rs:329` y `run_hpo.rs:384` exigen que el primer trade evaluado sea
   posterior a `calibration_end_ms` (2026-01-31T23:59:59.730Z). Si el HPO que produjo
   `hpo_btcusdt_2026.json` usó la misma guarda, evaluó datos de feb-2026 en adelante. Eso no encaja con
   «solo enero». **Pendiente de confirmar** con el historial del HPO.
2. **Las 70 jornadas son del HPO.** `TRADES_WARM_UP_MS` (`optimization.rs:30`) corta los folds del
   walk-forward; el CLI por trades no lo impone. El golden sintético abre su primera operación hacia el
   día 17.
3. **Un mes no basta.** `fetch_data --month` escribe un Parquet mensual (`binance_fetcher.rs:52-60`).
   `backtest --data` acepta un directorio y reproduce sus Parquet en orden (`tape_parquet.rs:163-177`).
4. **`resting_maker_exits` sí cambia la prueba.** Se aplica en `execution_core.rs:1069`: los take profit
   salen como maker, sin deslizamiento. Las variantes maker y taker difieren.
5. **Cortacircuitos del artefacto.** `max_daily_drawdown_pct` = 0,05 y `max_monthly_drawdown_pct` = 0,15.
   La ruta por trades detiene la simulación al saltar el mensual (`main.rs:338-342`). Por eso la prueba
   desactiva los cortacircuitos en sus variantes y corre la referencia aparte.
6. **Scorecard negativo.** El artefacto tiene OOS negativo fuera de muestra: profit factor medio 0,64;
   Sortino medio −0,22; DSR no significativo; 84 operaciones OOS. `odd/tasks/hpo-scoring.md:112`
   concluye que el borde bruto es pequeño y los costes, 3,4 veces mayores. Los costes de spot son 8–9
   veces los de perpetuos (v4 §2.6). **Se espera un NO PASA.**
7. **Fracciones de los cortacircuitos.** `validate` acepta fracciones en (0, 1] (`config.rs:177-187`).
   Un valor de 1 no dispara nunca, y la prueba lo usa para medir la ventana completa.
8. **`rusqlite` con `bundled`** compila SQLite en C. Requiere `cc`, que está en el entorno.
9. **El puerto `RunRecorder`** devuelve futuros `Send` y no es dyn-compatible: la composición usa un
   enum (`Recorder`).
10. **`crate::` dentro de `domain`.** Un crate no puede referirse a sí mismo por su nombre: los imports
    dentro de `domain` usan `crate::`.
11. **Clippy y tests.** `unnecessary_literal_unwrap` sí se dispara en tests con `Some(1).unwrap()`
    cuando se usa `-D warnings`. El lint `unwrap_used` queda exento en `#[cfg(test)]`.
12. **Formato.** `cargo fmt --all` reordena imports y reflota firmas; los ficheros cambian en disco.

## 11. Deuda y preguntas abiertas

- **Rama `feat/kraken-eur-spot-portfolio` (`b35e929`). Decisión pendiente.** Tiene cinco commits que
  `main` no tiene: `3c13234` (live gateway con prueba de orden spot), `c05c95b` (pares spot EUR en
  simulación y paper), `b2f5009` (dollar bars de 11 resoluciones para el universo Kraken), `c8886a5`
  (benchmark parametrizable y correlación dinámica en EUR) y `b35e929` (CLI de sweep de dollar bars y
  calibración de umbrales de Kraken).
  - Contenido: `crates/adapters/src/kraken_ws.rs` (467 líneas nuevas, cliente WebSocket de Kraken); seis
    configs de pares EUR (`configs/hpo_kraken_{avax,bnb,btc,eth,near,sol}eur.json`);
    `configs/kraken_spot_execution_context.json`; `configs/kraken_universe_deployment_artifact.json`;
    `crates/backtest/src/bin/sweep_dollar_bars.rs` (569 líneas nuevas).
  - Modifica binarios de portfolio que el refactor eliminó.
  - Fusionar con `main` da conflicto en 7 ficheros: `crates/adapters/src/bin/live_gateway.rs`,
    `crates/adapters/src/lib.rs`, `crates/backtest/src/bin/paper_trading_portfolio.rs`,
    `run_portfolio.rs`, `run_portfolio_hpo.rs`, `run_universe_portfolio.rs` y
    `crates/backtest/src/portfolio_engine.rs`.
  - Hasta resolverlo, `main` no tiene cliente de Kraken. Esa rama es la única fuente de ese código.
- `AGENTS.md`: cambios propuestos (ejemplos de Binance a Kraken, listado de reglas R1 a R7, lint).
  Pendiente de aprobación (§12, U5).
- Restos de CUSUM y BigQuery en `docs/architecture/04`, `05`, `06` y `08`. Tienen banner, pero conviene
  reescribirlos en WP-14 a WP-16.
- `docs/architecture/03`: pendiente de reescribir con Kraken (WP-09 para datos; WP-14 para ejecución).
- `docs/architecture/02`: la caja del diagrama pasó de `[ Portfolio ]` a `[ Position  ]`. Falta revisar el
  resto del texto en la próxima pasada de documentación.
- `live_trading.rs`: el mensaje de cortacircuitos mensual no lleva hora (solo lo lleva `main.rs`).
- `--spot` y `--bnb-discount` en `backtest` y `run_hpo`: referencias a Binance. Se quitan en L3N (`--spot`
  se mantiene hasta WP-11).
- `parity_check` se autodescubre por Cargo porque no tiene bloque `[[bin]]`. Se reescribe en WP-15.
- `SqliteRunRecorder::in_memory()` no tiene llamadores. Se mantiene como API pública para pruebas y
  ejecuciones efímeras.
- `walk_forward.rs` usa f64 y se queda en backtest. Si algún día se mueve a domain, convertir a `Decimal`
  con redondeo `MidpointAwayFromZero` y comprobar que los splits no cambian.
- Money y Currency (v4 3.9): aplazados a WP-11.
- `SimClock`: no añadido (sin consumidores).
- Rama `feat/dynamic-topk-universe`: sin commits propios respecto a `50b4ee5`. Sin acción.
- Trailers: conflicto entre la nota de atribución y las instrucciones del sistema (§8.1). Pendiente de
  decisión del usuario.
- Ventana del HPO del artefacto: confirmar con el historial del usuario (§10, punto 1).
- `crates/adapters/src/binance_auth.rs`: el test contiene el vector oficial de prueba de firma HMAC de la
  documentación de Binance (valores de ejemplo públicos). Ya estaba en `main` antes de este trabajo. Un
  escáner de secretos los marcará: es esperado. Se eliminan con el resto de la ruta de Binance (L3N).

## 12. Acciones pendientes del usuario

- **U1 (WP-00):** clave de Kraken spot sin retirada; `.env` con permisos 600 y token de Telegram nuevo.
- **U2:** permitir `docs.kraken.com` en la red del entorno (instrucciones en §5.1).
- **U3:** ejecutar la prueba A (preflight y ventana completa) y enviar el resumen.
- **U4:** confirmar o cambiar el umbral de 50 operaciones.
- **U5:** decidir sobre los cambios propuestos a `AGENTS.md`.
- **U6:** nivel de verificación de Kraken (límites de tasa, WP-13).
- **U7:** decidir cómo reconciliar la rama `feat/kraken-eur-spot-portfolio` con el refactor, o si se
  descarta (§11).

Kraken Prop queda fuera del sistema: sus órdenes son manuales.

## 13. Referencias

- `docs/plan/refactor-local-kraken.md` (v4): estrategia, decisiones D1–D12, fases y prueba A.
- `docs/plan/implementacion-kraken-spot.md`: paquetes de trabajo WP-00 a WP-17 y compuertas.
- `docs/ROADMAP.md`: estado frente a las compuertas.
- `docs/capital-scaling.md`: dimensionado y tapas para la cuenta spot.
- `docs/architecture/13-run-registry.md`: tablas del registro SQLite.
- `scripts/gate_a.sh`: la prueba A.
- `AGENTS.md`: reglas de ingeniería del repositorio.
- `docs/archive/README.md`: qué hay en el archivo y por qué.
