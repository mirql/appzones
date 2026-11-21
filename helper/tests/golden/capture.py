"""Freeze the Python implementation's answers as golden files, before it was removed.

Run once, from the last tree that still had src/*.py (appzones 0.11.3 on rust-version),
inside a Nix build sandbox, so the file system it saw (/etc, /nix, /run) is the one the
Rust golden tests run in (capture.nix; then copy result/*.json here):

  config.json  every zonecfg.load_text call the unit tests made (944), with the file tree
               and environment it saw, and what Python answered (errors, warnings,
               settings, origins, settings_hash)
  plan.json    make_plan for three zones x two layouts, with the Rust zone-init
  policy.json  mount_source_error over the policy test's paths, and the never-list

Paths of the test's temporary directory are "{T}" in all of them; a case whose answer
names another temporary path is left out (the Rust test could not rebuild it).
"""
import base64
import importlib
import io
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SRC = Path(sys.argv[1]).resolve()
OUT = Path(sys.argv[2])
sys.path.insert(0, str(SRC))
import test_zonecfg as T  # noqa: E402

TMP = tempfile.gettempdir()
ENV_KEYS = ("HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR", "USER", "PATH", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS",
            "XDG_CURRENT_DESKTOP", "LANG")


def tree(root: Path) -> list:
    """Every directory, file and link under ROOT (relative), with modes, contents, ages."""
    out = []
    now = time.time()
    for dirpath, dirs, files in os.walk(root):
        for name in sorted(dirs) + sorted(files):
            p = Path(dirpath) / name
            rel = str(p.relative_to(root))
            st = p.lstat()
            if p.is_symlink():
                out.append({"path": rel, "link": os.readlink(p).replace(str(root), "{T}")})
            elif p.is_dir():
                out.append({"path": rel, "dir": st.st_mode & 0o7777})
            elif p.is_socket() or p.is_fifo():
                out.append({"path": rel, "special": "socket" if p.is_socket() else "fifo"})
            elif p.is_file():
                data = p.read_bytes().replace(str(root).encode(), b"{T}")
                out.append({"path": rel, "mode": st.st_mode & 0o7777, "age": round(now - st.st_mtime),
                            "data": base64.b64encode(data).decode()})
        dirs[:] = [d for d in dirs if not (Path(dirpath) / d).is_symlink()]
    return out


def sub(v, root: str):
    """V with ROOT replaced by {T}, in every string."""
    if isinstance(v, str):
        return v.replace(root, "{T}")
    if isinstance(v, list):
        return [sub(x, root) for x in v]
    if isinstance(v, dict):
        return {sub(k, root): sub(x, root) for k, x in v.items()}
    return v


def parsed(text: str) -> bool:
    import tomllib
    try:
        tomllib.loads(text)
        return True
    except tomllib.TOMLDecodeError:
        return False


def foreign(v) -> bool:
    return TMP in json.dumps(v)


def capture_config() -> list:
    cases = []
    real_hook = T._diff_load_text

    def hook(z):
        root = z.HOME.parent  # the test's temporary directory (home is in it)
        code = root / "code"
        code.mkdir(exist_ok=True)
        z.CODE_DIR = code  # inside the tree, so a Rust run can rebuild it
        real = z.load_text
        snap = {k: getattr(z, k) for k in T._PATCHABLE}

        def load_text(text, path="zones.toml"):
            cfg = real(text, path)
            if any(getattr(z, k) is not v for k, v in snap.items()):
                return cfg  # a test replaced part of zonecfg
            view = T._config_view(z, cfg)
            if view is None:
                return cfg
            # (HOME and PATH are the sandbox's: the test's home is APPZONES_HOME_FOR_TESTS)
            env = {k: v for k, v in os.environ.items()
                   if (k.startswith(("APPZONES_", "ZONES_")) or k in ENV_KEYS) and k not in ("HOME", "PATH")}
            env.update(APPZONES_HOME_FOR_TESTS=str(z.HOME), APPZONES_NIX_LD_SHIM=z.NIX_LD_SHIM,
                       APPZONES_NIX_LD_LIB=z.NIX_LD_LIB)
            case = sub({"text": text, "path": str(path), "env": env, "want": view, "parsed": parsed(text)}, str(root))
            if foreign(case):
                return cfg
            # (the test's own zones.toml is the case's text, written back by the Rust test)
            case["tree"] = [e for e in tree(root) if e["path"] != "zones.toml"]
            cases.append(case)
            return cfg

        z.load_text = load_text

    T._diff_load_text = hook
    T.CONFIG_DIFF["helper"] = "capture"
    try:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(T.ZonecfgTest)
        unittest.TextTestRunner(stream=io.StringIO(), verbosity=0).run(suite)
    finally:
        T._diff_load_text = real_hook
        T.CONFIG_DIFF["helper"] = None
    return cases


