# velm

Multiplexor de terminal en Rust para uso diario en macOS (Apple Silicon) dentro de [Alacritty](https://alacritty.org/), acompañado de herramientas locales desacopladas (`term-*`) enfocadas en privacidad verificable y bajo consumo de memoria.

`velm` es un **fork de [`herdrdev/herdr`](https://github.com/herdrdev/herdr) (`v0.9.5`)** bajo licencia [Apache-2.0](LICENSE). Consulta [NOTICE](NOTICE) para la atribución completa y la lista de ficheros modificados y añadidos.

---

## Qué incluye

- **Ventana sin modificar:** usa **Alacritty `0.17.0`** oficial con [`extras/alacritty.toml`](extras/alacritty.toml) (`option_as_alt = "OnlyLeft"` para conservar `@ # [ ] { } | \ ~ €` en teclado español con Option derecha).
- **Multiplexor (`velm`):** fork de `herdr` v0.9.5 con configuración en `~/.config/velm/config.toml` y sockets propios (`velm.sock`), sin comprobación de actualizaciones, sin `update` y sin descarga remota de plugins. Soporta marcas semánticas `OSC 133` e ignora silenciosamente la sección `[term]`.
- **Integración de shell ([`shell/term.zsh`](shell/term.zsh)):** emite marcas `OSC 133` (`A`/`B`/`C`/`D`) en `zsh` y añade un widget `zle` (por defecto `^G`) para pedir comandos al asistente local.
- **Piezas propias (`crates/term-*`, todas apagadas por defecto con `enabled = false`):**
  - [`term-ai`](docs/asistente.md): sugiere comandos en `zsh`, explica la salida del último comando (`term.explain_last`), diagnostica servidores (`doctor`) y ejecuta batería (`bench`) contra un servidor local compatible con OpenAI (LM Studio, Ollama o `llama.cpp`) **solo por HTTP en loopback y sin TLS**.
  - [`term-repos`](docs/repos.md): panel TUI / `--json` de solo lectura para inspeccionar el estado de repositorios Git invocando `git` con `GIT_OPTIONAL_LOCKS=0` y `GIT_TERMINAL_PROMPT=0`.
  - [`term-sql`](docs/sql.md): cliente TUI de Postgres para entornos `local` y `dev` (`env = "prod"` está prohibido por configuración), credenciales en el Llavero de macOS, modo lectura por defecto y modo escritura con análisis AST (`sqlparser`), confirmaciones explícitas y cuenta atrás visible de transacción.
  - [`term-claude`](docs/claude.md): panel TUI / `--json` de solo lectura sobre `~/.claude/projects` con deduplicación estricta por `message.id`, separación de tokens `sidechain`, cálculo de coste (`cost-state` o precios configurados) y alerta de contexto.

---

## Instalación (macOS Apple Silicon)

### 1. Requisitos previos

- macOS en Apple Silicon (`arm64`) y Xcode Command Line Tools (`xcode-select --install`).
- Toolchain de Rust fijado en [`rust-toolchain.toml`](rust-toolchain.toml) (`1.96.1`).
- **Zig `0.16.0`** en el `PATH` (requerido por `crates/ghostty-vt/build.rs` para compilar `libghostty-vt`).

### 2. Instalar Alacritty `0.17.0`

El cask de Homebrew `alacritty` está deshabilitado desde el 2026-09-01 por Gatekeeper. Instala Alacritty `0.17.0` desde el **DMG oficial** en las [releases de Alacritty](https://github.com/alacritty/alacritty/releases/tag/v0.17.0).

Copia la configuración recomendada para macOS:

```bash
mkdir -p ~/.config/alacritty
cp extras/alacritty.toml ~/.config/alacritty/alacritty.toml
```

### 3. Instalar `velm` y las herramientas `term-*`

Desde la raíz del repositorio (o usando el binario `macos-arm64` publicado en GitHub Releases):

```bash
cargo install --path . --locked
cargo install --path crates/term-ai --locked
cargo install --path crates/term-repos --locked
cargo install --path crates/term-sql --locked
cargo install --path crates/term-claude --locked
```

Asegúrate de que `~/.cargo/bin` está en tu `PATH` dentro de `~/.zshrc`. Al abrir Alacritty desde el Dock, `extras/alacritty.toml` arranca `/bin/zsh -lc "exec velm"`.

### 4. Activar integración de `zsh` y configuración `[term]`

Añade a tu `~/.zshrc`:

```zsh
source /ruta/a/velm/shell/term.zsh
```

Genera la configuración base si no existe:

```bash
mkdir -p ~/.config/velm
velm --default-config > ~/.config/velm/config.toml
```

Cada herramienta arranca desactivada (`enabled = false`). Activa en `~/.config/velm/config.toml` solo lo que quieras usar y añade los atajos en `[[keys.command]]` (en [`extras/alacritty.toml`](extras/alacritty.toml) los atajos `Cmd+*` ya envían estas secuencias):

```toml
[[keys.command]]
key = "prefix+y"
command = "term.copy_last_output"

[[keys.command]]
key = "prefix+alt+e"
command = "term.explain_last"

[[keys.command]]
key = "prefix+alt+r"
type = "pane"
command = "term-repos"

[[keys.command]]
key = "prefix+alt+s"
type = "pane"
command = "term-sql"

[[keys.command]]
key = "prefix+alt+l"
type = "pane"
command = "term-claude"

[term.ai]
enabled = true

[term.repos]
enabled = true

[term.sql]
enabled = true

[term.claude]
enabled = true
```

---

## Medición de memoria

La métrica usada es el **Physical footprint** devuelto por `vmmap --summary <pid>` (equivalente a la columna Memoria del Monitor de Actividad), sumando **Alacritty + servidor de `velm` + cliente de `velm`**, sin contar las shells ni los procesos ejecutados dentro de los paneles.

El script [`scripts/mem.sh`](scripts/mem.sh) automatiza 3 rondas por escenario con configuración aislada y muestra la **mediana** en MiB:

| Escenario | Descripción |
|---|---|
| `E1` | Una pestaña recién abierta e inactiva 30 s |
| `E2` | Tras ejecutar `seq 1 100000` |
| `E3` | Cinco pestañas abiertas |
| `E4` | `E1` tras abrir y cerrar una división (`VELM_MEM_INCLUDE_E4=1`) |

```bash
./scripts/mem.sh
# Incluir E4:
VELM_MEM_INCLUDE_E4=1 ./scripts/mem.sh
```

---

## Cómo verificar la privacidad

La regla de diseño es que **ningún binario conecta con ningún destino externo que no hayas configurado o pedido explícitamente en ese momento** (cero telemetría, cero comprobación de actualizaciones).

1. **Auditoría de dependencias y TLS (`cargo-deny`):**
   ```bash
   cargo deny check
   ```
   [`deny.toml`](deny.toml) bloquea `reqwest`, `ureq`, `hyper`, `isahc`, `curl`, `surf`, `attohttpc` y cualquier crate de TLS (`rustls`, `native-tls`, `openssl`) salvo en los wrappers permitidos (`term-ai` —compilado con `ureq` sin TLS— y `term-sql`).

2. **Comprobación estática de código (CI):**
   [`.github/workflows/term-ci.yml`](.github/workflows/term-ci.yml) verifica en cada PR/push que no exista `std::net` fuera de `term-ai`/`term-sql` ni invocaciones a `curl`, `wget` o `nc` fuera de tests.

3. **Monitorización en tiempo real con `lsof`:**
   ```bash
   ./scripts/net.sh        # 10 minutos por defecto, muestreo cada 5 s
   ./scripts/net.sh 1      # prueba rápida de 1 minuto
   ```
   Inspecciona los procesos `alacritty` y `velm` (sin contar shells hijas) con `lsof -i -P -n` y falla si observa cualquier conexión de red.

---

## Documentación por herramienta

- [Asistente local (`term-ai`) y marcas `OSC 133`](docs/asistente.md)
- [Panel de repositorios (`term-repos`)](docs/repos.md)
- [Panel Postgres seguro (`term-sql`)](docs/sql.md)
- [Panel de sesiones de Claude Code (`term-claude`)](docs/claude.md)
- [Historial de cambios](CHANGELOG.md) · [Atribución y modificaciones (NOTICE)](NOTICE) · [Licencia Apache-2.0](LICENSE)
