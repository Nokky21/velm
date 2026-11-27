# Asistente local (`term-ai`) e integración `OSC 133`

`term-ai` conecta con un servidor local compatible con la API de OpenAI (`GET /v1/models` y `POST /v1/chat/completions` con streaming) para sugerir comandos en `zsh` y explicar la salida del último comando.

## Garantías de privacidad

- **Solo loopback sin TLS:** `term-ai` compila `ureq` con `default-features = false` (sin `rustls`, `native-tls` ni `openssl`).
- **Validación estricta de URL:** solo admite esquema `http://` y hosts `127.0.0.1`, `::1` o `localhost` (resuelto y validado contra loopback antes de abrir el socket; no se siguen redirecciones HTTP).
- **Contexto mínimo:** en `suggest` solo envía versión de macOS, `zsh`, `--cwd` y `--branch` (si se pasan). Nunca lee historial ni archivos del disco por su cuenta.
- **Comandos peligrosos protegidos:** nunca ejecuta comandos; si detecta un patrón destructivo (por segmentos, evitando falsos positivos en literales entrecomillados), antepone `# ` a cada línea y sale con código `4`.

---

## Configuración (`[term.ai]`)

En `~/.config/velm/config.toml`:

| Clave | Tipo | Defecto | Validación |
|---|---|---|---|
| `enabled` | bool | `false` | — |
| `server` | string | `"auto"` | `auto` \| `lmstudio` \| `ollama` \| `llamacpp` \| `openai-compatible` |
| `url` | string | `""` | Vacía (autodetección) o `http://` local terminada en `/v1` (normaliza barra final) |
| `model` | string | `""` | Vacío = primer modelo devuelto por `/v1/models` |
| `temperature` | float | `0.2` | `0.0` a `1.0` |
| `max_tokens_suggest` | int | `512` | `64` a `8192` (incluye razonamiento `<think>` si el modelo lo emite) |
| `max_tokens_explain` | int | `2048` | `256` a `16384` |
| `lmstudio_ttl_seconds` | int | `600` | `30` a `86400` (solo se envía si el servidor es `lmstudio`) |
| `zsh_key` | string | `"^G"` | Notación `bindkey` de `zsh` |
| `explain_head_lines` | int | `50` | `>= 0` |
| `explain_tail_lines` | int | `150` | `>= 1` |
| `explain_max_chars` | int | `12000` | `1000` a `100000` |

Ejemplo mínimo:

```toml
[term.ai]
enabled = true
server = "auto"
model = "qwen3.8-9b-distill-uncensored-heretic"
```

### Autodetección (`url = ""`)

Prueba en orden con un límite de 300 ms por puerto:

1. `http://127.0.0.1:1234/v1` (LM Studio)
2. `http://127.0.0.1:11434/v1` (Ollama)
3. `http://127.0.0.1:8080/v1` (`llama.cpp`)

Se queda con el primer puerto que responda `200 OK` con **al menos un modelo** en `/v1/models` (si un servidor está encendido sin modelos, continúa al siguiente).

---

## Uso CLI

```bash
term-ai [--config <ruta>] <subcomando>
```

### `suggest`

```bash
term-ai suggest [--cwd <carpeta>] [--branch <rama>] -- <texto>
```

Extrae el primer bloque de código (o una única línea sin prosa) y elimina `$ ` inicial. Descarta etiquetas `<think>…</think>` y campos `reasoning_content` / `reasoning`.

### `explain`

```bash
term-ai explain --input-file <ruta> [--delete-input] [--no-wait]
```

Lee un fichero UTF-8 con formato:

```text
command: <línea de comando>
exit: <código o «desconocido»>
truncated: <líneas omitidas>
---
<salida recortada>
```

Emite la explicación en streaming por `stdout`. Con `--delete-input` borra el fichero nada más leerlo (incluso si falla después). Al terminar espera `q` (o `Esc`/`Ctrl+C`) en terminal interactiva salvo `--no-wait`.

### `doctor` y `bench`

```bash
term-ai doctor
term-ai bench [--battery <ruta>] [--no-interactive]
```

- `doctor`: valida configuración, sondea los puertos locales y lista modelos disponibles.
- `bench`: ejecuta una batería TOML (`crates/term-ai/tests/battery.toml` por defecto) midiendo aciertos y latencias.

---

## Integración en `zsh` y en `velm`

1. **Sugerencias en línea (`shell/term.zsh`):** escribe la descripción en el prompt y pulsa `Ctrl+G` (o `zsh_key`). Muestra `pensando…` en `zle -M`, sustituye `BUFFER` si hay comando y avisa si sale comentado por peligroso.
2. **Marcas `OSC 133`:** `shell/term.zsh` emite `\e]133;A\a`, `\e]133;B\a`, `\e]133;C\a` y `\e]133;D;<código>\a`.
3. **Acciones en `velm` (`[[keys.command]]`):**
   - `term.copy_last_output`: copia al portapapeles la salida del último comando marcado con `OSC 133`.
   - `term.explain_last`: recorta la última salida (`50` líneas iniciales, `150` finales, máx. `12000` caracteres), la escribe en un fichero temporal `0600` (`O_EXCL`) y abre un panel con `term-ai explain --input-file … --delete-input`. Si la sesión no tiene marcas (por ejemplo dentro de `ssh` o `docker exec`), muestra `esta sesión no tiene marcas de shell`.

---

## Códigos de salida

| Código | Significado |
|---|---|
| `0` | Éxito |
| `1` | Error en tiempo de ejecución (servidor inaccesible, modelo inexistente, timeout, redirección, respuesta vacía) |
| `2` | Configuración o argumentos incorrectos (incluye URL no local) |
| `3` | `suggest`: el modelo respondió `NO_COMMAND` o no se pudo extraer comando |
| `4` | `suggest`: comando peligroso devuelto comentado con `# ` |
| `130` | Cancelado por el usuario (`Esc` o `SIGINT`) |
