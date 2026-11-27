# Velm shell integration: OSC 133 semantic prompt marks (CONTRATO §D)

# Do nothing in non-interactive shells.
[[ -o interactive ]] || return 0

# Track whether a command has executed since the last prompt (A mark).
if (( ! ${+_term_osc133_cmd_active} )); then
  typeset -gi _term_osc133_cmd_active=0
fi

_term_osc133_precmd_start() {
  local exit_code=$?
  if (( _term_osc133_cmd_active )); then
    printf '\e]133;D;%d\a' "$exit_code"
    _term_osc133_cmd_active=0
  fi
}

_term_osc133_precmd_end() {
  printf '\e]133;A\a'
}

_term_osc133_preexec() {
  _term_osc133_cmd_active=1
  printf '\e]133;C\a'
}

if (( ! ${+precmd_functions} )); then
  typeset -ga precmd_functions
fi
if (( ! ${+preexec_functions} )); then
  typeset -ga preexec_functions
fi

if (( ! ${precmd_functions[(Ie)_term_osc133_precmd_start]} )); then
  if (( ${#precmd_functions} )); then
    precmd_functions=(_term_osc133_precmd_start "${precmd_functions[@]}")
  else
    precmd_functions=(_term_osc133_precmd_start)
  fi
fi

if (( ! ${precmd_functions[(Ie)_term_osc133_precmd_end]} )); then
  precmd_functions+=(_term_osc133_precmd_end)
fi

if (( ! ${preexec_functions[(Ie)_term_osc133_preexec]} )); then
  preexec_functions+=(_term_osc133_preexec)
fi

_term_osc133_b_mark=$'\e]133;B\a'
if [[ "${PS1:-}" != *"$_term_osc133_b_mark"* ]]; then
  PS1="${PS1:-}%{${_term_osc133_b_mark}%}"
fi
unset _term_osc133_b_mark

# Velm AI assistant zle widget (U3.3, CONTRATO §A.1 / §B.1, PLAN §5.3)
_term_ai_read_zsh_key() {
  setopt local_options extended_glob
  if [[ -n "${TERM_AI_ZSH_KEY:-}" ]]; then
    print -r -- "$TERM_AI_ZSH_KEY"
    return 0
  fi
  local cfg="${XDG_CONFIG_HOME:-$HOME/.config}/velm/config.toml"
  if [[ -r "$cfg" ]]; then
    local in_section=0 line key_val
    while IFS= read -r line || [[ -n "$line" ]]; do
      line="${line%%#*}"
      line="${line##[[:space:]]##}"
      line="${line%%[[:space:]]##}"
      [[ -z "$line" ]] && continue
      if [[ "$line" == \[*\] ]]; then
        if [[ "$line" == "[term.ai]" ]]; then
          in_section=1
        else
          in_section=0
        fi
        continue
      fi
      if (( in_section )) && [[ "$line" == zsh_key[[:space:]]*=* ]]; then
        key_val="${line#*=}"
        key_val="${key_val##[[:space:]]##}"
        key_val="${key_val%%[[:space:]]##}"
        key_val="${key_val#\"}"
        key_val="${key_val%\"}"
        key_val="${key_val#\'}"
        key_val="${key_val%\'}"
        if [[ -n "$key_val" ]]; then
          print -r -- "$key_val"
          return 0
        fi
      fi
    done < "$cfg"
  fi
  print -r -- "^G"
}

_term_ai_suggest_widget() {
  local query="$BUFFER"
  [[ -n "${query//[[:space:]]/}" ]] || return 0

  if ! command -v term-ai >/dev/null 2>&1; then
    zle -M "term-ai no encontrado en PATH"
    return 0
  fi

  local -a args
  args=(suggest --cwd "$PWD")
  local branch=""
  if command -v git >/dev/null 2>&1; then
    branch="$(git symbolic-ref --quiet --short HEAD 2>/dev/null || git rev-parse --short HEAD 2>/dev/null || true)"
  fi
  if [[ -n "$branch" ]]; then
    args+=(--branch "$branch")
  fi
  args+=(-- "$query")

  zle -M "pensando…"
  zle -R

  local err_file out rc err_msg
  err_file="$(mktemp "${TMPDIR:-/tmp}/term-ai-zle.XXXXXX")" || {
    zle -M "no se pudo crear fichero temporal"
    return 0
  }

  out="$(term-ai "${args[@]}" 2>"$err_file")"
  rc=$?
  err_msg="$(<"$err_file")"
  rm -f -- "$err_file"

  case $rc in
    0)
      BUFFER="$out"
      CURSOR=${#BUFFER}
      zle -M ""
      ;;
    4)
      BUFFER="$out"
      CURSOR=${#BUFFER}
      zle -M "comando peligroso: quita el # si de verdad lo quieres"
      ;;
    130)
      zle -M ""
      ;;
    *)
      err_msg="${err_msg%%$'\n'}"
      zle -M "${err_msg:-error de term-ai (código $rc)}"
      ;;
  esac
}

if zmodload -e zsh/zle 2>/dev/null || [[ -o zle ]] 2>/dev/null || whence -w zle >/dev/null 2>&1; then
  zle -N _term_ai_suggest_widget
  _term_ai_zsh_key="$(_term_ai_read_zsh_key)"
  if [[ -n "$_term_ai_zsh_key" ]]; then
    bindkey "$_term_ai_zsh_key" _term_ai_suggest_widget
  fi
  unset _term_ai_zsh_key
fi
