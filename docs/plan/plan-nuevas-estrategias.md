# Plan: preparar ATSNT para alojar nuevas estrategias (fricciones + molde completo)

## Contexto

Revisé `main` (`d993d3d`): 569 pruebas pasan, y clippy y fmt están limpios. El repo ya registra una
estrategia con una línea, pero el código genérico conserva reglas fijas del swarm, y el modelo de
ejecución admite un solo tipo de estrategia: una entrada triple-barrera, un símbolo, una posición,
sin salida por señal y solo con trades como entrada.

Decisiones del usuario (2026-11-19):
- **Alcance completo**: corregir las fricciones y además ampliar el molde (salida por señal,
  contexto de posición y multi-activo).
- **Barrera temporal**: queda una sola, un plazo en ms (`deadline_ms`); `max_bars_hold` desaparece.
- **Gobernanza**: se reabre la decisión fija «Solo `swarm_consensus`» de `HANDOFF.md` §4.

Criterio que atraviesa todas las fases: **el swarm es el caso N=1**. `golden_swarm`,
`golden_execution`, `gate_parity` y `parity` no cambian en ninguna fase. Si un golden cambia, la fase
se detiene y se explica el cambio antes de reescribirlo con `UPDATE_GOLDEN=1`. Cada fase es un PR en
borrador sobre `claude/upbeat-albattani-sg5vr7`, o una rama derivada por fase, que pasa
`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` y
`cargo fmt --all -- --check`. Se respetan las reglas R1–R10 de `AGENTS.md`.

---

## Fase 0 — Gobernanza y documentación (PR pequeño)

- `HANDOFF.md` §4: sustituir «Solo `swarm_consensus`» por «Se admiten varias estrategias registradas;
  el swarm sigue siendo la referencia de los goldens». Añadir una sección de actualización con fecha
  que enlace este plan.
- `README.md` (Status, Architecture) y `docs/ROADMAP.md`: reflejar la reapertura y listar las fases.
- Arreglar la documentación desfasada:
  - la cabecera de `crates/strategies/src/registry.rs` habla de una «market folder» que ya no
    existe;
  - `docs/architecture/12-adding-a-strategy.md` cita `strategy.rs:134`, pero el trait está en la
    línea 74. Citar por nombre de símbolo, no por número de línea.

---

## Fase 1 — Fricciones del contrato y del optimizador

### 1.1 Warm-up, calibración y config compartida declarados por la estrategia
En `OptimizableStrategy` (`crates/strategies/src/strategy.rs`):
```rust
/// Data a run must give the strategy before its first evaluated decision (ms). Required.
fn warm_up_ms(shared: &Self::SharedConfig) -> i64;
/// End of the data the shared configuration was calibrated on; evaluated data starts after it.
fn calibration_end_ms(_shared: &Self::SharedConfig) -> Option<i64> { None }
/// Whether a run must be given the shared configuration explicitly (calibrated strategies).
fn requires_shared_config() -> bool { false }
```
- Swarm (`crates/strategies/src/swarm_consensus/mod.rs`):
  - `warm_up_ms` devuelve 70 días; la constante se traslada aquí desde
    `backtest/src/optimization.rs:30`;
  - `calibration_end_ms` devuelve `Some(shared.calibration_end_ms)`;
  - `requires_shared_config` devuelve `true`.
- `crates/backtest/src/optimization.rs`: quitar `TRADES_WARM_UP_MS` y la función
  `calibration_end_ms<T: Serialize>`, que lee el JSON por nombre de campo. `check_calibration` se
  conserva.
- `crates/backtest/src/bin/run_hpo.rs`:
  - usar `S::warm_up_ms(shared)` en `after_warm_up` (l. 247) y en el mensaje;
  - usar `S::calibration_end_ms(shared)` en l. 245;
  - exigir `--shared-config` solo si `S::requires_shared_config()` (l. 330).
- `crates/backtest/src/registry.rs` (`LoadedStrategy`): nuevo campo
  `calibration_end_ms: Option<i64>`, que el visitor rellena con el `S` concreto.
  `crates/backtest/src/main.rs:173` lo lee de ahí, no del JSON.
- `crates/application/src/error.rs:25`: en `InsufficientWarmUp`, «the swarm» pasa a «the strategy».
  Actualizar la aserción de `optimization.rs:1364`.
- Re-exports de `crates/backtest/src/lib.rs`: quitar `TRADES_WARM_UP_MS` y `calibration_end_ms`.

