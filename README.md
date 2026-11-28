<div align="center">

<img src="assets/velm-banner.svg" alt="velm — Multiplexor de terminal en Rust con privacidad verificable y suite term-*" width="100%" />

<br />
<br />

[![CI (term-ci)](https://github.com/Nokky21/velm/actions/workflows/term-ci.yml/badge.svg)](https://github.com/Nokky21/velm/actions/workflows/term-ci.yml)
![Plataformas: macOS · Linux · Windows](https://img.shields.io/badge/plataformas-macOS%20%C2%B7%20Linux%20%C2%B7%20Windows-1e1e2e?labelColor=313244)
![Rust 1.96.1](https://img.shields.io/badge/Rust-1.96.1-f38ba8?logo=rust&logoColor=cdd6f4&labelColor=313244)
![Privacidad: Cero telemetría](https://img.shields.io/badge/privacidad-cero%20telemetr%C3%ADa-a6e3a1?labelColor=313244)
[![Licencia: Apache-2.0](https://img.shields.io/badge/licencia-Apache--2.0-89b4fa?labelColor=313244)](LICENSE)

<p>
  <strong>Multiplexor de terminal en Rust para macOS, Linux y Windows</strong>,<br />
  con marcas semánticas <code>OSC 133</code>, herramientas locales desacopladas (<code>term-*</code>) y <strong>privacidad verificable</strong>.
</p>

<p>
  <a href="https://nokky21.github.io/velm/">Landing web</a> ·
  <a href="#instalación-rápida">Instalación</a> ·
  <a href="#arquitectura">Arquitectura</a> ·
  <a href="#suite-modular-term-">Suite <code>term-*</code></a> ·
  <a href="#atajos-de-teclado">Atajos</a> ·
  <a href="#verificación-de-privacidad">Privacidad</a> ·
  <a href="#compatibilidad-por-plataforma">Plataformas</a> ·
  <a href="#documentación">Documentación</a>
</p>

</div>

---

> **Sobre el fork:** `velm` es un fork de [`herdrdev/herdr`](https://github.com/herdrdev/herdr) (`v0.9.5`) bajo licencia [Apache-2.0](LICENSE). Consulta [NOTICE](NOTICE) para ver la atribución completa y el inventario exacto de ficheros modificados y añadidos.

## Instalación rápida

### macOS (Apple Silicon / Intel) y Linux (x86_64 / arm64)

Instalador automático con verificación SHA-256 ([`scripts/install.sh`](scripts/install.sh)):

```bash
curl -fsSL https://raw.githubusercontent.com/Nokky21/velm/main/scripts/install.sh | sh
```

### Windows 10 / 11 (x86_64 · PowerShell)

Instalador PowerShell con verificación SHA-256 y alta en `PATH` de usuario ([`scripts/install.ps1`](scripts/install.ps1)):

```powershell
irm https://raw.githubusercontent.com/Nokky21/velm/main/scripts/install.ps1 | iex
```

### Compilar desde código fuente (cualquier plataforma)

Requiere **Rust `1.96.1`** (fijado por [`rust-toolchain.toml`](rust-toolchain.toml)) y **Zig `0.16.0`** en el `PATH` (requerido por [`crates/ghostty-vt/build.rs`](crates/ghostty-vt/build.rs)):

```bash
git clone https://github.com/Nokky21/velm && cd velm
cargo install --path . --locked
cargo install --path crates/term-ai --locked
cargo install --path crates/term-repos --locked
cargo install --path crates/term-sql --locked
cargo install --path crates/term-claude --locked
```

> [!TIP]
> También puedes descargar los binarios comprimidos y su `SHA256SUMS.txt` directamente desde [GitHub Releases](https://github.com/Nokky21/velm/releases) (`macos-arm64`, `macos-x86_64`, `linux-x86_64`, `linux-arm64`, `windows-x86_64`), o consultar la guía interactiva en [`site/index.html`](site/index.html) ([GitHub Pages](https://nokky21.github.io/velm/)).

---

## Por qué `velm`

| Principio | Qué garantiza en la práctica |
|---|---|
| **Privacidad verificable** | Sin telemetría, sin comprobación de actualizaciones (`update` eliminado) y sin descarga remota de plugins. Auditado en CI con `cargo-deny`, chequeo estático de código y monitorización `lsof`. |
| **Modular y opt-in** | Todas las herramientas `crates/term-*` arrancan apagadas (`enabled = false`). Activas solo lo que usas en `~/.config/velm/config.toml`. |
| **Tu terminal sin modificar** | Funciona dentro de **Alacritty**, **Ghostty**, **WezTerm**, **Kitty** o **Windows Terminal**. Incluye [`extras/alacritty.toml`](extras/alacritty.toml) (`option_as_alt = "OnlyLeft"` para conservar `@ # [ ] { } \| \ ~ €` con `Option` derecha en teclado español). |
| **Semántica de shell (`OSC 133`)** | [`shell/term.zsh`](shell/term.zsh) emite marcas `OSC 133` (`A`/`B`/`C`/`D`) para copiar o explicar la salida del último comando al instante. |
| **Aislado de `herdr`** | Configuración propia en `~/.config/velm/config.toml` y sockets propios (`velm.sock`, `velm-client.sock`). Ignora silenciosamente la sección `[term]`. |

---

## Arquitectura

```mermaid
flowchart LR
    A["Alacritty · Ghostty · WezTerm · WT"] --> V["velm<br/>Multiplexor · OSC 133 · velm.sock"]
    V --> Z["zsh + shell/term.zsh<br/>Marcas A/B/C/D · Widget ^G"]
    V --> AI["term-ai<br/>HTTP loopback sin TLS"]
    V --> REPOS["term-repos<br/>Git solo lectura"]
    V --> SQL["term-sql<br/>Postgres local/dev · Llavero"]
    V --> CLAUDE["term-claude<br/>Transcripciones locales"]
    AI -. "127.0.0.1 / ::1" .-> LLM["LM Studio · Ollama · llama.cpp"]
```

---

## Suite modular `term-*`

Todas las herramientas comparten validación centralizada en [`crates/term-config`](crates/term-config) y permanecen desactivadas por defecto (`enabled = false`).

| Herramienta | Qué hace | Política de red / seguridad | Salida | Guía |
|---|---|---|---|---|
| **`term-ai`** | Sugiere comandos en `zsh` (`Ctrl+G`), explica la salida del último comando (`term.explain_last`), diagnostica (`doctor`) y ejecuta batería (`bench`). | **Solo HTTP en loopback** (`127.0.0.1`, `::1`, `localhost`), `ureq` compilado **sin TLS**, sin redirecciones. Comenta con `#` comandos destructivos. | CLI / streaming | [docs/asistente.md](docs/asistente.md) |
| **`term-repos`** | Panel TUI para ver estado, ramas, cambios y worktrees de tus repositorios Git locales en paralelo. | **Solo lectura.** Invoca `git` con `GIT_OPTIONAL_LOCKS=0` y `GIT_TERMINAL_PROMPT=0`. Red solo si pulsas `r`. | TUI / `--json` | [docs/repos.md](docs/repos.md) |
| **`term-sql`** | Cliente TUI de Postgres para entornos `local` y `dev` con modo lectura por defecto y escritura controlada (`F2`). | **`env = "prod"` prohibido.** Credenciales en el **Llavero de macOS**. Análisis AST (`sqlparser`), confirmaciones explícitas y cuenta atrás de transacción. | TUI / CLI `conn` | [docs/sql.md](docs/sql.md) |
| **`term-claude`** | Supervisa sesiones de Claude Code desde `~/.claude/projects`: contexto actual, tokens, `sidechain` y coste. | **Solo lectura local** y cero red. Deduplicación estricta por `message.id`, cálculo por `cost-state` o precios configurados y alerta de contexto. | TUI / `--json` | [docs/claude.md](docs/claude.md) |

---

## Configuración inicial

### 1. Terminal recomendado en macOS: Alacritty `0.17.0`

> [!NOTE]
> El cask de Homebrew `alacritty` está deshabilitado desde el 2026-09-01 por Gatekeeper. Instala Alacritty `0.17.0` desde el **DMG oficial** en las [releases de Alacritty](https://github.com/alacritty/alacritty/releases/tag/v0.17.0).

```bash
mkdir -p ~/.config/alacritty
cp extras/alacritty.toml ~/.config/alacritty/alacritty.toml
```

### 2. Integración de shell (`zsh` y autocompletado)

Añade a tu `~/.zshrc` para activar marcas `OSC 133` y el widget `Ctrl+G`:

```zsh
source /ruta/a/velm/shell/term.zsh
```

Autocompletado nativo para `zsh`, `bash`, `fish`, `elvish` o `powershell`:

```bash
velm completion zsh > ~/.zfunc/_velm
# También: velm completion bash | fish | powershell
```

### 3. Archivo `~/.config/velm/config.toml`

Genera la configuración base:

```bash
mkdir -p ~/.config/velm
velm --default-config > ~/.config/velm/config.toml
```

<details open>
<summary><strong>Ejemplo de configuración para activar <code>[term]</code> y los atajos rápidos</strong></summary>

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

</details>

---

## Atajos de teclado

Con [`extras/alacritty.toml`](extras/alacritty.toml) y [`shell/term.zsh`](shell/term.zsh) activos (prefijo por defecto de `velm`: `Ctrl+B`):

| Atajo macOS | Secuencia `velm` | Acción |
|---|---|---|
| `⌘T` | `prefix+c` | Nueva pestaña |
| `⌘W` | `prefix+x` | Cerrar panel actual |
| `⌘D` / `⌘⇧D` | `prefix+v` / `prefix+-` | Dividir panel vertical / horizontal |
| `⌘[` / `⌘]` | `prefix+p` / `prefix+n` | Pestaña anterior / siguiente |
| `⌘1` … `⌘9` | `prefix+1` … `prefix+9` | Saltar directamente a pestaña `1`–`9` |
| `⌘⇧↵` | `prefix+z` | Zoom sobre el panel activo |
| `Ctrl+G` | widget `zle` (`zsh`) | Pedir sugerencia de comando a `term-ai` |
| — | `prefix+y` | Copiar salida del último comando (`OSC 133`) |
| `⌘E` | `prefix+alt+e` | Explicar última salida con `term-ai` (`term.explain_last`) |
| `⌘⇧R` | `prefix+alt+r` | Abrir panel `term-repos` |
| `⌘⇧S` | `prefix+alt+s` | Abrir panel `term-sql` |
| `⌘⇧L` | `prefix+alt+l` | Abrir panel `term-claude` |

---

## Verificación de privacidad

> **Regla de diseño:** ningún binario conecta con ningún destino externo que no hayas configurado o pedido explícitamente en ese momento. Cero telemetría y cero comprobaciones automáticas en segundo plano.

La privacidad se valida en **tres capas independientes**:

| Capa | Herramienta / Script | Qué comprueba |
|---|---|---|
| **1. Dependencias y TLS** | `cargo deny check` ([`deny.toml`](deny.toml)) | Bloquea `reqwest`, `hyper`, `isahc`, `curl`, `surf`, `attohttpc` y todo crate TLS (`rustls`, `native-tls`, `openssl`) en todo el workspace; solo `term-ai` compila `ureq` sin TLS. |
| **2. Análisis estático en CI** | [`.github/workflows/term-ci.yml`](.github/workflows/term-ci.yml) | Verifica en cada push y PR que no exista `std::net` fuera de `term-ai`/`term-sql` ni llamadas a `curl`, `wget` o `nc` fuera de tests. |
| **3. Auditoría en tiempo real** | [`./scripts/net.sh`](scripts/net.sh) | Inspecciona `alacritty` y `velm` (sin contar shells hijas) con `lsof -i -P -n` cada 5 s y falla si detecta cualquier conexión de red. |

```bash
cargo deny check
./scripts/net.sh        # 10 minutos por defecto
./scripts/net.sh 1      # comprobación rápida de 1 minuto
```

---

## Compatibilidad por plataforma

| Componente | macOS (arm64 / x86_64) | Linux (x86_64 / arm64) | Windows (x86_64) |
|---|---|---|---|
| **`velm` multiplexor + `OSC 133`** | Nativo | Nativo (musl) | Nativo (ConPTY) |
| **`term-ai`** (loopback sin TLS) | Nativo | Nativo | Nativo |
| **`term-repos`** (Git solo lectura) | Nativo | Nativo | Nativo |
| **`term-claude`** (sesiones locales) | Nativo | Nativo | Nativo |
| **`term-sql`** (Postgres `local`/`dev`) | Nativo (Llavero macOS) | Compila (Keyring en roadmap) | Compila (Cred. Manager en roadmap) |
| **Integración `shell/term.zsh`** | Nativo (`zsh`) | Nativo (`zsh`) | WSL / Git Bash |
| **Auditoría `net.sh` / `mem.sh`** | `lsof` + `vmmap` | `net.sh` (`lsof`) | Monitor de recursos / PowerShell |

---

## Medición de memoria

En macOS la métrica oficial es el **Physical footprint** devuelto por `vmmap --summary <pid>` (equivalente a la columna *Memoria* del Monitor de Actividad), sumando **Alacritty + servidor de `velm` + cliente de `velm`** (excluyendo las shells y procesos hijos dentro de cada panel).

[`scripts/mem.sh`](scripts/mem.sh) ejecuta 3 rondas aisladas por escenario y devuelve la **mediana en MiB**:

| Escenario | Descripción |
|---|---|
| `E1` | Una pestaña recién abierta e inactiva durante 30 s |
| `E2` | Tras ejecutar `seq 1 100000` |
| `E3` | Cinco pestañas abiertas simultáneamente |
| `E4` | `E1` tras abrir y cerrar una división (`VELM_MEM_INCLUDE_E4=1`) |

```bash
./scripts/mem.sh
VELM_MEM_INCLUDE_E4=1 ./scripts/mem.sh
```

---

## Documentación

| Recurso | Contenido |
|---|---|
| [Landing interactiva (`site/index.html`)](site/index.html) | Demos TUI interactivas, descargas por SO y generador visual de `config.toml` |
| [Asistente local (`term-ai`) y `OSC 133`](docs/asistente.md) | Autodetección de LM Studio / Ollama / `llama.cpp`, `suggest`, `explain`, `doctor`, `bench` y códigos de salida |
| [Panel de repositorios (`term-repos`)](docs/repos.md) | Configuración `[term.repos]`, atajos TUI y esquema `--json` |
| [Panel Postgres seguro (`term-sql`)](docs/sql.md) | Modelo de seguridad `local`/`dev`, Llavero de macOS (`conn add/rm/list`), reglas AST y transacciones |
| [Panel de Claude Code (`term-claude`)](docs/claude.md) | Lectura incremental, deduplicación por `message.id`, `sidechain`, costes y alertas de contexto |
| [Historial de cambios](CHANGELOG.md) | Evolución de `velm` e historial heredado de `herdr` |
| [Atribución y modificaciones](NOTICE) | Lista completa de ficheros modificados y añadidos respecto a `herdr v0.9.5` |
| [Licencia Apache-2.0](LICENSE) | Términos de licencia |

---

<div align="center">
  <sub>Hecho con Rust sobre <a href="https://github.com/herdrdev/herdr">herdr</a> v0.9.5 y <code>libghostty-vt</code> · Licencia <a href="LICENSE">Apache-2.0</a></sub>
</div>
