# Strategy Specification: Non-Equilibrium Langevin Dynamics & Empirical Potential Wells

> **Estado**: En desarrollo (In Development)  
> **Fundamento Físico**: Mecánica Estadística Fuera del Equilibrio, Ecuación de Langevin, Ecuación de Fokker-Planck, Teoría de Escape de Kramers.

---

## 1. Visión y Objetivos

Esta estrategia abandona los indicadores técnicos empíricos tradicionales (medias móviles, osciladores de ventana fija, ATR) y modela la microestructura del mercado a través de los primeros principios de la **física estadística y la dinámica browniana**.

El precio $x(t)$ sobre **Dollar Bars** se modela como una partícula browniana sujeta a un campo de fuerzas derivado de un **pozo de potencial efectivo $V(x)$**, amortiguada por fricción y bombardeada por fluctuaciones estocásticas gaussianas:

$$\frac{dx}{dt} = -\frac{\partial V(x)}{\partial x} + \sigma \eta(t)$$

Donde:
- $V(x)$: Perfil de energía potencial determinado por la acumulación histórica de liquidez y volumen.
- $F(x) = -\frac{\partial V}{\partial x}$: Fuerza determinista restauradora o repulsiva ejercida por el mercado.
- $\sigma \eta(t)$: Ruido blanco gaussiano ($\langle \eta(t)\eta(t') \rangle = \delta(t-t')$).

---

## 2. Dominio Temporal: ¿Por qué Dollar Bars y no Ticks Crudos?

Para que la **Ecuación de Langevin** clásica sea analíticamente válida, el término estocástico $\eta(t)$ **debe ser ruido blanco gaussiano sin memoria**.

1. **Ticks Crudos / Tiempo de Reloj**:
   - En el tiempo calendario o por trade discreto, los datos exhiben heterocedasticidad condicional extrema (volatilidad agrupada) y colas pesadas de Lévy/Cauchy.
   - El ruido no es gaussiano ($\eta(t)$ violaría las hipótesis de Langevin).
2. **Dollar Bars (Muestreo por Flujo de Información)**:
   - Al muestrear barras cada vez que se intercambia un volumen constante de dólares ($\theta_{\text{dollar}}$), el proceso se relojea a su **tiempo intrínseco u operacional** (Marcos López de Prado, AFML).
   - Según el Teorema Central del Límite aplicado al flujo transaccional, los retornos sobre Dollar Bars **recuperan una distribución aproximadamente normal / gaussiana**, eliminando la heterocedasticidad.
   - **Conclusión**: El modelado de Langevin y la solución de Boltzmann-Fokker-Planck son formalmente aplicables gracias al sustrato de `DollarBars`.

---

## 3. Reconstrucción del Pozo de Potencial Efectivo

Por la **Ecuación de Fokker-Planck**, la función de densidad de probabilidad estacionaria $P_{st}(x)$ de una partícula en el pozo satisface la distribución de Gibbs-Boltzmann:

$$P_{st}(x) = \frac{1}{Z} \exp\left(-\frac{2 V(x)}{\sigma^2}\right)$$

Invertimos analíticamente la distribución empírica de precios sobre una ventana rodante de $N$ Dollar Bars para reconstruir el potencial:

$$V(x) = -\frac{\sigma^2}{2} \ln(P_{\text{empírico}}(x))$$

Ajustamos $V(x)$ mediante una regresión polinomial cuártica:
$$V(x) = c_4 x^4 + c_3 x^3 + c_2 x^2 + c_1 x + c_0$$

La topología del polinomio clasifica el régimen del mercado:
- **Pozo Único (Armónico)**: Un único mínimo global $x_0$ ($V''(x_0) > 0$). Régimen de rango simétrico.
- **Doble Pozo Cuártico**: Dos mínimos locales $x_1$ (soporte) y $x_2$ (resistencia) separados por un máximo local inestable $x_b$ (barrera de potencial $\Delta V = V(x_b) - V(x_1)$).

---

## 4. Mecánica Operativa y Reglas de Entrada (Long / Short)

### 4.1 Régimen de Oscilación / Reversión (Partícula Atrapada: $E_k < \Delta V$)
Cuando la energía cinética instantánea $E_k = \frac{1}{2} v^2$ es inferior a la barrera $\Delta V$, el precio no tiene energía suficiente para escapar del pozo.

