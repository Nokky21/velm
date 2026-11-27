# Panel de sesiones de Claude Code (`term-claude`)

`term-claude` lee en modo **solo lectura** las transcripciones locales de Claude Code (`~/.claude/projects/*/*.jsonl` y `subagents/*.jsonl`) para supervisar el contexto actual, tokens consumidos y coste estimado de cada sesión.

## Cómo calcula tokens y coste

- **Lectura incremental segura:** recuerda el desplazamiento en bytes por fichero; ignora líneas incompletas sin salto de línea al final y relee desde cero si el fichero se trunca.
- **Sin duplicados:** deduplica estrictamente por `message.id`, evitando contar varias veces respuestas repetidas en el `.jsonl`.
- **Contexto actual:** suma `input_tokens + cache_read_input_tokens + cache_creation_input_tokens` del último mensaje `assistant` que no sea `sidechain` (`isSidechain = false`).
- **Sidechain aparte:** los tokens de subagentes (`isSidechain = true` o `subagents/*.jsonl`) se muestran separados en `sidechainTokens`.
- **Coste (`costSource`):**
  1. `cost-state`: usa `totalCostUSD` si la transcripción incluye líneas `type = "cost-state"`.
  2. `estimated`: si no hay `cost-state`, calcula con los precios configurados en `[term.claude.prices."<modelo>"]`.
  3. `none` / `costIncomplete = true`: si `hasUnknownModelCost` es verdadero o falta el precio de algún modelo usado.

---

## Configuración (`[term.claude]`)

En `~/.config/velm/config.toml`:

| Clave | Tipo | Defecto | Validación |
|---|---|---|---|
| `enabled` | bool | `false` | — |
| `projects_dir` | string | `"~/.claude/projects"` | Carpeta existente (`~` se expande) |
| `days` | int | `7` | `1` a `365` |
| `context_alert_tokens` | int | `250000` | `>= 1000` |

Tabla opcional de precios por modelo (`[term.claude.prices."<id>"]`, en USD por millón de tokens, sin valores por defecto):

- `input` (float)
- `output` (float)
- `cache_write_5m` (float)
- `cache_write_1h` (float)
- `cache_read` (float)

Ejemplo:

```toml
[term.claude]
enabled = true
projects_dir = "~/.claude/projects"
days = 7
context_alert_tokens = 250000

[term.claude.prices."claude-sonnet-4-5"]
input = 3.0
output = 15.0
cache_write_5m = 3.75
cache_write_1h = 6.0
cache_read = 0.30
```

---

## Uso CLI y salida JSON

```bash
term-claude [--config <ruta>] [--json] [--days <n>]
```

- `--days <n>`: sobrescribe `days` (`1..=365`).
- `--json`: emite un array JSON ordenado por `lastActivityAt` descendente con `sessionId`, `project`, `cwd`, `gitBranch`, `lastActivityAt`, `models`, `contextTokens`, `contextAlert`, `tokens`, `sidechainTokens`, `costUsd`, `costSource`, `costIncomplete`.

---

## Atajos en la interfaz TUI

La TUI refresca automáticamente cada 2 segundos y resalta en rojo (`⚠`) las sesiones que superan `context_alert_tokens`.

| Tecla | Acción |
|---|---|
| `j` / `↓`, `k` / `↑` | Mover selección |
| `g` / `Home`, `G` / `End` | Ir al inicio o al final |
| `c` | Activar/desactivar filtro rápido por proyecto/CWD actual |
| `/` | Buscar por proyecto, sesión, CWD, rama o modelo (`Enter`/`Esc` confirma) |
| `r` | Refrescar manualmente |
| `q` / `Esc` / `Ctrl+C` | Salir |

---

## Códigos de salida

| Código | Significado |
|---|---|
| `0` | Éxito |
| `1` | Error al leer transcripciones, iniciar TUI o serializar JSON |
| `2` | Configuración o argumentos incorrectos |
