# Plan de refactorización: ATSNT local, swarm sobre futuros de Kraken

> **Estado:** propuesta v2. Sustituye a la v1 (commit `f1bfaf0`).
> **Base de código:** commit `50b4ee5`. Verificado en verde antes de escribir este plan: `cargo check --workspace --all-targets`, `cargo test --workspace`, `cargo fmt --check` y `cargo clippy --all-targets -- -D warnings`.
> **Fecha:** 2026-10-30. Solo documentación: el código no ha cambiado.

## Cambios respecto a la v1

- **Spot sale del alcance.** Se opera en futuros perpetuos lineales de Kraken (`PF_`, colateral multi-divisa).
- **Margen, liquidación y breakers en USD.** EUR solo para informes, salvo que elijas colateral en EUR.
- **Apalancamiento por defecto: 2x**, no 10x.
- **La prueba económica ya no es sobre comisiones.** El artefacto de swarm ya usa comisiones de tramo 1 de futuros, así que la prueba mide la estrategia.
- **Corregida la estimación de fricción de la v1.** Se basaba en un logbook cuyas cifras no cuadran (sección 2.4).

## 0. Resumen

**Objetivo.** Un bot local que opera swarm en perpetuos lineales de Kraken Futures, con el margen en USD, los informes en EUR y sin GCP.

**Lo que este plan no hace.** No crea ventaja. Y el venue ya no explica la pérdida del logbook: su run 9 ya usa comisiones de tramo 1 de futuros y aun así perdió dinero. Si la fase 1 falla, el siguiente paso es investigación de estrategia, no infraestructura.

**Orden de trabajo.**

1. **Fase 0:** higiene. Parte ya hecha (claves de Binance revocadas, acceso a GitHub).
2. **Fase 1:** puerta económica. Reproducir el run 9 y aplicar los parámetros de Kraken.
3. **Fase 2:** grabadora de futuros, en paralelo. El libro y el mark price no se pueden recuperar después.
4. **Fases 3 y 4:** hexagonal y borrado, protegidos por goldens de swarm.
5. **Fases 5 a 7:** dominio de futuros en USD, adaptador de Kraken Futures y fidelidad de simulación.
6. **Fases 8 y 9:** operación local y paso a dinero real por tramos.

## 1. Decisiones por defecto

Tomo estas decisiones para avanzar. Si quieres otra, dímelo y ajusto las fases afectadas.

| # | Decisión | Motivo | Cómo cambiarla |
|---|---|---|---|
| D1 | Solo `swarm_consensus`. Se eliminan las otras cuatro estrategias y el stack de portfolio y universo | Tu instrucción. El VM con el bot ya no existe | Volver a registrar una estrategia en `registry.rs` |
| D2 | Futuros perpetuos lineales de Kraken (`PF_`). Spot fuera de alcance | A 200 €/mes el spot queda en el tramo base y es caro. Los derivados tienen tramo 1 de 0,02 % / 0,05 % (sección 2.4) | Reabrir spot como fase posterior |
| D3 | Largo y corto, como permiten los perpetuos | Sin restricción de venta en corto | — |
| D4 | Swarm en modo `classifier` para paper y live | El modo legacy emite `Decision::Target` y el registro lo rechaza (`crates/backtest/src/registry.rs:138`) | Ninguna: es una restricción actual del código |
| D5 | Binance deja de ser fuente de datos y de ejecución al cerrar la fase 1 | Su único uso restante es el histórico público de la fase 1 | Mantener un descargador público si quieres comparar venues |
| D6 | Parquet para trades, libro y mark price; SQLite para el registro de runs | Sin servidor ni cloud. SQLite cubre lo que hoy hace BigQuery | DuckDB para análisis, sin cambiar el diseño |
| D7 | Telegram se mantiene | No es GCP y ya es un puerto | — |
| D8 | Linux con unidad systemd de usuario | El repo ya trae `docs/deployment/*.service`, `journald.conf` y envoltorios `systemd-inhibit` | Si usas macOS, launchd; las fases no cambian |
| D9 | Se añade el puerto `Clock` | AGENTS.md lo exige; el roadmap lo descartó. Se corrige el roadmap | — |
| D10 | Margen, liquidación, breakers y riesgo en **USD**. Informes en **EUR** | Las pérdidas de `PF_` siempre se cargan en USD (sección 2.7) | Colateral en EUR: decisión tuya, con sus costes (sección 2.7) |
| D11 | Salidas gestionadas por el cliente (en dominio) | No he verificado órdenes condicionales en el API de futuros | Usar órdenes condicionales si la especificación lo confirma |
| D12 | Apalancamiento máximo configurable, **por defecto 2x** | Kraken lista hasta 10x en la UE, pero la prensa cita un aviso de ESMA que trataría estos perpetuos como CFD, con tope de 2:1 para minoristas (sección 2.6) | Subir el tope en la configuración, solo si tu cuenta lo permite |
| D13 | La demo (`demo-futures.kraken.com`) sirve para fontanería, no para validar estrategia | Es un entorno de pruebas que se refresca de vez en cuando y no guarda histórico | — |

