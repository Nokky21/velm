# Panel de repositorios (`term-repos`)

`term-repos` muestra en una sola vista el estado de todos tus repositorios Git y worktrees en modo **estrictamente de solo lectura**.

## Comportamiento y seguridad

- Invoca `git` del sistema (sin `git2` ni `gix`) con `GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0` y `LC_ALL=C`: no compite con el IDE por `index.lock` ni se queda colgado pidiendo credenciales.
- Carga los repositorios descubiertos de forma progresiva en paralelo con límite de tiempo por repositorio.
- Cero tráfico de red por defecto: solo consulta al remoto si pulsas `r` en la interfaz TUI (`git ls-remote --symref origin HEAD`).

---

## Configuración (`[term.repos]`)

En `~/.config/velm/config.toml`:

| Clave | Tipo | Defecto | Validación |
|---|---|---|---|
| `enabled` | bool | `false` | — |
| `roots` | lista de string | `["~/Desktop/WORK"]` | Carpetas existentes; `~` se expande |
| `max_depth` | int | `2` | `1` a `5` |
| `include_worktrees` | bool | `false` | — |
| `skip` | lista de string | `["node_modules", "target", ".worktrees"]` | Nombres de carpeta (no rutas) |
| `parallel` | int | `8` | `1` a `32` |
| `timeout_ms` | int | `5000` | `500` a `60000` |
| `remote_timeout_ms` | int | `10000` | `1000` a `60000` |

Ejemplo:

```toml
[term.repos]
enabled = true
roots = ["~/Desktop/WORK"]
max_depth = 3
include_worktrees = true
```

---

## Uso CLI y salida JSON

```bash
term-repos [--config <ruta>] [--json] [--root <carpeta>]...
```

- Sin `--json`: abre la interfaz TUI interactiva.
- Con `--json`: emite por `stdout` un array ordenado por `path` y termina. Si se pasan uno o más `--root`, sustituyen a `roots` de la configuración.

Campos por repositorio en `--json`: `path`, `name`, `branch`, `detached`, `oid`, `upstream`, `upstreamGone`, `ahead`, `behind`, `modified`, `untracked`, `conflicted`, `stashes`, `lastCommitAt`, `worktree`, `empty`, `timedOut`, `error`.

---

## Atajos en la interfaz TUI

| Tecla | Acción |
|---|---|
| `j` / `↓`, `k` / `↑` | Mover selección |
| `PgUp` / `PgDn`, `g` / `Home`, `G` / `End` | Saltar páginas o extremos |
| `d` | Alternar filtro «solo con cambios pendientes» |
| `/` | Buscar por nombre o ruta (`Enter` confirma, `Esc` limpia) |
| `r` | Consultar rama por defecto real en `origin` (muestra indicador `[red]`) |
| `o` | Abrir el repositorio seleccionado en un panel o pestaña nueva de `velm` |
| `R` / `F5` | Re-escanear repositorios |
| `q` / `Esc` / `Ctrl+C` | Salir |

En el panel inferior de detalle se muestran la ruta completa, `upstream`, `origin/HEAD` cacheado y `git diff --stat` (solo lectura).

---

## Códigos de salida

| Código | Significado |
|---|---|
| `0` | Éxito (aunque algún repositorio individual tenga `error` o `timedOut`) |
| `1` | Error al iniciar la TUI o al serializar JSON |
| `2` | Configuración, `--root` inexistente o argumentos incorrectos |