### 1.2 Barrera temporal única en ms
- `crates/domain/src/order.rs`:
  - `OrderIntent` sustituye `max_bars_hold: usize` por un `deadline_ms: i64` obligatorio, que deja
    de ser `Option`;
  - los constructores (`new`, `with_risk`, `with_risk_and_trailing`, `with_risk_and_exits`,
    `limit`) reciben `max_hold_ms` y validan `deadline_ms > timestamp`;
  - nuevo error `DomainError::InvalidTimeBarrier`;
  - los artefactos guardan configuraciones, no intents, así que el cambio de serde no rompe ninguno
    (verificarlo con grep sobre `configs/` y sobre los goldens antes de cambiarlo).
- `crates/domain/src/decision.rs`: `ProtectiveExits` pierde `max_bars` y `deadline_ms` pasa a ser
  obligatorio.
- `crates/application/src/execution_core.rs` (`barrier_exit`, l. 221): quitar `bars_held` y
  comprobar solo `bar.end_ms >= deadline`. Retirar `bars_held` de `BacktestEngine` (`engine.rs:181`).
- `crates/application/src/session.rs:661` y `live_exits.rs`: el deadline deja de ser opcional.
- El swarm ya usa un plazo (`policy.rs:411-426`), así que los goldens no cambian. Reescribir los
  tests que usan barras en `engine.rs`, `execution_core.rs`, `gate.rs`, `risk.rs`,
  `simulated_venue.rs`, `tests/tick_simulation.rs`, `tests/gate_parity.rs` y
  `tests/trade_excursion.rs`, pasando a ms.
- Test nuevo de paridad: una intención con plazo cierra por `TimeBarrier` en el mismo instante en la
  ruta de barras y en la de tick.

### 1.3 Suite de contrato que no pueda pasar en vacío
`crates/strategies/src/contract.rs`:
- La entrada sintética cubre `warm_up_ms(default shared) + 400 min`. La parte de warm-up se genera a
  un trade por minuto (`synthetic_trades` acepta una densidad) para acotar el tiempo.
- Nueva aserción: **al menos una decisión** con la configuración por defecto, más una advertencia
  explícita en el mensaje si no la hay.
- Si el swarm no decide sobre la cinta sintética con su config por defecto, añadir el hook
  opcional `fn contract_fixture() -> Option<(SharedConfig, Config)>` (por defecto `None`) para darle
  umbrales que decidan. Medir el tiempo en debug: el objetivo es menos de 10 s.
- Comprobar que `warm_up_ms >= 0` y que, si hay calibración, `calibration_end_ms` es coherente con
  el `Default`.

### 1.4 Restos del swarm en código genérico
- `crates/backtest/src/registry.rs:22` (`DEFAULT_STRATEGY_ID`): eliminarlo. `strategy_id_of`
  (`registry.rs:220`) falla con `unknown_strategy_message` si ni la CLI ni el artefacto nombran una
  estrategia. Mantener un fallback solo si el registro tiene exactamente una.
- `run_hpo.rs`: `--strategy` pasa a ser obligatorio y la salida por defecto pasa a
  `configs/hpo_<id>_results.json`.
- Revisar `scripts/gate_a.sh` (pasa `--config` con artefacto, que ya nombra la estrategia) y
  `tests/swarm_runners.rs` (añadir `--strategy` donde falte).
- `docs/architecture/12-adding-a-strategy.md`: documentar los tres hooks nuevos, el plazo en ms y la
  obligación de añadir un golden propio (patrón de `tests/golden_swarm.rs` y `tests/support/swarm.rs`).

---

## Fase 2 — Decisiones ricas: contexto de posición y salida por señal

### 2.1 Contrato
En `crates/domain/src/decision.rs`:
```rust
pub enum Decision { Enter(OrderIntent), Exit { reason: &'static str } }
/// What the strategy may see of its own book when it decides (read-only, no account balances).
pub struct StrategyView<'a> { pub position: Option<PositionView>, pub now_ms: i64, .. }
```
- `Strategy::on_event(&mut self, event, view: &StrategyView, out: &mut Vec<Decision>)`. El búfer lo
  reutiliza el runner: no hay reserva de memoria por trade, porque las cintas tienen cientos de
  millones de trades.
- `OptimizableStrategy::on_features` recibe la misma firma, y la regla de dos etapas sigue igual.
- `FeatureExtractor` no cambia: las features no dependen de la posición. El contrato lo exige
  comparando dos etapas frente a una con la misma vista.
- El swarm emite solo `Enter`, en una adaptación mecánica. `Box<S>` reenvía.

### 2.2 Ejecución de la salida por señal
- Barras: `ExecutionCore` añade `signal_exit(...)`, una orden de mercado con deslizamiento y comisión
  taker y motivo `"SignalExit"`. `BacktestEngine::process_event` deja de descartar decisiones con
  posición abierta (`engine.rs:336`): aplica `Exit`. Un `Enter` sobre una posición abierta se
  rechaza y se cuenta como `PositionAlreadyOpen` en `rejected_orders`, en lugar de descartarse sin
  dejar rastro.
