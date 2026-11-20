<!-- SPDX-License-Identifier: AGPL-3.0-only -->
[English](README.md) | **Español**

# `bench/` — el arnés de benchmarks de abyss

Especificación rectora: **COMP-14** (VOL1 §2 presupuestos, §3 patrones prohibidos, §4.1
benchmarks, §5 cumplimiento en CI). No es TCB, pero es lo que valida el código TCB: el
criterio de salida del hito 16 es `check()` a **≤50 µs p99, medido aquí**.

```
cargo run --release -p ec-abyss-bench
```

Termina con código distinto de cero si algún benchmark excede su presupuesto o asigna memoria en una ruta donde
la invariante raíz 11 lo prohíbe. Ese es todo el contrato con CI — COMP-14 §5 no necesita
ningún envoltorio a su alrededor.

Siempre `--release`. Una ejecución en debug mide la compilación de debug, y los números no
son comparables con nada.

## Estructura

| Archivo | Qué |
|---|---|
| `src/lib.rs` | `measure()` — cronometra una clausura, conserva cada muestra y las ordena. `Samples` — percentiles. `Report` — la tabla y el veredicto de salida. |
| `src/budgets.rs` | COMP-14 §2.1 y §2.1b como datos. Un solo lugar para cambiar cuando cambie la especificación. |
| `src/alloc.rs` | El asignador contador que pide COMP-14 §3. `Report::forbid_alloc(name)` hace que un benchmark falle en su primera asignación. |
| `src/main.rs` | El ejecutor. Instala el asignador, registra los benchmarks y devuelve el código de salida. |

## Por qué no criterion

COMP-14 §4.1 decía antes "microbenchmarks con Criterion". Criterion reporta una media
con un intervalo de confianza y trata la cola lenta como ruido que hay que clasificar y descartar;
cada presupuesto de COMP-14 §2 es un p99. Consulta
`docs/decisions/0043-hand-rolled-bench-harness.md` — la línea de la especificación se enmendó
en el mismo cambio.

## Qué se ejecuta hoy

Solo `timer-overhead`, el arnés midiendo su propio piso. Los cinco sujetos
que nombra §4.1 todavía no existen: `check()` y la coincidencia de scope llegan con el hito
16, la codificación de auditoría con el 12, la serialización de árboles con el 22 y la fusión de daños con el
trabajo de render. Cada uno llega con su hito, como una llamada a `measure()` más un presupuesto.

`timer-overhead` no es un marcador de posición. El presupuesto más estricto de la tabla es
20 µs (§2.1b, el matcher irreversible); si el viaje de ida y vuelta de `Instant` estuviera
cerca de eso, cada número que imprime este crate se estaría midiendo a sí mismo.
Actualmente corre a ~18 ns en p99, tres órdenes de magnitud por debajo.

## Pendiente

COMP-14 §5 quiere líneas base por commit y una falla de compilación ante una regresión de >10%
respecto de la mediana móvil de las últimas 10 compilaciones exitosas. No está construido — no hay
nada que tomar como línea base hasta que lleguen los sujetos reales. Se sigue en ADR 0043.
