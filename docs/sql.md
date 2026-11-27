# Panel Postgres seguro (`term-sql`)

`term-sql` es un cliente TUI de Postgres diseñado para consultar y operar en entornos `local` y `dev` reduciendo errores accidentales.

## Modelo de seguridad

- **Prohibido en producción:** la configuración exige `env = "local"` o `env = "dev"`. Usar `env = "prod"` (o cualquier otro valor) provoca un error inmediato de configuración.
- **Sin secretos en disco ni logs:** las cadenas de conexión nunca se guardan en `config.toml`; se almacenan en el **Llavero de macOS** bajo el servicio `term-sql` y nunca se imprimen en pantalla ni en mensajes de error.
- **Lectura por defecto:** toda sesión arranca en modo **LECTURA** con `default_transaction_read_only=on`, `statement_timeout` y corte de resultados en streaming a `row_limit` filas.
- **Escritura controlada (`F2`):**
  - Solo acepta **una sentencia** por ejecución, analizada recursivamente con `sqlparser` (dialecto Postgres, incluyendo CTEs `WITH …` y `EXPLAIN ANALYZE`). Si no se entiende, se rechaza («ejecútala en psql»).
  - Rechaza antes de enviar sentencias incompatibles con transacciones (`CREATE INDEX CONCURRENTLY`, `DROP INDEX CONCURRENTLY`, `REINDEX … CONCURRENTLY`, `VACUUM`, `CREATE DATABASE`, `DROP DATABASE`, `ALTER SYSTEM`).
  - `UPDATE` o `DELETE` sin `WHERE` (incluso dentro de CTEs o `EXPLAIN ANALYZE`) exige escribir exactamente `SIN WHERE`.
  - `DROP` o `TRUNCATE` exige escribir el nombre exacto de la conexión (tanto en `local` como en `dev`).
  - Cada sentencia corre dentro de `BEGIN` con `SET LOCAL statement_timeout` y `SET LOCAL idle_in_transaction_session_timeout` (60 s por defecto). La barra roja **ESCRITURA** muestra la cuenta atrás esperando `c` (`COMMIT`) o `r` (`ROLLBACK`).
  - **Confirmación reforzada en `dev`:** al pulsar `c` en una conexión `dev`, pide escribir el nombre de la conexión antes de confirmar el `COMMIT`.

> Nota: esta protección es contra accidentes, no contra un `SET` manual ejecutado en la sesión. No se guardan consultas ni resultados en disco.

---

## Configuración (`[term.sql]`)

En `~/.config/velm/config.toml`:

| Clave | Tipo | Defecto | Validación |
|---|---|---|---|
| `enabled` | bool | `false` | — |
| `row_limit` | int | `500` | `1` a `100000` |
| `statement_timeout_s` | int | `30` | `1` a `600` |
| `idle_tx_timeout_s` | int | `60` | `10` a `600` |

Cada entrada `[[term.sql.connections]]`:

| Clave | Tipo | Defecto | Validación |
|---|---|---|---|
| `name` | string | obligatorio | `[a-z0-9-]{1,40}`, único |
| `env` | string | obligatorio | `local` \| `dev` (`prod` es error) |
| `color` | string | `green` (`local`) / `yellow` (`dev`) | `green` \| `yellow` \| `blue` \| `magenta` |

Ejemplo:

```toml
[term.sql]
enabled = true
row_limit = 500
statement_timeout_s = 30
idle_tx_timeout_s = 60

[[term.sql.connections]]
name = "local-app"
env = "local"
color = "green"

[[term.sql.connections]]
name = "dev-tunnel"
env = "dev"
color = "yellow"
```

---

## Gestión de conexiones (Llavero)

```bash
term-sql [--config <ruta>] conn add <nombre>   # pide la URL sin mostrarla, prueba conexión y guarda en Llavero
term-sql [--config <ruta>] conn rm <nombre>    # elimina la entrada del Llavero
term-sql [--config <ruta>] conn list           # lista nombre y entorno (NUNCA la URL)
```

`conn add` exige que `<nombre>` esté declarado en `[[term.sql.connections]]` y que la cadena empiece por `postgres://` o `postgresql://`.

---

## Uso y atajos en la interfaz TUI

```bash
term-sql [--config <ruta>]
```

- **Selector de conexiones:** `↑`/`↓` (`k`/`j`) elegir conexión, `Enter` conectar en modo lectura, `q`/`Esc`/`Ctrl+C` salir.
- **Espacio de trabajo:**
  - `Tab`: alternar foco entre editor y resultados
  - `F5` / `Ctrl+E`: ejecutar sentencia
  - `F2`: alternar entre modo **LECTURA** y **ESCRITURA**
  - `Ctrl+P` / `Ctrl+N`: navegar historial de la sesión (solo en memoria)
  - `PgUp` / `PgDn` (o `[` / `]` en resultados): paginar resultados (50 filas/página)
  - `c` / `r`: confirmar (`COMMIT`) o deshacer (`ROLLBACK`) transacción pendiente
  - `Esc`: volver al selector de conexiones
  - `Ctrl+Q` (o `q` con foco en resultados): salir

---

## Códigos de salida

| Código | Significado |
|---|---|
| `0` | Éxito |
| `1` | Error de conexión, Llavero o ejecución |
| `2` | Configuración (p. ej. `env = "prod"`) o uso incorrecto |