- Tick: `TradingSession` traduce `Exit` a `ExitManager::flatten` (ya existe para el deadline,
  `live_exits.rs:258-277`) con un `ExitEvent::SignalExitClosed` nuevo.
- `domain::metrics`: añadir la constante de motivo `SIGNAL_EXIT`.
- Tests:
  - estrategia de prueba que entra y sale por señal, con el mismo número de operaciones y los mismos
    motivos en ambas rutas, en un test nuevo `tests/signal_exit_parity.rs`;
  - contrato: un `Exit` sin posición es un no-op y no un error.

---

## Fase 3 — Dominio multi-activo

### 3.1 Instrumentos
- `crates/domain/src/instrument.rs`, nuevo:
  - `InstrumentId` (newtype sobre `String`, validado);
  - `InstrumentSpec { id, market: MarketType, min_quantity, quantity_step, min_notional, tick_size }`.
- Esto sustituye los filtros de lote globales de `RiskPolicy` (`risk.rs`, defaults 0.001/0.001/5),
  que el ROADMAP señala como gap. `RiskPolicy::with_execution_rules` pasa a tomar un `&InstrumentSpec`.
- `MarketEvent::Trade { instrument: &InstrumentId, trade: &Trade }`. `Trade` no cambia: la cinta
  Parquet sigue sin columna de símbolo y el instrumento lo pone el adaptador.
- `OrderIntent` y `Decision::Exit` llevan `instrument: InstrumentId`.
- `ClosedTrade` (`domain/src/metrics.rs:11`) gana `instrument`. La tabla `trades` de SQLite ya tiene
  `symbol` (`sqlite_runs.rs:88`) y se rellena por trade, no por run.

### 3.2 Cartera y gate de cuenta
- `crates/domain/src/book.rs`, nuevo: `Book { positions: BTreeMap<InstrumentId, Position> }`, con
  exposición bruta y neta, número de posiciones abiertas y número por dirección. Es determinista
  (`BTreeMap`).
- `EntryGate::admit(intent, &AccountState)`, donde `AccountState` reúne equity marcada, pico diario,
  breaker y `&Book`. Añade comprobaciones de cartera que reutilizan los `RiskError` que ya existen:
  - `MaxPositionsExceeded` y `DirectionalLimitExceeded` (`risk.rs:44-48`, hoy sin uso);
  - `PositionLimitExceeded` pasa a contar la exposición agregada;
  - `RiskPolicy` gana `max_open_positions`, `max_same_direction` y `max_gross_exposure_pct`, con
    valores por defecto que equivalen a N=1 (1 posición) para no mover los goldens.
- El margen (`MarginRule`, `gate.rs:47`) se calcula sobre el saldo libre: equity menos el margen ya
  comprometido por el `Book`.
- `DailyBreaker` y `MonthlyGuard` siguen a nivel de cuenta, sin cambios.
- Como referencia de diseño, no para copiarlo sin más: el motor de cartera retirado en `e470238`
  (`git show e470238 -- crates/backtest/src/portfolio_engine.rs crates/domain/src/risk.rs`).

### 3.3 Alcance de la estrategia
- `Strategy::scope() -> StrategyScope { PerInstrument, CrossSectional }`.
  - `PerInstrument` (el swarm): el runner crea una instancia por instrumento, con la cuenta y el
    gate compartidos.
  - `CrossSectional`: una sola instancia ve todos los instrumentos y decide sobre cualquiera.
- `StrategyView` (fase 2) gana `book: &BookView` para las estrategias `CrossSectional`.

---

## Fase 4 — Runners multi-activo

### 4.1 Datos
- `ports::market_data`: `MarketDataStream::next_trade` pasa a devolver `(InstrumentId, Trade)`, o se
  añade `next_event`.
- `crates/adapters/src/storage/tape_parquet.rs`: nuevo `MergedTapeStream`, una fusión k-way por
  timestamp de varios directorios o ficheros, uno por instrumento, con desempate estable por
  `InstrumentId`. El instrumento se deduce del directorio (`data/historical/BTCUSDT`), igual que hoy
  `main.rs` deduce el símbolo del nombre.
- CLI: `--data` se puede repetir, o se acepta `--data SYMBOL=path`.

### 4.2 Ruta de barras
- `BacktestEngine` (`crates/backtest/src/engine.rs`) pasa de un solo `core: ExecutionCore` a
  `BTreeMap<InstrumentId, ExecutionCore>`, con caja y equity únicas y la marca a mercado sumada por
  instrumento con el último precio de cada uno.
- `PriceEvent` lleva el instrumento. Las barreras de cada posición se evalúan solo con eventos de su
  instrumento.

