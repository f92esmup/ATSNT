# Strategy Specification: Non-Equilibrium Langevin Dynamics & Empirical Potential Wells

> **Estado**: En desarrollo (In Development)  
> **Fundamento Físico**: Mecánica Estadística Fuera del Equilibrio, Ecuación de Langevin sobreamortiguada, Ecuación de Fokker-Planck, Estimador de Densidad por Núcleo Exponencialmente Ponderado (EW-KDE) Invariante de Escala, Teoría de Escape de Kramers.

---

## 1. Reloj Intrínseco Dual: Dollar Bars ($\tau$) y Tasa de Flujo en Reloj ($t$)

Trabajamos sobre el log-precio $x_\tau = \ln(P_\tau)$ muestreado cada vez que el mercado intercambia un cuanto constante de valor monetario $\Delta DV = \sum_i P_i v_i = \theta_{\text{dollar}}$.

* **Ecuación de Langevin sobreamortiguada en tiempo de barra ($\tau$):**
  En el índice discreto de *Dollar Bars* ($\tau = 1, 2, \dots$), la varianza difusiva por barra $\sigma^2$ se estabiliza y el proceso obedece en el límite continuo de Smoluchowski:

$$dx_\tau = -V'(x_\tau)\,d\tau + \sigma\,dW_\tau$$

  donde $W_\tau$ es un proceso de Wiener estándar ($\langle dW_\tau\,dW_{\tau'} \rangle = \delta(\tau - \tau')\,d\tau$).
  
  > **Definición de $\sigma^2$:** El coeficiente de difusión térmico $\sigma^2$ **no** es la varianza de los precios, sino la **varianza de los retornos logarítmicos por barra**:
  > $$\sigma^2 = \sum_{i=1}^N w_i (\Delta x_i)^2$$

* **Conservación de la señal de volumen (Tiempo de Reloj $\Delta t_\tau$):**
  Como cada *Dollar Bar* contiene por definición el mismo volumen monetario $\theta_{\text{dollar}}$, una inyección agresiva de liquidez institucional no aumenta el volumen de la barra, sino que **comprime el tiempo de reloj** $\Delta t_\tau = t_\tau - t_{\tau-1}$ necesario para imprimirla.
  
  Protegemos divisiones por cero en backtests agregados forzando:
  $$\Delta t_\tau = \max(t_\tau - t_{\tau-1}, 1\,\text{ms})$$

* **Intensidad de flujo (tasa de llegada):**

$$\lambda_\tau = \frac{1000}{\Delta t_\tau\,(\text{ms})}, \qquad \tilde{\lambda}_\tau = \frac{\lambda_\tau}{\text{EMA}_M(\lambda_\tau)}$$

* Este escalar adimensional $\tilde{\lambda}_\tau$ actúa como la masa/temperatura cinética instantánea del flujo de órdenes sin violar la geometría de las *Dollar Bars*.

---

## 2. Reconstrucción Analítica del Paisaje Energético $V(x)$

Descartamos el ajuste global por polinomio cuártico sobre histograma (que diverge a $+\infty$ en los bordes de la ventana e impide modelar roturas exteriores) y lo sustituimos por un **Estimador de Densidad por Núcleo Gaussiano Exponencialmente Ponderado con Suelo de Vacío Invariante de Escala (Regularized EW-KDE)**.

```text
         V(x) [Energía Potencial]
           ▲
           │        Cima de Barrera x_b: V'(x_b) = 0, V''(x_b) < 0
           │        (Frontera Fuerza Repulsiva = STOP LOSS / BREAKOUT)
           │                 │
           │                 ▼          Punto de Inflexión x_inf: V''(x) = 0
     V_max ┼────── - - - - - ╭───────── (Fuerza Restauradora MÁXIMA = ENTRADA)
   (Vacío) │                ╱ ╲                       │
           │               ╱   ╲                      ▼
           │              ╱     • x_inf⁻            • x_inf⁺
           │             ╱       ╲                 ╱ ╲
           │            ╱         ╲               ╱   ╲
           │           ╱           ╲_____________╱     ╲
           │          ╱               Fondo x_0         ╲
           │         ╱              (TAKE PROFIT)        ╲
           └────────┴─────────────────────┴───────────────┴────► x = ln(P)
                  Pozo Vecino        Pozo Activo
```

