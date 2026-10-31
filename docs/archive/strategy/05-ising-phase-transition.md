# Strategy Specification: Curie-Weiss Ising Spin Model & Critical Phase Transitions

> **Estado**: En espera / Planificada (Planned)  
> **Fundamento Físico**: Mecánica Estadística de Sistemas de Espines, Teoría de Campo Medio de Curie-Weiss, Susceptibilidad Magnética, Entropía de Información de Shannon.

---

## 1. Visión y Objetivos

Modelar la microestructura de transacciones y el flujo de órdenes agresivo (*taker trades*) como una red de espines magnéticos interactuantes $s_i \in \{+1, -1\}$.

El objetivo es detectar la proximidad a **transiciones de fase de segundo orden** (roturas espontáneas de simetría) donde la susceptibilidad magnética del mercado diverge, desencadenando cascadas de liquidación y movimientos explosivos unidireccionales en futuros perpetuos.

---

## 2. Formalismo Físico

### 2.1 Mapeo de Trades a Espines
Para cada transacción procesada por [`Trade`](file:///home/f92esmup/Projects/ATSNT/crates/domain/src/trade.rs):

$$s_i = \begin{cases} +1 & \text{si agresor es comprador (taker buy, is\_buyer\_maker = false)} \\ -1 & \text{si agresor es vendedor (taker sell, is\_buyer\_maker = true)} \end{cases}$$

### 2.2 Hamiltoniano de Campo Medio y Magnetización
La energía del sistema de $N$ espines recientes se define mediante el Hamiltoniano de Curie-Weiss:

$$H(\{s\}) = -\frac{J}{2N}\sum_{i,j=1}^N s_i s_j - h \sum_{i=1}^N s_i$$

Donde:
- $J$: Parámetro de interacción / acoplamiento (efecto rebaño y arbitraje).
- $h$: Campo externo (desbalance del libro de órdenes L2 y funding rate).
- Magnetización media: $m = \frac{1}{N}\sum_{i=1}^N s_i \in [-1, +1]$.

### 2.3 Transición Crítica y Susceptibilidad
La magnetización en equilibrio cumple:
$$m = \tanh\left(\frac{Jm + h}{k_B T}\right)$$

Con una temperatura crítica $T_c = J / k_B$:
- **$T > T_c$ (Fase Paramagnética)**: Estado desordenado, $m \approx 0$. Mercado en equilibrio/ruido browniano.
- **$T \to T_c^+$ (Divergencia de Susceptibilidad)**: La respuesta a perturbaciones externas $\chi = \frac{\partial m}{\partial h} \to \infty$. El mercado es extremadamente frágil.
- **$T < T_c$ (Fase Ferromagnética)**: Rotura espontánea de simetría. Los espines se alinean ($|m| \to 1$), produciendo una cascada violenta.

---

## 3. Hoja de Ruta de Implementación

- [ ] Extractor de magnetización y susceptibilidad sobre `MarketEvent::Trade`.
- [ ] Monitorización de entropía de Shannon $S = -\sum p_k \ln p_k$ sobre bloques de micro-estados.
- [ ] Detección de colapso entrópico previo a la ruptura de simetría.
- [ ] Integración en el registro de estrategias de ATSNT.