def one_binary(v):
    """Python's `PYTHON zone.py CMD` in a plan is `HELPER cli CMD` with one binary."""
    if isinstance(v, list):
        out, i = [], 0
        while i < len(v):
            if v[i:i + 2] == ["{PYTHON}", "{HERE}/zone.py"]:
                out += ["{HELPER}", "cli"]
                i += 2
            else:
                out.append(one_binary(v[i]))
                i += 1
        return out
    if isinstance(v, dict):
        return {k: one_binary(x) for k, x in v.items()}
    return v


def capture_examples() -> list:
    """examples/*.toml, as the differential test loaded them after the unit tests."""
    import zonecfg
    cases = []
    root = Path(tempfile.mkdtemp()).resolve()
    (root / "home").mkdir()
    (root / "code").mkdir()
    for d in ("Downloads", "Pictures", "Projects", "Documents", "Music", "src"):
        (root / "home" / d).mkdir()
    (root / "denied-paths").write_text("")
    os.environ.update(APPZONES_HOME_FOR_TESTS=str(root / "home"), APPZONES_DBUS_SERVICE_DIRS="",
                      APPZONES_DENIED_PATHS=str(root / "denied-paths"), APPZONES_LAYOUT="nixos")
    z = importlib.reload(zonecfg)
    z.CODE_DIR = root / "code"
    for ex in sorted((SRC.parent / "examples").glob("*.toml")):
        text = ex.read_text()
        view = T._config_view(z, z.load_text(text, f"examples/{ex.name}"))
        env = {k: v for k, v in os.environ.items()
               if (k.startswith(("APPZONES_", "ZONES_")) or k in ENV_KEYS) and k not in ("HOME", "PATH")}
        env.update(APPZONES_NIX_LD_SHIM=z.NIX_LD_SHIM, APPZONES_NIX_LD_LIB=z.NIX_LD_LIB)
        cases.append(sub({"text": text, "path": f"examples/{ex.name}", "env": env, "want": view, "parsed": True,
                          "tree": tree(root)}, str(root)))
    return cases