### A. Densidad Regularizada Invariante de Escala y Potencial de Boltzmann-Gibbs

Sobre una ventana rodante de $N$ *Dollar Bars* con pesos de decaimiento exponencial $w_i \propto \exp(-\eta(N-i))$ ($\sum_{i=1}^N w_i = 1$), ancho de banda $h$ y un suelo de vacío adimensional $\epsilon_0 \in [10^{-4}, 10^{-2}]$:

$$\hat{P}_{\text{reg}}(x) = \frac{1}{\sqrt{2\pi}h} \left[ (1 - \epsilon_0) \sum_{i=1}^{N} w_i \exp\left(-\frac{(x - x_i)^2}{2h^2}\right) + \epsilon_0 \right]$$

A partir de la solución estacionaria de la ecuación de Fokker-Planck:

$$V(x) = -\frac{\sigma^2}{2} \ln \hat{P}_{\text{reg}}(x)$$

> **Invarianza de Escala:** Al factorizar $\frac{1}{\sqrt{2\pi}h}$, el término $-\frac{\sigma^2}{2}\ln(\frac{1}{\sqrt{2\pi}h})$ es una constante aditiva que **desaparece idénticamente al derivar** la fuerza $F(x) = -V'(x)$ y en las diferencias de potencial $\Delta V = V(x_b) - V(x)$. El potencial queda 100% desacoplado de la magnitud de $h$.

### B. Calibración Robusta de $h$ (Silverman con $N_{\text{eff}}$ e IQR/MAD)

Para evitar que las barras antiguas con peso residual distorsionen el ancho de banda, usamos el **tamaño muestral efectivo de Kish**:

$$N_{\text{eff}} = \frac{1}{\sum_{i=1}^N w_i^2}$$

Y para evitar sobredimensionar $h$ en mercados bimodales (donde $\sigma_x$ mide la distancia entre pozos), usamos la dispersión robusta:

$$\hat{\sigma}_{\text{rob}} = \min\left(\hat{\sigma}_{x,w},\, 1.4826 \cdot \text{MAD}_w(x)\right), \qquad h = \kappa_h \cdot 0.9 \, \hat{\sigma}_{\text{rob}} \, N_{\text{eff}}^{-1/5}$$

con $\kappa_h \in [0.5, 2.0]$ como hiperparámetro.

### C. Derivadas Analíticas Exactas (Cero Diferencias Finitas)

Definiendo $u_i(x) = \frac{x - x_i}{h}$ y el kernel base $K_0(x) = (1 - \epsilon_0) \sum_{i=1}^N w_i \exp\left(-\frac{1}{2}u_i(x)^2\right) + \epsilon_0$:

$$K'_0(x) = -\frac{1 - \epsilon_0}{h} \sum_{i=1}^N u_i(x)\,w_i\exp\left(-\frac{1}{2}u_i(x)^2\right)$$

$$K''_0(x) = \frac{1 - \epsilon_0}{h^2} \sum_{i=1}^N (u_i(x)^2 - 1)\,w_i\exp\left(-\frac{1}{2}u_i(x)^2\right)$$

1. **Fuerza Determinista del Mercado $F(x)$:**

