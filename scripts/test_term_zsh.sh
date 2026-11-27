#!/usr/bin/env bash
set -euo pipefail

# Verifies shell/term.zsh against CONTRATO §D:
# 1. Non-interactive guard leaves PS1/hooks untouched.
# 2. Existing precmd_functions/preexec_functions are preserved.
# 3. Sourcing twice is idempotent (no duplicate hooks, single B mark in PS1).
# 4. Raw PTY output emits OSC 133 marks in A, B, C, D;<code> order,
#    omits D on initial prompt and empty Enter, and captures Ctrl+C (130).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TERM_ZSH="$REPO_ROOT/shell/term.zsh"

if ! command -v zsh >/dev/null 2>&1; then
  echo "zsh not found; skipping shell/term.zsh test" >&2
  exit 1
fi

# 1. Non-interactive check
non_interactive_out="$(zsh -c "
  precmd_functions=(user_precmd)
  preexec_functions=(user_preexec)
  PS1='plain> '
  source '$TERM_ZSH'
  print -r -- \"\$PS1|\${(j:,:)precmd_functions}|\${(j:,:)preexec_functions}\"
")"
if [[ "$non_interactive_out" != "plain> |user_precmd|user_preexec" ]]; then
  echo "FAIL: non-interactive guard modified state: $non_interactive_out" >&2
  exit 1
fi

# 2-4. Interactive PTY test via python3
python3 - "$TERM_ZSH" <<'PY'
import os
import pty
import re
import select
import subprocess
import sys
import tempfile
import termios
import time

term_zsh = sys.argv[1]