- **Entrada LONG**:
  - Condición: El precio se encuentra cerca del soporte del pozo ($x \approx x_1$), la fuerza restauradora apunta al alza ($F(x) = -V'(x) > F_{\text{threshold}} > 0$) y la curvatura es restauradora ($V''(x) > 0$).
- **Entrada SHORT**:
  - Condición: El precio se encuentra cerca de la resistencia del pozo ($x \approx x_2$), la fuerza restauradora apunta a la baja ($F(x) = -V'(x) < -F_{\text{threshold}} < 0$) y la curvatura es restauradora ($V''(x) > 0$).

### 4.2 Régimen de Escape de Kramers (Breakout Direccional: $E_k > \Delta V$)
Cuando una inyección de volumen transaccional confiere a la partícula energía cinética superior a la altura de la barrera de potencial, ocurre una **transición de Kramers**:

$$r_K \approx \frac{\sqrt{V''(x_{\text{min}}) |V''(x_{\text{barrera}})|}}{2\pi \gamma} \exp\left(-\frac{2\Delta V}{\sigma^2}\right)$$

- **Entrada LONG BREAKOUT**: Precio cruza $x_b$ al alza con velocidad y energía cinética $E_k > \Delta V$.
- **Entrada SHORT BREAKOUT**: Precio rompe el mínimo del pozo a la baja con $E_k > \Delta V_{\text{soporte}}$.

---

## 5. Salidas Físicas: Take-Profit, Stop-Loss, Breakeven y Trailing Stop

> **Cero Indicadores Arbitrarios**: No se utiliza el ATR ni múltiplos fijos en pips. Todas las salidas se derivan del paisaje energético $V(x)$.

### 5.1 Take-Profit Físico (Puntos de Mínima Energía / Atractor)
- En operaciones de reversión:
  - Long TP: El fondo del pozo o nivel de equilibrio $x_0$ donde la fuerza neta se anula:
    $$F(x_0) = -\left.\frac{dV}{dx}\right|_{x_0} = 0 \quad \text{con} \quad V''(x_0) > 0$$
    Es el punto de máxima probabilidad estacionaria $\max P_{st}(x)$.

### 5.2 Stop-Loss Físico (Puntos de Inflexión de Curvatura)
En lugar de multiplicar el ATR por una constante heurística, el Stop Loss se sitúa en los **puntos de inflexión de la energía potencial**:

$$V''(x) = \frac{d^2 V}{dx^2} = 0$$

- **Justificación Física**: Mientras $V''(x) > 0$, la fuerza es restauradora (tiende a devolver el precio al fondo del pozo). En cuanto el precio rebasa el punto de inflexión ($V''(x) < 0$), la fuerza se vuelve repulsiva (acelera alejándose del pozo). 
- Mantener una posición cuando la fuerza se vuelve repulsiva viola las leyes del sistema: el Stop Loss se ejecuta de inmediato en $x_{\text{inflexión}}$.

### 5.3 Breakeven Físico (Disipación por Fricción)
- Cuando el precio alcanza el punto medio entre la entrada y el centro del pozo $x_0$, y la aceleración se reduce debido a la disipación $\gamma$, el Stop Loss se desplaza al precio de entrada ($P_{\text{entry}}$) libre de riesgo.

### 5.4 Trailing Stop Físico (Superficie Equipotencial Móvil)
- En operaciones de escape (breakout), el potencial $V(x, t)$ se recalcula con cada nueva Dollar Bar.
- El trailing stop persigue la **superficie equipotencial** que garantiza que el precio no vuelva a caer dentro de la barrera de atracción superada:
  $$\text{Trailing SL}(t) = x_{\text{barrera}}(t)$$

---

## 6. Arquitectura e Implementación en ATSNT

- **Módulo**: `crates/strategies/src/langevin_potential.rs`
- **Traits**: Implementa `Strategy` y `OptimizableStrategy` para calibración WFO (Walk-Forward).
- **Aritmética**: 100% `rust_decimal::Decimal` para precios y gestión de órdenes; ajuste polinomial determinista sin aproximaciones de punto flotante en la ejecución.