$$F(x) = -\frac{dV}{dx} = \frac{\sigma^2}{2} \frac{K'_0(x)}{K_0(x)}$$

2. **Curvatura Local / Rigidez del Pozo $V''(x)$:**

$$V''(x) = -\frac{dF}{dx} = -\frac{\sigma^2}{2} \left[ \frac{K''_0(x)}{K_0(x)} - \left(\frac{K'_0(x)}{K_0(x)}\right)^2 \right]$$

---

## 3. Algoritmo Numérico de Búsqueda de Raíces: Ray-Marching + Bisección

Para evitar la divergencia o división por cero de Newton-Raphson en mercados bimodales:

1. **Localización de $x_0$ (Fondo de la Cuenca Activa):**
   - Desde el precio actual $x_\tau$, avanzamos en la dirección del gradiente restaurador ($\text{sign}(F(x_\tau))$) a pasos $\delta x = 0.25\,h$.
   - En el primer paso donde $F(x)$ cambie de signo, se acota el intervalo $[a, b]$ con $V'' > 0$.
   - Se refina la raíz $F(x_0) = 0$ mediante bisección híbrida (5-6 iteraciones).

2. **Localización de $x_{\text{inf}}$ y $x_b$ (Ray-Marching bilateral desde $x_0$):**
   - Desde $x_0$, avanzamos hacia la izquierda ($x_0 - k\delta x$) y hacia la derecha ($x_0 + k\delta x$) con paso $\delta x = 0.20\,h$:
   - **Puntos de Inflexión ($x_{\text{inf}}^\pm$):** El primer intervalo donde $V''(x)$ cruza de positivo a negativo acota $x_{\text{inf}}$. Refinado por bisección de $V''(x) = 0$. Registramos $F_{\max}^- = F(x_{\text{inf}}^-) > 0$ y $F_{\max}^+ = |F(x_{\text{inf}}^+)| > 0$.
   - **Cimas de Barrera ($x_b^\pm$):** Continuando más allá de $x_{\text{inf}}$ (donde $V''(x) < 0$), el primer punto donde $F(x)$ cruza cero (pozo vecino) o donde $|F(x)| \le \alpha F_{\max}$ (disociación hacia el vacío) define $x_b$.

---

## 4. Reglas de Entrada y Confinamiento Energético

### A. Entrada por Reversión Elástica (Long y Short)

* **Energía Cinética Instantánea del Flujo:**

$$E_k(\tau) = \frac{1}{2} \tilde{\lambda}_\tau (\Delta x_\tau)^2$$

* **Barrera Energética Restante:** $\Delta V_{\text{rem}}(x) = V(x_b) - V(x)$.

* **Condición de Entrada LONG ($x < x_0$):**
  1. Compresión elástica: $F(x) \ge \beta F_{\max}^-$ (con $\beta \in [0.70, 0.90]$) y $x > x_b^-$.
  2. Confinamiento de energía: $E_k(\tau) < \Delta V_{\text{rem}}(x)$.
  3. Agotamiento bajista: $\Delta x_\tau \ge 0$.

* **Condición de Entrada SHORT ($x > x_0$):**
  1. Compresión elástica: $F(x) \le -\beta F_{\max}^+$ y $x < x_b^+$.
  2. Confinamiento de energía: $E_k(\tau) < \Delta V_{\text{rem}}(x)$.
  3. Agotamiento alcista: $\Delta x_\tau \le 0$.

### B. Entrada por Escape de Kramers (Breakout Explosivo)

* **Long Breakout:** $x_\tau > x_b^+$ con $E_k(\tau) > \Delta V^+$, aceleración institucional $\tilde{\lambda}_\tau \ge \lambda_{\text{thresh}}$ y $\Delta x_\tau > 0$.
  - Stop-Loss: $x_{\text{SL}} = x_b^+$.
  - Take-Profit: $x_{\text{TP}} = x_\tau + \mu_{\text{breakout}} |x_b^+ - x_0|$.
* **Short Breakout:** $x_\tau < x_b^-$ con $E_k(\tau) > \Delta V^-$, $\tilde{\lambda}_\tau \ge \lambda_{\text{thresh}}$ y $\Delta x_\tau < 0$.
  - Stop-Loss: $x_{\text{SL}} = x_b^-$.
  - Take-Profit: $x_{\text{TP}} = x_\tau - \mu_{\text{breakout}} |x_0 - x_b^-|$.

### C. Filtro de Profundidad Mínima del Pozo (Prevención de la Trampa de Micro-Pozos)

* **Diagnóstico Cuantitativo:**
  En contratos de futuros perpetuos (ej. Binance USDT-M), las comisiones de mercado (*taker fee* 0.05% apertura + 0.05% cierre + 0.05% slippage estimado) imponen un coste friccional de ida y vuelta de $\sim 0.15\%$. En Bitcoin a $\$80,000$ USD, esto representa una fricción ineludible de $\sim \$120$ USD por trade.
* **Condición de Profundidad Mínima:**
  Para entradas de reversión elástica, se exige que la distancia relativa hacia el fondo de equilibrio $x_0$ supere el umbral de comisiones:

$$\Delta x_{\text{depth}} = \frac{|P_{\text{close}} - \exp(x_0)|}{P_{\text{close}}} \ge \text{min\_well\_depth\_pct}$$

### D. Filtro de Deriva Macro (Macro Trend Drift Filter)

* **Fundamento Físico de Gravedad Macro:**
  La ecuación de Langevin asume difusión sobre un potencial cuasi-estacionario local. En caídas o subidas macro prolongadas, se rastrea una $\text{EMA}_{\text{macro}}$ (periodo 200) y su pendiente sobre 50 barras ($\text{slope}_{\text{macro}}$):
  - **Entradas LONG:** Permitidas únicamente si $\text{slope}_{\text{macro}} \ge 0$.
  - **Entradas SHORT:** Permitidas únicamente si $\text{slope}_{\text{macro}} \le 0$.

---

## 5. Mecanismos de Salida Físicos y Contrato Universal Triple-Barrier

1. **Take-Profit Físico con Sobreoscilación Armónica (Harmonic Overshoot):**
   Para multiplicar el recorrido monetario y evitar que el beneficio sea devorado por las comisiones de futuros, el objetivo de Take-Profit se extiende más allá de $x_0$ hacia el punto de inflexión de la pared opuesta mediante la fracción armónica $\gamma \in [0.0, 1.0]$ (`opposite_wall_fraction`, por defecto $0.60$):
   - **Reversión LONG ($x < x_0$):** $x_{\text{TP}}^{\text{long}} = x_0 + \gamma\,(x_{\text{inf}}^+ - x_0) - \zeta\,\delta_{\text{th}}$
   - **Reversión SHORT ($x > x_0$):** $x_{\text{TP}}^{\text{short}} = x_0 - \gamma\,(x_0 - x_{\text{inf}}^-) + \zeta\,\delta_{\text{th}}$

2. **Stop-Loss Físico (Cima de Barrera $x_b$):**
   - En $x_b$, la fuerza restauradora se anula o invierte su signo hacia un pozo vecino o el vacío. Se fija exactamente en $x_b^\pm$.

3. **Protección Horizontal y Deformación del Paisaje:**
   - La barrera horizontal `time_barrier_bars` previene quedar atrapado si el pozo experimenta relajación térmica sin alcanzar TP ni SL (calibración empírica: 300 barras).

4. **Telemetría y Auditoría en Tiempo Real:**
   - Implementa `Strategy::audit()`, registrando en cada barra la topología diagnosticada para monitoreo determinista.

5. **Breakeven Stop Dinámico y Trailing Ratchet:**
   - En cuanto la excursión favorable alcanza el fondo de equilibrio $x_0$, el Stop-Loss se desplaza inmediatamente a precio de entrada (`breakeven_atr: Some(dist_to_x0)`), protegiendo las operaciones ganadoras contra giros bruscos.

---

## 6. Estructuras de Datos en Rust

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalSample {
    pub x: f64,
    pub delta_x: f64,
    pub delta_t_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WellTopology {
    pub x_0: f64,
    pub curvature_0: f64,
    pub delta_thermal: f64,
    pub x_inf_left: f64,
    pub f_max_left: f64,
    pub x_inf_right: f64,
    pub f_max_right: f64,
    pub x_b_left: f64,
    pub x_b_right: f64,
    pub barrier_height_left: f64,
    pub barrier_height_right: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangevinConfig {
    pub window_size: usize,
    pub bandwidth_factor: Decimal,
    pub decay_eta: Decimal,
    pub vacuum_epsilon: Decimal,
    pub beta_entry: Decimal,
    pub alpha_dissociation: Decimal,
    pub zeta_thermal_tp: Decimal,
    pub lambda_ema_period: usize,
    pub time_barrier_bars: usize,
    pub min_take_profit_pct: Decimal,
    pub min_well_depth_pct: Decimal,
    pub enable_kramers_breakout: bool,
    pub kramers_lambda_threshold: Decimal,
    pub kramers_tp_multiple: Decimal,
    pub opposite_wall_fraction: Decimal,
    pub enable_dynamic_breakeven: bool,
    pub macro_trend_filter: bool,
    pub macro_ema_period: usize,
    pub macro_slope_window: usize,
}
```