## 2. Hechos que condicionan el plan

### 2.1 GCP y datos

- El proyecto se eliminó. Google permite restaurarlo durante 30 días con `gcloud projects undelete` ([documentación](https://docs.cloud.google.com/resource-manager/docs/delete-restore-projects)). No lo necesitamos.
- Se pierde el historial de runs de BigQuery y los artefactos de GCS que no se descargaron a `configs/`. Lo que importa está en git: `configs/hpo_swarm.json`, `configs/hpo_btcusdt_2026.json`, `configs/swarm_shared.json` y los logbooks de `docs/research/`.
- Los datos históricos de Binance son públicos y `fetch_data` los vuelve a descargar.

### 2.2 Secretos

- **Hecho:** claves de Binance revocadas.
- **Pendiente:** recrear el token de Telegram y guardarlo en `.env`.
- **Nuevo:** claves de API de producción de futuros **sin permiso de retirada**, y claves del demo separadas. Todo en `.env` con permisos `600`.

### 2.3 Swarm tiene dos modos, y solo uno se puede operar

| Modo | Config | Decisión que emite | Artefacto de referencia | Paper y live |
|---|---|---|---|---|
| Legacy | `classifier_mode: false` (valor por defecto) | `Decision::Target`, niveles de -3 a 3 | `configs/hpo_swarm.json` (no tiene la clave, así que carga en legacy) | Rechazado por el registro |
| Classifier | `classifier_mode: true` | `Decision::Entry`, barrera triple | `configs/hpo_btcusdt_2026.json` | Aceptado |

El candidato para operar es el modo classifier. Ambos artefactos son `usd_m_perpetual`.

### 2.4 Comisiones y logbook

**Tarifas** (fuentes de búsqueda, **verificar en tu cuenta**):

| Producto | Tramo base | Primer escalón | Fuentes |
|---|---|---|---|
| Spot | 0,40 % maker / 0,80 % taker | 2,5 K USD de volumen de 30 días | [tarifas Kraken Pro](https://www.kraken.com/pro/fees), [comparativa](https://www.kraken.com/learn/lowest-fee-crypto-exchange) |
| Derivados | 0,02 % maker / 0,05 % taker | Tramo 1 hasta 5 M USD de volumen de 30 días | [tabla de derivados](https://support.kraken.com/articles/360048917612-fee-schedule) |

- Las fuentes no coinciden sobre si el volumen spot cuenta para el nivel de derivados.
- La prensa cita una promoción sin comisiones de futuros para clientes nuevos del EEE. No la uso en la prueba; compruébala en tu cuenta.

**Tu caso (200 €/mes):** en spot no bajas de tramo, porque el primer escalón empieza en 2,5 K USD. En derivados ya estás en el tramo 1. Tu intuición de que sale más barato es correcta para futuros, no para spot.

**El artefacto ya asume estas comisiones.** `maker_fee_pct: 0.0002` y `taker_fee_pct: 0.0005` son el tramo 1 de derivados. Por eso el run 9 del logbook ya es swarm con comisiones de Kraken futuros, y aun así perdió dinero.

**El logbook no cuadra consigo mismo** (`docs/research/01-swarm-consensus-optimization-logbook.md`, sección 5.2):

- La fricción por razón de salida suma unos 45 USD: take profit 10,45 (109,79 bruto frente a 99,34 neto), trailing 2,17 (0,97 frente a −1,20) y stop loss 32,62 (−151,42 frente a −184,04).
- Pero el resumen declara una fricción total de 91,69 USD.
- Y −40,66 − 91,69 = −132,35, no −85,89.

**Por eso se retira la estimación de 4,5 y 8,5 veces de la v1.** Se basaba en 91,69. La fase 1 la sustituye por cifras del backtester.

### 2.5 Kraken en el código

No hay ninguna referencia a Kraken. Pero el código ya modela perpetuos USD-M: `domain/src/futures.rs` tiene funding, liquidación aislada y mínimos de apalancamiento. El trabajo de futuros es parametrizar y adaptar, no construir desde cero.

### 2.6 Acceso a futuros en España

- Kraken ofrece perpetuos en la UE. Sus páginas en español dicen que la plataforma cumple MiFID II en España ([página](https://pro.kraken.com/es/perpetual-futures)).
- **Requisito:** pasar el test de idoneidad de MiFID II y activar derivados en tu cuenta de Kraken Pro.
- **Apalancamiento:** Kraken declara hasta 10x para usuarios de la UE en pares elegibles. La prensa especializada informa de un aviso de ESMA sobre perpetuos apalancados (febrero; verificar fecha y texto). Según esa prensa, si se tratan como CFD, el tope para minoristas sería 2:1. No he encontrado confirmación de que Kraken lo haya aplicado.
- Un artículo de Kraken Learn de junio de 2026 indica que el país cambia la elegibilidad, el apalancamiento y los contratos listados (URL no confirmada; buscarlo en Kraken Learn antes de citarlo).

### 2.7 Colateral y divisa de liquidación

- Los contratos lineales multi-colateral (`PF_`) denominan sus PnL en **USD**. El pago por defecto es USD, pero puede cambiarse a otro colateral soportado, EUR entre ellos, con haircuts distintos por activo ([acuerdos EEE](https://support.kraken.com/articles/multi-collateral-derivatives-contracts-eea), [especificación lineal](https://support.kraken.com/hc/en-us/articles/4844359082772-Linear-Multi-Collateral-Perpetual-Contract-Specifications)).
- **Las pérdidas siempre se cargan en USD.** Si falta USD, se cobra comisión sobre el saldo convertido.
- El funding puede liquidarse en otra divisa según el contrato. Verificarlo en la especificación de cada `PF_`.

**Consecuencia para el diseño:** margen, liquidación y breakers se calculan en USD. EUR sirve para informes. Mantener un colchón de USD evita las conversiones. Si eliges colateral EUR, se añaden haircuts, exposición FX en el margen y la comisión de conversión, que se modela como fricción.

### 2.8 Violaciones hexagonales (verificadas con grep)

| Dónde | Problema | Acción |
|---|---|---|
| `backtest/src/{paper,tick_simulation,recording,parity}.rs` | Importan `adapters::` desde la capa de aplicación | Dependen de puertos; el cableado va a `wiring.rs` (fase 3) |
| `backtest/src/{sinks,artifact}.rs` | Sinks de BigQuery y GCS | Se eliminan (fases 3 y 4) |
| `adapters/src/analytics.rs` | Define el trait de persistencia en la capa de adaptadores | Se mueve a `ports/` (fase 3) |
| `ports/src/execution.rs`, `ports/src/lib.rs` | Nombran Binance en comentarios | Se corrige (fase 3) |
| `domain/src/futures.rs` | Lógica pura correcta, pero el intervalo de funding está fijado en 8 h | Se parametriza (fase 3). Pasa a ser el módulo central |
| `backtest/src/{daily_watermark,excursion,fee_ledger,monthly_guard,execution_core,metrics,walk_forward,price_event}.rs` | Reglas puras dentro de la capa de aplicación | Se mueven a `domain` si el grep confirma que no usan E/S (fase 3) |
| `backtest/src/monte_carlo.rs` | El grep de E/S lo marca por `rayon` | Separar: el núcleo estadístico va a `domain` con RNG determinista; el paralelismo queda en la aplicación |
| `backtest/src/{driver,session}.rs` | Orquestación | **Se quedan en la capa de aplicación.** No son dominio |
| `backtest/src/live_exits.rs` | Salidas gestionadas por el exchange, específicas de Binance | Se revisa con D11 (fase 6) |

### 2.9 Unidades de los campos de comisión

Los campos `*_pct` son fracciones: `taker_fee_pct: 0.0005` equivale a 0,05 %. Un 0,40 % escrito como `0.40` serían 40 %. Se renombran a `*_rate`, se validan al cargar en el rango `[0, 0.05]` y se migran los artefactos que se conservan.

## 3. Arquitectura objetivo

```text
crates/
├── domain/      Puro, sin E/S. Instrumentos de futuros, moneda y Money (monto + divisa),
│                tarifas, órdenes y su máquina de estados, posición, riesgo, margen y
│                liquidación (futures.rs), funding con intervalo y divisa parametrizados,
│                contabilidad (comisiones, drawdown diario y mensual en USD), analítica
│                (métricas, Monte Carlo con RNG determinista).
├── strategies/  Solo swarm_consensus, su contrato de pruebas y un registro con una entrada.
├── ports/       MarketDataStream, AsyncMarketDataStream, DepthStream*, MarkPriceStream*,
│                FundingStream*, TradeTapeWriter*, Clock*, ExecutionVenue, ExitGateway,
│                InstrumentCatalog*, FeeSource*, FxRateSource*, trait de persistencia de runs
│                (movido), AlertNotifier.                                   (* = nuevo)
├── adapters/    kraken_futures/  REST (público y privado), WebSocket v1, firma, catálogo,
│                                 gateway, grabadora
│                storage/         Parquet de trades, libro y mark price; SQLite del registro
│                telegram/        (movido desde gcp/)
│                simulated/       SimulatedVenue, libro simulado y mark price simulado
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

- **Hecho:** claves de Binance revocadas. Acceso a GitHub concedido. El plan v1 ya está subido.
- **Pendiente:**
  - Activar derivados en Kraken Pro. Requiere pasar el test de idoneidad de MiFID II.
  - Crear una cuenta en el demo (`demo-futures.kraken.com`) y sus claves, separadas de las de producción.
  - Crear las claves de producción de futuros **sin permiso de retirada**.
  - Recrear el token de Telegram.
  - Crear `.env` con permisos `600`.
- Confirmar el sistema operativo. Si no es Linux, decirlo antes de la fase 8.

**Salida:** `.env` local creado. Derivados activos en la cuenta. Claves del demo y de producción separadas.

### Fase 1: puerta económica (S de código, horas de cómputo)

**Pregunta:** ¿swarm tiene ventaja neta con comisiones de futuros, funding y apalancamiento de 2x?

**Paso 0. Verificar los contratos con datos públicos.** Ejecuta esto en tu equipo, donde sí hay acceso a Kraken. Desde este entorno el proxy bloquea `futures.kraken.com` (403):

```bash
curl -s https://futures.kraken.com/derivatives/api/v3/instruments \
  | python3 -c "import json,sys; d=json.load(sys.stdin); [print(json.dumps(i, indent=1)) for i in d.get('instruments', []) if i.get('symbol') in ('PF_XBTUSD','PF_ETHUSD','PF_SOLUSD')]"
```

Pégame la salida. De ella salen el tamaño del contrato, el mínimo y el paso de orden, el tick y el apalancamiento máximo. Hasta entonces, esos valores quedan como **verificar**.

**Paso 1. Reproducir el run 9 sin fiarse de sus totales.** Reproducirlo con el backtester actual y comparar con el logbook. Antes hay que saber qué artefacto contiene sus parámetros: sus valores de breakeven y trailing (sección 5.2 del logbook) no aparecen en `hpo_swarm.json` ni en `hpo_btcusdt_2026.json`. Si no se reproduce, el logbook no sirve de referencia y solo queda la prueba.

**Paso 2. Aplicar los deltas de Kraken** en copias del artefacto:

| Delta | Artefacto actual | Prueba |
|---|---|---|
| Comisiones | maker 0,0002, taker 0,0005 | Sin cambio (ya son tramo 1 de derivados; verificar) |
| Apalancamiento | `max_leverage: 20` | `2` |
| Funding | `funding_rate_pct: 0.0001` por liquidación de 8 h | Histórico de funding de Kraken si se puede obtener (la página regional indica horario; verificar). Si no, barrido de tasas |
| Conversión | — | Si el colateral es EUR, comisión de conversión (verificar) |
| Tamaño mínimo | `min_order_quantity: 0.001` (BTC) | Mínimo real de `PF_XBTUSD` del paso 0 |

**Paso 3. Escenarios.** Tres ejecuciones: (a) con los deltas anteriores; (b) con el doble de comisiones como margen de seguridad; (c) con el doble de funding. Anotar PnL neto, comisiones, funding pagado, Sortino y DSR.

**Criterio:**

- **Pasa** si en el escenario (a) el PnL neto OOS es positivo y el DSR cumple el criterio del proyecto. El umbral lo fija `run_hpo`; verificarlo en el código antes de decidir.
- **No pasa.** El venue ya no explica la pérdida, así que el siguiente paso es investigación de estrategia, no infraestructura. El refactor puede seguir porque elimina código sin uso, pero no se construye ejecución con dinero real hasta que el gate pase.

**Salida:** tabla de escenarios completa, con el paso 0 verificado o marcado como pendiente, y decisión tomada.

### Fase 2: grabadora de futuros (M), en paralelo

**Por qué va pronto:** el libro y el mark price no se pueden pedir hacia atrás por REST. Cada día sin grabar es un día de calibración perdido.

Alcance:

- Puertos mínimos que necesita la grabadora: `DepthStream`, `MarkPriceStream`, `FundingStream`, `TradeTapeWriter` y `Clock`.
- `adapters/src/kraken_futures/ws_public.rs`: WebSocket v1 de producción en `wss://futures.kraken.com/ws/v1`, con los feeds públicos `trade`, `book`, `ticker` (verificar que el ticker trae mark price, index y funding) ([documentación WebSocket de futuros](https://docs.kraken.com/exchange/api-reference/futures-websocket)).
- **Verificar la integridad del libro en el feed v1.** No asumir el checksum CRC32 de spot. Si no hay checksum, validar por número de secuencia y resincronizar con snapshot ante cualquier hueco.
- `adapters/src/kraken_futures/catalog.rs`: instrumentos públicos (paso 0 de la fase 1) para tamaños, ticks y apalancamiento de cada `PF_`.
- Persistencia en Parquet por día UTC: `data/kraken-futures/<símbolo>/{trades,book,mark}/AAAA-MM-DD.parquet`, con un snapshot del libro cada 60 s más deltas.
- Funding histórico diario por REST público ([histórico de funding](https://docs.kraken.com/api-reference/historical-funding-rates/historical-funding-rates)).
- Reconexión con backoff exponencial, heartbeat y rotación de ficheros a medianoche UTC.
- Salud cada hora: mensajes por segundo, huecos, reconexiones y fallos de secuencia o checksum. Alerta por Telegram si hay un hueco de más de 60 s.
- Binario temporal `kraken_record`. Pasa a `atsnt record` en la fase 10.

**Salida:** 7 días seguidos sin huecos sin alertar, y todo fallo de integridad del libro resincronizado.

### Fase 3: consolidación hexagonal con goldens de swarm (L)

Orden obligatorio: primero los goldens, después los movimientos.

- **3.1 Goldens de swarm.** Añadir casos de swarm (bar y tick, en modo classifier y en legacy) en `crates/backtest/tests/golden_execution.rs`. Generarlos con `UPDATE_GOLDEN=1 cargo test -p backtest --test golden_execution`, revisar el diff y commitear. Hoy los goldens de estrategia son de cusum y ofi; esos se borran en la fase 4. Los de registro se conservan.
- **3.2 Reglas R2 a R7** en `architecture_contract.rs`. Fallarán hasta completar 3.3 a 3.10.
- **3.3 Persistencia.** Mover el trait de persistencia de `adapters/src/analytics.rs` a `ports/src/`. `recording.rs` queda como traducción del dominio al puerto.
- **3.4 Sin `adapters::` en la librería.** `paper.rs`, `tick_simulation.rs` y `parity.rs` reciben puertos genéricos. El cableado concreto se mueve a `wiring.rs`.
- **3.5 Reglas puras al dominio.** `daily_watermark`, `excursion`, `fee_ledger`, `monthly_guard`, `execution_core`, `metrics` y `walk_forward` si no tiene E/S. Antes de mover cada módulo, comprobar con grep que no contiene `std::fs`, `tokio`, `rayon`, `rand` ni `SystemTime`.
- **3.6 Funding.** `FundingSchedule::new(intervalo, tasa, divisa)`. La constante de 8 h queda solo en el perfil histórico de Binance, que se borra en la fase 4.
- **3.7 Clock.** `SystemClock` en `adapters`; `SimClock` para simulación y tests. Corregir `docs/ROADMAP.md`, que dice que el puerto no hace falta.
- **3.8 Puertos sin nombres de exchange.** Quitar las menciones de Binance en `ports/src/execution.rs` (`order_id`, documentación de `ExecutionVenue`) y en `ports/src/lib.rs`.
- **3.9 Renombrar `tape`.** `binance_parquet.rs` pasa a `storage/tape_parquet.rs`, genérico, con el mismo esquema y con columnas para libro y mark price.
- **3.10 Divisas explícitas.** Introducir `Currency` y `Money` en `domain`. Ningún importe se suma sin divisa; las funciones de margen y breakers reciben USD.

**Salida:** `fmt`, `clippy -D warnings` y tests en verde. Goldens idénticos bit a bit. R2 a R7 pasan.

### Fase 4: eliminar estrategias, portfolio y Binance (M)

La lista completa está en la sección 5. Incluye el código de Binance que ya no se usa: gateway, streams, firma, fixtures y el descargador histórico, que solo sirvió para la fase 1.

**Salida:** `strategy_ids()` devuelve solo `swarm_consensus`. Ningún archivo fuera de swarm nombra otra estrategia (grep vacío). Tests en verde.

### Fase 5: dominio de futuros en USD (M)

- `domain/src/instrument.rs`: `Instrument` con símbolo (`PF_…`), colateral y divisa de liquidación (USD), tamaño de contrato, tick, paso y mínimo de orden, apalancamiento máximo y funding por intervalo y divisa. Todo en `Decimal`.
- `domain/src/futures.rs` (central): liquidación con **mark price**, no con último precio; funding por intervalo configurable; margen según la especificación del `PF_` (verificar si el margen es aislado o cruzado y los haircuts de colateral no USD).
- `domain/src/fees.rs`: `FeeTier { maker_rate, taker_rate }` como datos, validados en rango. Comisión de conversión de divisa como fricción explícita.
- **Apalancamiento.** `RiskPolicy` aplica un tope configurable, por defecto 2x (D12). Rechaza cualquier orden que lo supere.
- **Breakers y contabilidad en USD.** Informes en EUR a partir de `FxRateSource` (puerto).
- **Ledger completo.** Cada fill, comisión, pago de funding y conversión queda registrado. Lo necesitarás para tus declaraciones en España.
- Renombrar `*_pct` a `*_rate` (sección 2.9) y migrar los artefactos que se conservan.

**Pruebas:** liquidación con mark price; funding con intervalo configurable; redondeo al tick y al contrato; tope de apalancamiento; breakers en USD; conversión a EUR determinista con tasa fija; comisión de conversión aplicada solo cuando hay conversión.

**Salida:** tests de cada regla en verde. La configuración nueva se valida al cargar.

### Fase 6: adaptador de Kraken Futures (L)

- **6.1 Catálogo.** Instrumentos públicos → `Instrument`. Solo se operan los `PF_` que tu cuenta tenga habilitados en la UE.
- **6.2 Histórico.** Funding histórico por REST público. Para trades y libros históricos no he verificado un endpoint; si no existe, la grabadora de la fase 2 es la única fuente.
- **6.3 Firma REST.** Según la [guía de Derivatives REST](https://docs.kraken.com/api/docs/guides/futures-rest/):
  - Cabeceras: `APIKey` (la clave pública, no el secreto), `Authent` y `Nonce` (opcional).
  - `Authent` = base64( HMAC-SHA512( clave = base64decode(secret), mensaje = SHA256(postData + Nonce + endpointPath) ) ).
  - **No compartir código con spot**: spot pone la ruta antes del digest y usa API-Sign.
  - **Lo que la guía no deja claro:** cómo se codifica `postData`, qué parte de la URL es `endpointPath`, si el digest entra como bytes crudos o como hex, y qué se hashea cuando no hay cabecera `Nonce`.
  - **Oráculo:** el demo. Se validan las firmas con respuestas reales del demo antes de enviar nada a producción.
  - El nonce debe crecer por clave: milisegundos más un contador protegido.
- **6.4 REST privado.** Saldos y colateral, envío de órdenes (mercado y límite, con `reduceOnly`), cancelación, órdenes abiertas, fills y posiciones, para conciliar. Verificar los nombres exactos de los endpoints en la guía.
- **6.5 WebSocket privado.** Autenticación por challenge según la documentación del WS v1 (verificar). Feeds de fills, órdenes y saldos.
- **6.6 `KrakenFuturesGateway`** implementa `ExecutionVenue` y entra en el `TradingSession` existente. `live_trading.rs` se reescribe con él.
- **6.7 Límites de tasa.** Medirlos en el demo y limitar por endpoint con un cubo de fichas conservador.
- **6.8 Pruebas.** HTTP y WebSocket mockeados: AGENTS.md prohíbe la red en tests. El demo se usa en una prueba manual separada, no en la suite.

**Salida:** tests de firma en verde con respuestas reales del demo guardadas como fixtures. Ciclo completo en el demo: enviar, consultar, cancelar y conciliar.

### Fase 7: fidelidad de simulación (L)

AGENTS.md exige fills realistas. Hoy el simulador no tiene libro ni mark price (ver los "known gaps" del roadmap).

- **Mark price.** Los disparos y la liquidación usan mark price grabado, no último precio.
- **Funding** cobrado en cada liquidación, con el intervalo y la divisa del contrato.
- **Liquidación** según la especificación del `PF_`.
- **Market orders** recorren el libro L2 grabado. **Limit orders** usan un modelo de cola y solo se ejecutan cuando un trade atraviesa el nivel.
- **Comisiones.** `FeeSource` lee la tarifa de la cuenta, y simulación y ejecución usan la misma.
- **Latencia** medida con `parity_check` sobre órdenes reales mínimas.

**Salida:** `parity_check` con desviación explicada durante al menos 2 semanas de paper.

### Fase 8: operación local (M)

Estructura en tu equipo:

```text
~/atsnt/
├── .env                 (600) claves de producción, claves del demo y Telegram
├── configs/             artefactos versionados en git
├── data/kraken-futures/ Parquet de trades, libro, mark price y funding
├── state/atsnt.sqlite   registro de runs y ledger (WAL)
└── backup/              copias nocturnas
```

- **Registro.** SQLite con `runs`, `run_metrics`, `trades`, `run_series` y el ledger de fills, comisiones, funding y conversiones. Vistas `v_runs`, `v_trades` y `v_equity`. Reescribir `docs/architecture/13-bi-data-contract.md`, que hoy describe BigQuery.
- **Servicio.** Unidad de usuario `~/.config/systemd/user/atsnt.service` con `Restart=on-failure` y `KillSignal=SIGINT`, como la actual. `loginctl enable-linger` para que arranque sin sesión abierta. Desactivar la suspensión del equipo; requiere permisos de administrador y depende de tu sistema operativo.
- **Copias.** `sqlite3 state/atsnt.sqlite ".backup …"` cada noche, y copia de `data/` a un disco externo. Medir el tamaño tras la fase 2.
- **Alertas.** Telegram se mantiene y se mueve a `adapters/telegram/`.
- **Informes.** `atsnt report` sustituye las consultas de BigQuery.
- **Borrar.** `Dockerfile`, `.dockerignore`, `scripts/gcp/`, `docs/architecture/10-gcp-cloud-architecture.md`, las variables `GCP_*` y los flags `--gcp-bigquery`, `--strict-sinks` y `--upload-winner`.

**Salida:** el servicio se reinicia tras matar el proceso. Una copia nocturna se restaura en una prueba. Una parada genera alerta.

### Fase 9: paper y dinero real por tramos (calendario, no código)

- **Paper:** al menos 4 semanas con datos de producción grabados y el simulador de la fase 7. El demo solo valida el ciclo de órdenes.
- **Pasar a real** solo si el DSR cumple, el PnL neto OOS es positivo con comisiones y funding reales, y la paridad está dentro de tolerancia.
- **Apalancamiento de 2x.** Con 500 € son unos 1.000 € de nocional. Comprobar que el mínimo de orden de `PF_XBTUSD` cabe en ese nocional (paso 0 de la fase 1). Si no cabe, elegir otro `PF_` o aumentar el capital.
- **Tramo inicial** en el mínimo de orden verificado. Escalar según `docs/capital-scaling.md`, reescrito alrededor de 2x y margen en USD. Breakers diario y mensual activos.

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
| `crates/adapters/src/bin/{fetch_data,stream_trades,live_gateway}.rs` | Borrar; sustituidos por `kraken_record` y el gateway de futuros | 4 |
| `crates/adapters/tests/fixtures/binance_private_events/` | Borrar | 4 |
| `crates/backtest/src/bin/live_trading.rs` | Reescribir con `KrakenFuturesGateway` | 6 |
| `crates/adapters/src/gcp/*` salvo `telegram.rs` | Borrar; mover `telegram.rs` a `adapters/src/telegram/` | 3 |
| `crates/adapters/src/analytics.rs` | Mover el trait a `ports/` | 3 |
| `crates/backtest/tests/golden/*cusum*`, `*ofi*` y `hpo_cusum_ranking.json`, con sus casos | Borrar | 4 |
| Tests que prueban código borrado (`strict_sinks_cli.rs`, partes de `recording_contract.rs`) | Borrar, o reescribir con su código | 4 |
| `configs/` salvo `hpo_swarm.json`, `hpo_btcusdt_2026.json` y `swarm_shared.json` | Borrar (daytrading, langevin, quantum, matrix, portfolio, universe y execution_*). Añadir `kraken_futures_eur.json` | 4 y 5 |
| `configs/hpo_results.json` | Revisar su contenido antes de decidir | 4 |
| `scripts/gcp/`, `scripts/run_production_pipeline.sh`, `scripts/run_joint_portfolio_pipeline.sh`, `scripts/tests/` | Borrar; los sustituye `atsnt` | 8 |
| `Dockerfile`, `.dockerignore` | Borrar | 8 |
| `docs/strategy/{01,02,04,05}*.md`, `docs/research/02-*`, `SWARM_OPTIMIZATION_ROADMAP.md` | Mover a `docs/archive/` | 4 |
| `docs/architecture/10-gcp-cloud-architecture.md` | Borrar | 8 |
| `docs/architecture/{07,08,13}*.md`, `docs/capital-scaling.md`, `docs/deployment/atsnt.service`, `README.md`, `AGENTS.md`, `docs/ROADMAP.md`, `.env.example` | Reescribir (08 pasa a describir el gateway de futuros de Kraken) | 3, 5 y 8 |
| `odd/tasks/` | Conservar como historial | — |

## 6. Pruebas y puertas por fase

| Fase | Pruebas nuevas | Puerta de salida |
|---|---|---|
| 1 | Verificación de contratos (paso 0); reproducción del run 9 | Tabla de escenarios completa |
| 2 | Parseo de mensajes WebSocket v1 con fixtures; verificación de secuencia | 7 días sin huecos sin alertar |
| 3 | Goldens de swarm; reglas R2 a R7; `Money` sin mezcla de divisas | Goldens idénticos bit a bit |
| 4 | `strategy_ids()`; grep de nombres de estrategias | Tests en verde y sin referencias residuales |
| 5 | Liquidación con mark price; funding configurable; tope de 2x; breakers en USD; conversión a EUR | Cada regla con test |
| 6 | Firma con respuestas reales del demo; paginación con HTTP mockeado | Ciclo completo en el demo |
| 7 | Liquidación y disparos con mark price; fills de market contra libro sintético; cola de limit | `parity_check` dentro de tolerancia |
| 8 | Reinicio tras matar el proceso; restauración de una copia | Alerta al parar |
| 9 | — | Criterios de la fase 9 |

En todas las fases: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` y `cargo test --workspace`. Ningún test toca la red.

## 7. Riesgos

| Riesgo | Impacto | Mitigación |
|---|---|---|
| La estrategia no tiene ventaja neta | Alto: no hay producto | Fase 1 antes de construir ejecución. Sin OOS positivo y DSR, no hay dinero real |
| Kraken o la CNMV/ESMA limitan el apalancamiento a 2:1 | Medio | Tope configurable; el diseño ya asume 2x |
| Cambio de elegibilidad de la cuenta o de contratos listados | Medio | Catálogo que se refresca; alerta si un `PF_` deja de estar disponible |
| Funding que se come el resultado | Medio | Funding real en la fase 1; contabilidad separada en el ledger |
| Conversión USD/EUR cara si el colateral es EUR | Medio | Colchón de USD por defecto; comisión modelada como fricción |
| La grabadora para (equipo dormido, red, disco) | Alto: se pierden datos del libro y del mark price | Alertas de hueco, copias y servicio con reinicio |
| El libro se desincroniza | Medio | Verificación de secuencia; resincronizar con snapshot |
| El refactor cambia el comportamiento | Alto | Goldens antes de mover |
| Clave API comprometida | Alto | `.env` con permisos `600`; sin permiso de retirada; restricción por IP si la cuenta lo permite |
| Cambios en la API de Kraken Futures | Medio | Respuestas reales del demo guardadas como fixtures; revisar el changelog antes de cada despliegue |
| Registro fiscal incompleto | Medio | El ledger guarda cada fill, comisión, funding y conversión desde el primer día |

## 8. Fuentes

- Restaurar proyectos de GCP: https://docs.cloud.google.com/resource-manager/docs/delete-restore-projects
- Comisiones de spot: https://www.kraken.com/pro/fees · https://www.kraken.com/learn/lowest-fee-crypto-exchange
- Comisiones de derivados: https://support.kraken.com/articles/360048917612-fee-schedule
- Perpetuos en España (MiFID II): https://pro.kraken.com/es/perpetual-futures
- Colateral y divisa de liquidación: https://support.kraken.com/articles/multi-collateral-derivatives-contracts-eea · https://support.kraken.com/hc/en-us/articles/4844359082772-Linear-Multi-Collateral-Perpetual-Contract-Specifications · https://support.kraken.com/articles/perpetual-contract-specifications-for-clients-in-the-eea
- Autenticación de futuros: https://docs.kraken.com/api/docs/guides/futures-rest/
- WebSocket de futuros: https://docs.kraken.com/exchange/api-reference/futures-websocket
- Histórico de funding: https://docs.kraken.com/api-reference/historical-funding-rates/historical-funding-rates
- Demo de futuros: https://docs.kraken.com/exchange/guides/futures/introduction
- Funding horario, página regional: https://www.kraken.com/gb/pro/perps/crypto-perpetuals/funding-rates
- Aviso de ESMA sobre perpetuos (prensa; verificar texto original): https://tradeinformer.com/regulations/esma-perpetual-futures-cfd-regulation-eu-2025

Todas las cifras de Kraken de este documento proceden de búsquedas o de la prensa, y deben verificarse en tu cuenta y en la documentación vigente antes de usarse.