def capture_plan() -> list:
    cases = []
    root = Path(tempfile.mkdtemp()).resolve()
    t = root / "f"
    for layout in ("nixos", "fhs"):
        for zone in ("alpha", "beta", "gamma"):
            env = T._plan_fixture(t)
            # only what the fixture set, and a PATH: the plan records the server's environment
            base = os.environ["PATH"]
            env = {k: v for k, v in env.items() if k.startswith(("APPZONES_", "XDG_")) or k in ENV_KEYS}
            env["PATH"] = f"{t}/bin:{base}"
            env.update(APPZONES_LAYOUT=layout, APPZONES_HELPER="rust", APPZONES_BIN_APPZONES_HELPER="{HELPER}")
            fixture = tree(t)
            fenv = json.loads(json.dumps(sub(env, str(t))).replace(base, "{PATH}"))
            # the Python plan maker, with a stand-in helper that says it implements every in-zone part
            fake = root / "fake-helper"
            fake.write_text("#!/bin/sh\n[ \"$1\" = --commands ] && printf 'zone-wayland\\nzone-init\\nopen-link\\nserve\\nplan\\ncli\\n'\n")
            fake.chmod(0o755)
            env["APPZONES_BIN_APPZONES_HELPER"] = str(fake)
            code = ("import sys; sys.path.insert(0, sys.argv[2]); from appzones import server; "
                    "print(server.write_plan(server.make_plan(sys.argv[1])))")
            r = subprocess.run([sys.executable, "-B", "-c", code, zone, str(SRC)], env=env, capture_output=True, text=True)
            snap = T._snapshot(t)
            want = {"rc": r.returncode, "stdout": r.stdout.splitlines(), "stderr": r.stderr.splitlines(),
                    "files": {k: list(v) for k, v in snap.items()}}
            want = json.loads(json.dumps(want).replace(str(fake), "{HELPER}").replace(sys.executable, "{PYTHON}")
                               .replace(str(SRC), "{HERE}").replace(base, "{PATH}"))
            want = one_binary(want)
            cases.append({"layout": layout, "zone": zone, "env": fenv, "tree": fixture, "want": sub(want, str(t))})
    return cases


def capture_policy() -> dict:
    out = {}

    class Grab(T.DifferentialPolicyTest):
        pass

    real_run = subprocess.run

    def fake_run(argv, *a, **k):
        if argv and argv[0] == "@capture":
            raise _Got(argv)
        return real_run(argv, *a, **k)

    class _Got(Exception):
        def __init__(self, argv):
            self.argv = argv

    # the test computes Python's answers, then calls the Rust helper: stop it there and keep both
    os.environ["APPZONES_TEST_HELPER"] = "@capture"
    import importlib as il
    T.subprocess.run = fake_run
    real_reload = il.reload

    def reload(m):
        m = real_reload(m)
        if m.__name__ == "zonecfg":
            root = Path(m.HOME).parent
            (root / "code").mkdir(exist_ok=True)
            m.CODE_DIR = root / "code"
            out["module"] = m
        return m

    T.importlib.reload = reload
    try:
        tc = Grab("test_same_answers")
        try:
            tc.test_same_answers()
        except _Got as g:
            args = g.argv
            i = args.index("--list-denied")
            paths = args[i + 1:]
            z = out["module"]
            root = str(Path(z.HOME).parent)
            # the environment the test had when it asked Python (it restored ours since)
            os.environ["APPZONES_DENIED_PATHS"] = args[args.index("--denied-file") + 1]
            os.environ.pop("XDG_STATE_HOME", None)
            want = [z.mount_source_error(p) for p in paths]
            denied = [str(d) for d in z.denied_paths()]
            out = sub({"args": args[1:i + 1], "paths": paths, "want": want, "denied": denied,
                       "tree": tree(Path(root))}, root)
    finally:
        T.subprocess.run = real_run
        T.importlib.reload = real_reload
    return out


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    os.environ["APPZONES_LAYOUT"] = "nixos"
    config = capture_config() + capture_examples()
    trees: dict = {}  # each tree once: most cases share the test's standard one
    for c in config:
        key = json.dumps(c["tree"], sort_keys=True)
        c["tree"] = trees.setdefault(key, len(trees))
    tree_list = [None] * len(trees)
    for k, i in trees.items():
        tree_list[i] = json.loads(k)
    (OUT / "config.json").write_text(json.dumps({"trees": tree_list, "cases": config}, separators=(",", ":")) + "\n")
    plan = capture_plan()
    (OUT / "plan.json").write_text(json.dumps(plan, indent=0) + "\n")
    policy = capture_policy()
    (OUT / "policy.json").write_text(json.dumps(policy, indent=0) + "\n")
    print(f"config {len(config)} ({len(trees)} trees), plan {len(plan)}, policy {len(policy.get('paths', []))}")
