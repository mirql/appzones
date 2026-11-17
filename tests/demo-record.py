"""Records the README's terminal demo (tests/demo.nix runs it in a VM).

Runs bash in a pty and writes what the terminal shows as an asciicast v2
file. "run" steps are typed into the pty, key by key; the shell's echo of
those keys is replaced in the recording by the same characters, coloured
(command, subcommand, flags, paths, URLs). "say" steps are narration: written
to the recording only, on their own line above the prompt, never typed.

Usage: demo-record.py STEPS.json OUT.cast [ROWS]
"""
import codecs
import fcntl
import json
import os
import pty
import random
import re
import select
import struct
import sys
import termios
import time

COLS = 96
ROWS = int(sys.argv[3]) if len(sys.argv) > 3 else 24


def sgr(rgb: str, style: str = "") -> str:
    return f"\x1b[{style}38;2;{rgb}m"


# Catppuccin Mocha, as the zones' colours
RESET = "\x1b[0m"
SAY = sgr("249;226;175", "3;")  # italic yellow
CMD, SUB, FLAG = sgr("137;180;250", "1;"), sgr("203;166;247"), sgr("250;179;135")
URL, PATH, TEXT = sgr("148;226;213", "4;"), sgr("245;194;231"), sgr("205;214;244")
ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07|\r")


def colours(line: str) -> list[tuple[str, str]]:
    """(char, colour) for each character of a command line."""
    out: list[tuple[str, str]] = []
    words = line.split(" ")
    for i, w in enumerate(words):
        if i == 0:
            c = CMD
        elif i == 1 and words[0] == "appzones":
            c = SUB
        elif w.startswith("-") or w.isdigit():
            c = FLAG
        elif "://" in w:
            c = URL
        elif w.startswith("~") or "/" in w:
            c = PATH
        else:
            c = TEXT
        if i:
            out.append((" ", ""))
        out += [(ch, c) for ch in w]
    return out


steps = json.load(open(sys.argv[1]))
cast = open(sys.argv[2], "w")
cast.write(json.dumps({"version": 2, "width": COLS, "height": ROWS,
                       "env": {"TERM": "xterm-256color", "SHELL": "/bin/bash"}}) + "\n")
t0 = time.time()
dec = codecs.getincrementaldecoder("utf-8")("replace")


def emit(text: str) -> None:
    cast.write(json.dumps([round(time.time() - t0, 3), "o", text]) + "\n")


pid, fd = pty.fork()
if pid == 0:
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    os.environ.update(TERM="xterm-256color", COLUMNS=str(COLS), LINES=str(ROWS))
    os.execvp("bash", ["bash", "-i"])

tail = b""


def read(secs: float) -> bytes | None:
    """The pty's next output (b"" if none within secs), None at EOF."""
    global tail
    r, _, _ = select.select([fd], [], [], secs)
    if not r:
        return b""
    try:
        data = os.read(fd, 65536)
    except OSError:
        return None
    if not data:
        return None
    tail = (tail + data)[-4096:]
    return data


def pump(secs: float) -> bool:
    """Copy the pty's output into the cast for up to secs; False at EOF."""
    data = read(secs)
    if data is None:
        return False
    text = dec.decode(data)
    if text:
        emit(text)
    return True


def drop(secs: float) -> None:
    """Read and discard the pty's output: the echo of our own keys."""
    while read(secs):
        pass


def quiet(secs: float) -> bool:
    r, _, _ = select.select([fd], [], [], secs)
    return not r


def wait_prompt(limit: float = 60) -> None:
    """Until the screen ends in a shell prompt ("$ " or "# ") and is quiet.
    Raises TimeoutError after limit seconds, and RuntimeError if the shell
    exits first: a recording must not go on past a step that never finished."""
    end = time.time() + limit
    while time.time() < end:
        if not pump(0.4):
            raise RuntimeError(f"the shell exited before its prompt; the screen ends with {tail[-200:]!r}")
        if re.search(rb"[$#] $", ANSI.sub(b"", tail)) and quiet(0.4):
            return
    raise TimeoutError(f"no shell prompt after {limit:.0f} s; the screen ends with {tail[-200:]!r}")


said = False


def say(text: str) -> None:
    """Narration above the prompt, after a blank line (not before the first)."""
    global said
    prompt = tail.rsplit(b"\n", 1)[-1].decode(errors="replace")
    emit("\r\x1b[2K" + ("\r\n" if said else "") + SAY)
    said = True
    for ch in "# " + text:
        emit(ch)
        time.sleep(0.012)
    emit(RESET + "\r\n" + prompt)
    time.sleep(0.7)


def run(line: str) -> None:
    for ch, c in colours(line):
        os.write(fd, ch.encode())
        emit(c + ch + RESET if c else ch)
        drop(0)
        time.sleep(random.uniform(0.025, 0.045))
    time.sleep(0.25)
    drop(0.1)
    os.write(fd, b"\r")
    wait_prompt()
    time.sleep(0.8)
    while pump(0.05) and not quiet(0):
        pass


wait_prompt()
time.sleep(0.5)
for step in steps:
    if "say" in step:
        say(step["say"])
    else:
        run(step["run"])
time.sleep(2)
cast.close()
# after the recording, which ends on a prompt: leave the zone's shell (if
# one is open) and the host's
for _ in range(3):
    try:
        os.write(fd, b"exit\r")
    except OSError:
        break
    while read(1):
        pass
if os.waitpid(pid, os.WNOHANG) == (0, 0):
    os.kill(pid, 9)
    os.waitpid(pid, 0)