### 4.3 Ruta de tick y venue
- `ports::TapeVenue`: `on_trade(&InstrumentId, &Trade)`, `position(&InstrumentId)`, y
  `submit_market_order_at` con símbolo (`ExecutionVenue::submit_order` ya recibe `symbol`).
  `ExecutionReport` gana `symbol`.
- `crates/adapters/src/simulated_venue.rs`: el estado por instrumento queda en un mapa, con la
  cartera del monedero compartida.
- `crates/application/src/session.rs` (`TradingSession`):
  - `phase`, `ExitManager` y `recovery` pasan a ir por instrumento (`BTreeMap`);
  - el monedero, las comisiones, los breakers y el `ledger` quedan a nivel de cuenta;
  - `driver.rs` y `tick_simulation.rs` enrutan por instrumento.
- `crates/application/src/parity.rs` mide por instrumento y en conjunto.

### 4.4 Optimización y artefactos
- `optimization.rs`:
  - `Tape<F>` por instrumento sobre una línea temporal común de cubos;
  - `WalkForwardSplitter` corta por tiempo y no por índice de cubo, para que los folds coincidan
    entre instrumentos;
  - `run_tape_slice` alimenta el motor multi-activo.
- `artifact.rs`: `schema_version` "2.0" con `symbols: Vec<String>` e `instrument_specs`. Lee "1.0"
  como un solo símbolo y tiene tests de compatibilidad con los artefactos de `configs/`.
- `recording.rs` / SQLite: rellenar `trades.symbol` por operación. `runs.symbol` guarda la lista
  unida por comas, sin migración de esquema: el texto sigue siendo `TEXT NOT NULL`. Se documenta en
  `docs/architecture/13-run-registry.md`.
- `monte_carlo.rs` trabaja sobre los retornos de las operaciones y no necesita cambios; solo se
  verifica.
- `metrics.rs`: añadir un desglose por instrumento al informe.

### 4.5 Reglas de arquitectura
- Añadir a `tests/architecture_contract.rs` un test que impida a `domain` y `application` usar
  `HashMap` en el estado de la cartera, para que el orden sea determinista. Es una regla R11 nueva,
  se documenta en `AGENTS.md` y requiere el visto bueno del usuario porque cambia las reglas.

---

## Fase 5 — Entradas más allá de trades

- `MarketEvent::Funding { instrument, rate, time_ms }`: los datos existen
  (`crates/adapters/src/binance_funding.rs`, `funding_schedule_from_history`). Ambas rutas lo emiten
  a la estrategia además de aplicarlo a la cuenta.
- `MarketEvent::Bar` externa: **no**. Las estrategias construyen sus barras desde trades, y en
  noviembre se borraron las dollar bars porque nada las usaba.
- Libro de órdenes: **bloqueado por datos, no por código**. No hay ninguna fuente: la grabadora
  WP-10 de Kraken está parada porque GA no pasó. Se deja documentado en el ROADMAP como dependiente
  de WP-10. `MarketEvent` ya es `#[non_exhaustive]`, así que añadirlo después no rompe estrategias.

---

## Verificación (en cada fase)

1. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` y
   `cargo test --workspace`. El recuento de pruebas sube y no hay fallos.
2. Goldens sin diff: `cargo test -p backtest --test golden_swarm --test golden_execution`, sin
   `UPDATE_GOLDEN`.
3. Fase 1: `run_hpo --strategy <stub sin shared>` corre sin `--shared-config`, y con el swarm lo
   sigue exigiendo (`swarm_runners.rs`). El contrato falla con una estrategia que no decide nunca.
4. Fase 2: `tests/signal_exit_parity.rs` da el mismo número de operaciones y los mismos motivos en
   barras y en tick.
5. Fase 3–4:
   - **equivalencia N=1**: el backtest del swarm con un solo instrumento por la nueva ruta
     multi-activo da métricas idénticas a las previas, sobre el golden y sobre `gate_parity`;
   - test nuevo con dos instrumentos sintéticos y el límite `max_open_positions = 1`: el segundo
     `Enter` se rechaza como `MaxPositionsExceeded` en ambas rutas;
   - con el límite en 2, ambos operan y la equity es la suma.
6. Opcional, si los datos están en local: los ejemplos de `README.md` con
   `--data data/historical/BTCUSDT` dan las mismas cifras que la tabla de `docs/ROADMAP.md`
   (47/48 operaciones).

## Orden y tamaño estimado
F0 (docs) → F1 (≈1 PR mediano) → F2 (≈1 PR mediano) → F3 (dominio, PR mediano) → F4 (el grande:
conviene partirlo en 4.1+4.2, 4.3 y 4.4) → F5 (pequeño). Cada PR se fusiona antes de empezar el
siguiente.