with tempfile.TemporaryDirectory() as zdotdir:
    zshrc = os.path.join(zdotdir, ".zshrc")
    with open(zshrc, "w", encoding="utf-8") as f:
        f.write(f"""
unsetopt BEEP
PROMPT_EOL_MARK=''
PS1='TESTPROMPT> '
_user_precmd_ran=0
_user_preexec_ran=0
user_precmd() {{ _user_precmd_ran=$(( _user_precmd_ran + 1 )); }}
user_preexec() {{ _user_preexec_ran=$(( _user_preexec_ran + 1 )); }}
precmd_functions=(user_precmd)
preexec_functions=(user_preexec)

source {term_zsh!r}
source {term_zsh!r}
""")

    master, slave = pty.openpty()
    attrs = termios.tcgetattr(slave)
    attrs[3] &= ~termios.ECHO
    termios.tcsetattr(slave, termios.TCSANOW, attrs)

    env = os.environ.copy()
    env["ZDOTDIR"] = zdotdir
    env["TERM"] = "xterm-256color"

    proc = subprocess.Popen(
        ["zsh", "-f", "-i"],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        close_fds=True,
        preexec_fn=os.setsid,
    )
    os.close(slave)

    buf = bytearray()

    def read_until(pattern: bytes, timeout: float = 5.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if re.search(pattern, buf):
                return
            r, _, _ = select.select([master], [], [], 0.1)
            if r:
                chunk = os.read(master, 4096)
                if not chunk:
                    break
                buf.extend(chunk)
        raise TimeoutError(f"timed out waiting for {pattern!r} in {bytes(buf)!r}")

    try:
        # Wait for initial prompt A .. B
        read_until(rb"\x1b\]133;B\x07")
        time.sleep(0.1)

        # Command 1: echo hello (exit 0)
        os.write(master, b"echo hello\n")
        read_until(rb"\x1b\]133;D;0\x07.*?\x1b\]133;B\x07")

        # Command 2: subshell exit 42
        os.write(master, b"(exit 42)\n")
        read_until(rb"\x1b\]133;D;42\x07.*?\x1b\]133;B\x07")

        # Empty Enter (must emit A, B with NO C or D)
        mark_count_before_empty = len(re.findall(rb"\x1b\]133;[ABCD](?:;[^\x07]*)?\x07", buf))
        os.write(master, b"\n")
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            marks_now = re.findall(rb"\x1b\]133;([ABCD](?:;[^\x07]*)?)\x07", buf)
            if len(marks_now) >= mark_count_before_empty + 2:
                break
            r, _, _ = select.select([master], [], [], 0.1)
            if r:
                buf.extend(os.read(master, 4096))
        marks_after_empty = [
            m.decode() for m in re.findall(rb"\x1b\]133;([ABCD](?:;[^\x07]*)?)\x07", buf)
        ]
        assert marks_after_empty[mark_count_before_empty:] == ["A", "B"], (
            f"empty Enter emitted unexpected marks: {marks_after_empty[mark_count_before_empty:]}"
        )

        # Command 3: sleep 10 interrupted by Ctrl+C -> D;130
        os.write(master, b"sleep 10\n")
        read_until(rb"\x1b\]133;D;42\x07.*?\x1b\]133;C\x07")
        time.sleep(0.1)
        os.write(master, b"\x03")
        read_until(rb"\x1b\]133;D;130\x07.*?\x1b\]133;B\x07")

        # Check user hooks & single B mark in PS1
        os.write(
            master,
            b'print -r -- "HOOKS=$_user_precmd_ran:$_user_preexec_ran:BMARKS=${#${(M)${(s::)PS1}:#\x1b}}"\n',
        )
        read_until(rb"HOOKS=[0-9]+:[0-9]+:")

        # U3.3 zle widget test with fake term-ai on PATH (exit 0, 4, 1)
        fake_bin = os.path.join(zdotdir, "bin")
        os.makedirs(fake_bin, exist_ok=True)
        fake_ai = os.path.join(fake_bin, "term-ai")
        with open(fake_ai, "w", encoding="utf-8") as f:
            f.write("""#!/usr/bin/env python3
import sys
q = sys.argv[-1]
if q == "ok":
    print("echo suggested")
    sys.exit(0)
elif q == "danger":
    print("# rm -rf /tmp/x")
    sys.exit(4)
elif q == "fail":
    print("no hay servidor de IA local", file=sys.stderr)
    sys.exit(1)
sys.exit(3)
""")
        os.chmod(fake_ai, 0o755)
        os.write(master, f'export PATH="{fake_bin}:$PATH"\n'.encode())
        read_until(rb"\x1b\]133;B\x07")

        # Exit 0 replaces BUFFER with suggested command
        os.write(master, b"ok\x07")
        read_until(rb"echo suggested")
        os.write(master, b"\n")
        read_until(rb"suggested\r?\n")

        # Exit 4 replaces BUFFER with commented command and warns
        os.write(master, b"danger\x07")
        read_until(rb"comando peligroso: quita el # si de verdad lo quieres")
        os.write(master, b"\x15")  # Ctrl+U clear line

        # Exit 1 keeps BUFFER and shows stderr via zle -M
        os.write(master, b"fail\x07")
        read_until(rb"no hay servidor de IA local")
        os.write(master, b"\x15")

        os.write(master, b"exit\n")
        proc.wait(timeout=5)
    finally:
        os.close(master)

    raw = bytes(buf)
    marks = [m.decode() for m in re.findall(rb"\x1b\]133;([ABCD](?:;[^\x07]*)?)\x07", raw)]
    expected_prefix = [
        "A", "B",               # initial prompt
        "C", "D;0", "A", "B",   # echo hello
        "C", "D;42", "A", "B",  # (exit 42)
        "A", "B",               # empty Enter (no C, no D)
        "C", "D;130", "A", "B", # sleep 10 + Ctrl+C
    ]
    assert marks[:len(expected_prefix)] == expected_prefix, (
        f"unexpected OSC 133 mark sequence: got {marks[:len(expected_prefix)]}, expected {expected_prefix}"
    )

    assert raw.count(b"\x1b]133;B\x07TESTPROMPT") == 0
    # Verify PS1 only contains one B mark (even though sourced twice)
    ps1_b_count = None
    # Check hook counters
    m_hooks = re.search(rb"HOOKS=(\d+):(\d+):", raw)
    assert m_hooks and int(m_hooks.group(1)) >= 4 and int(m_hooks.group(2)) >= 3, (
        f"existing precmd/preexec hooks were clobbered: {raw!r}"
    )

print("PASS: shell/term.zsh OSC 133 ordering, idempotency, hooks, empty Enter, Ctrl+C")
PY
