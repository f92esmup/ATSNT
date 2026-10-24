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

* **Long Breakout:** $x_\tau > x_b^+$ con $E_k(\tau) > \Delta V^+$ y $\tilde{\lambda}_\tau > 1.5$.
* **Short Breakout:** $x_\tau < x_b^-$ con $E_k(\tau) > \Delta V^-$ y $\tilde{\lambda}_\tau > 1.5$.

---

## 5. Mecanismos de Salida Físicos (Cero ATR)

1. **Take-Profit Físico con Cancelación Exacta:**
   Como $V''(x_0) = -\frac{\sigma^2}{2} \frac{K''_0(x_0)}{K_0(x_0)}$, el factor $\sigma^2/2$ se cancela exactamente en el radio térmico:

$$\delta_{\text{th}} = \sqrt{-\frac{K_0(x_0)}{K''_0(x_0)}}$$

$$x_{\text{TP}}^{\text{long}} = x_0 - \zeta\,\delta_{\text{th}}, \qquad x_{\text{TP}}^{\text{short}} = x_0 + \zeta\,\delta_{\text{th}} \qquad (\zeta \in [0, 0.5])$$

2. **Stop-Loss Físico (Cima de Barrera $x_b$):**
   - En $x_b$, la fuerza restauradora se anula o invierte su signo hacia un pozo vecino o el vacío. Se fija en $x_b^\pm$.

3. **Salida por Deformación del Paisaje:**
   - Si $\Delta V(\tau) < \frac{1}{2}\sigma^2$ (desaparición de barrera térmica) o $x_0(\tau)$ migra cruzando el precio de entrada, se aborta a mercado.

4. **Trailing Stop de Escape:**
   - En breakouts, sigue la cima de la barrera atravesada $x_b(\tau)$.

---

## 6. Estructuras de Datos en Rust

```rust
#[derive(Debug, Clone, Copy)]
pub struct PhysicalSample {
    pub x: f64,
    pub delta_x: f64,
    pub delta_t_ms: u64,
}

#[derive(Debug, Clone)]
pub struct KdePotential {
    window: VecDeque<PhysicalSample>,
    capacity: usize,
    bandwidth_factor: f64,
    bandwidth_h: f64,
    decay_eta: f64,
    vacuum_epsilon: f64,
    weights: Vec<f64>,
    n_eff: f64,
    diffusivity_sigma2: f64,
    lambda_ema: f64,
}

#[derive(Debug, Clone, Copy)]
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
```
