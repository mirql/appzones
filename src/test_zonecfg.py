"""Tests for zonecfg and parts of zone.py. Run (in src/): python3 -B -m unittest test_zonecfg

Each test builds a fake home in a temp dir (APPZONES_HOME_FOR_TESTS), so nothing
depends on, or touches, the real home.
"""
import contextlib
import importlib
import io
import json
import os
import re
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
# the modules behind `appzones` (src/zone.py was split into these, #35), common first:
# the others read its settings at call time
ZONE_MODULES = ["common", "net", "fs", "portals", "agent", "server", "client", "mounts", "upgrade"]


class _Zone:
    """The appzones modules as one namespace: reading a name finds the module that
    defines it; setting one sets it there and wherever it was imported by name
    (so a test can replace a function or a setting where the code looks it up)."""

    def __init__(self, mods):
        object.__setattr__(self, "_mods", mods)

    def _owner(self, name):
        for m in self._mods:
            if name in vars(m):
                return m
        raise AttributeError(name)

    def __getattr__(self, name):
        return getattr(self._owner(name), name)

    def __setattr__(self, name, value):
        # also where another module imported it by name (`from .common import load_cfg`)
        old = getattr(self._owner(name), name)
        for m in self._mods:
            if name in vars(m) and vars(m)[name] is old:
                setattr(m, name, value)


class ZonecfgTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name) / "home"
        for d in ("Downloads/work", "Pictures", "Music", ".ssh", ".gnupg", ".config/opencode",
                  ".local/share/opencode", ".claude", "Docs"):
            (self.home / d).mkdir(parents=True)
        (self.home / "Docs/sshlink").symlink_to(self.home / ".ssh")
        (self.home / "dotlink").symlink_to(self.home)
        os.environ["APPZONES_HOME_FOR_TESTS"] = str(self.home)
        os.environ["APPZONES_DBUS_SERVICE_DIRS"] = ""  # no host .service files unless a test adds some
        # the host's own denied paths (/etc/appzones/denied-paths)
        (Path(self.tmp.name) / "denied-paths").write_text("# a host's additions\n/srv/code\n~/Secrets  # in the home\n\n")
        os.environ["APPZONES_DENIED_PATHS"] = str(Path(self.tmp.name) / "denied-paths")
        # a host with nix-ld: the shim (a link into its store path) and the loader's lib dir
        t = Path(self.tmp.name)
        (t / "store/abc-nix-ld-2.0/libexec").mkdir(parents=True)
        (t / "store/abc-nix-ld-2.0/libexec/nix-ld").write_text("")
        (t / "lib64").mkdir()
        (t / "lib64/ld-linux-x86-64.so.2").symlink_to(t / "store/abc-nix-ld-2.0/libexec/nix-ld")
        (t / "nix-ld/lib").mkdir(parents=True)
        (t / "nix-ld/lib/ld.so").write_text("")
        os.environ["APPZONES_NIX_LD_SHIM"] = str(t / "lib64/ld-linux-x86-64.so.2")
        os.environ["APPZONES_NIX_LD_LIB"] = str(t / "nix-ld/lib")
        sys.path.insert(0, str(HERE))
        import zonecfg
        self.z = importlib.reload(zonecfg)
        # the fake home sits in /tmp (TMPDIR): the home is exempt from the system-tree rule, as on a
        # host whose /home lives in /var

    def tearDown(self):
        self.tmp.cleanup()
        os.environ.pop("APPZONES_HOME_FOR_TESTS", None)
        os.environ.pop("APPZONES_DBUS_SERVICE_DIRS", None)
        os.environ.pop("APPZONES_DENIED_PATHS", None)
        os.environ.pop("APPZONES_NIX_LD_SHIM", None)
        os.environ.pop("APPZONES_NIX_LD_LIB", None)

    def cfg(self, text):
        p = Path(self.tmp.name) / "zones.toml"
        p.write_text(text)
        return self.z.load(p)

    def errs(self, zone_body, extra=""):
        return self.cfg(f'version = 1\nrequires = {self.z.RELEASE}\n[zones.tt]\ncolour = "#112233"\n'
                        f'{zone_body}\n{extra}').errors

    def assertRefused(self, errors, needle):
        self.assertTrue(any(needle in e for e in errors), f"{needle!r} not in {errors}")

    def test_example_is_valid(self):
        cfg = self.z.load(HERE.parent / "examples/zones.toml")
        self.assertEqual(cfg.errors, [])
        self.assertEqual(set(cfg.zones), {"daily", "work", "untrusted"})
        w = cfg.zones["work"]
        self.assertEqual((w.settings["netbird"], w.origin["netbird"]), (True, "zone"))
        self.assertEqual((w.settings["dns"], w.origin["dns"]), ("host", "defaults"))
        self.assertEqual(cfg.zones["untrusted"].settings["dns"], "public")

    def test_minimal_example_is_valid_anywhere(self):
        """examples/minimal.toml needs no binds, no nix-ld: valid on a fresh host."""
        (Path(self.tmp.name) / "lib64/ld-linux-x86-64.so.2").unlink()  # no nix-ld here
        cfg = self.z.load(HERE.parent / "examples/minimal.toml")
        self.assertEqual(cfg.errors, [])
        self.assertEqual(set(cfg.zones), {"daily", "untrusted"})

    def test_zone_new_creates_a_missing_config(self):
        zone = self._zone_mod()
        zone.CONFIG = Path(self.tmp.name) / "fresh/zones.toml"
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(zone.new("games", False), 0)  # a preview writes nothing
            self.assertFalse(zone.CONFIG.exists())
            self.assertEqual(zone.new("games", True), 0)
        cfg = self.z.load(zone.CONFIG)
        self.assertEqual(cfg.errors, [])
        self.assertEqual(list(cfg.zones), ["games"])

    def test_builtin_defaults(self):
        cfg = self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\n')
        s = cfg.zones["tt"].settings
        self.assertEqual(cfg.errors, [])
        self.assertEqual((s["dns"], s["links"], s["keyring"], s["netbird"]), ("host", "portal", "basic", False))

    def test_host_denied_paths(self):
        self.assertEqual(self.z.host_denied_paths(), [Path("/srv/code"), self.home / "Secrets"])
        (self.home / "Secrets/sub").mkdir(parents=True)
        self.assertRefused(self.errs('binds = [{ path = "~/Secrets/sub" }]'), "inside")
        self.assertRefused(self.errs('binds = [{ path = "/srv/code" }]'), "is /srv/code")

    def test_host_denied_paths_fail_closed(self):
        """A file that is there but unusable refuses every bind; only a missing default file means none."""
        (self.home / "Docs/ok").mkdir()
        f = Path(self.tmp.name) / "denied-paths"
        f.write_text("/srv/code\nrelative/path\n")  # a bad line
        self.assertRefused(self.errs('binds = [{ path = "~/Docs/ok" }]'), "must be absolute or start with ~/")
        self.assertIn("must be absolute", self.z.mount_source_error(str(self.home / "Docs/ok")))
        f.unlink()
        f.mkdir()  # unreadable as a file
        self.assertRefused(self.errs('binds = [{ path = "~/Docs/ok" }]'), "cannot read the host's denied paths")
        os.environ["APPZONES_DENIED_PATHS"] = str(Path(self.tmp.name) / "missing")  # an explicit override is required
        self.assertRefused(self.errs('binds = [{ path = "~/Docs/ok" }]'), "does not exist")
        os.environ.pop("APPZONES_DENIED_PATHS")
        orig = self.z.HOST_DENIED_FILE, self.z.LEGACY_HOST_DENIED_FILE
        try:
            self.z.HOST_DENIED_FILE = str(Path(self.tmp.name) / "no-such-default")  # missing default: no additions
            self.z.LEGACY_HOST_DENIED_FILE = str(Path(self.tmp.name) / "no-such-legacy")
            self.assertEqual(self.z.host_denied_paths(), [])
            self.assertEqual(self.errs('binds = [{ path = "~/Docs/ok" }]'), [])
            # the pre-0.6 file (/etc/zones/denied-paths) is read when the new one is not there
            legacy = Path(self.tmp.name) / "legacy-denied-paths"
            legacy.write_text("/srv/legacy\n")
            self.z.LEGACY_HOST_DENIED_FILE = str(legacy)
            self.assertEqual(self.z.host_denied_paths(), [Path("/srv/legacy")])
            new = Path(self.tmp.name) / "new-denied-paths"
            new.write_text("/srv/new\n")
            self.z.HOST_DENIED_FILE = str(new)
            self.assertEqual(self.z.host_denied_paths(), [Path("/srv/new")])
        finally:
            self.z.HOST_DENIED_FILE, self.z.LEGACY_HOST_DENIED_FILE = orig

    def test_env_names(self):
        """$APPZONES_* first; the pre-0.6 $ZONES_* still read; an empty value counts as set."""
        for k in ("APPZONES_T_X", "ZONES_T_X"):
            os.environ.pop(k, None)
        try:
            self.assertIsNone(self.z.env("T_X"))
            self.assertFalse(self.z.has_env("T_X"))
            os.environ["ZONES_T_X"] = "old"
            self.assertEqual(self.z.env("T_X"), "old")
            os.environ["APPZONES_T_X"] = ""
            self.assertEqual(self.z.env("T_X"), "")
            self.assertTrue(self.z.has_env("T_X"))
        finally:
            for k in ("APPZONES_T_X", "ZONES_T_X"):
                os.environ.pop(k, None)

    def test_config_path(self):
        """~/.config/appzones/zones.toml; the pre-0.6 file is used only when the new one is not there."""
        os.environ.pop("APPZONES_CONFIG", None)
        new, old = self.home / ".config/appzones/zones.toml", self.home / ".config/zones/zones.toml"
        self.assertEqual(self.z.config_path(self.home), (new, None))
        old.parent.mkdir(parents=True)
        old.write_text("")
        self.assertEqual(self.z.config_path(self.home), (old, old))
        new.parent.mkdir(parents=True)
        new.write_text("")
        self.assertEqual(self.z.config_path(self.home), (new, None))

    def test_mount_helper_clears_both_prefixes(self):
        """The root helper drops every APPZONES_* and ZONES_* variable before it imports zonecfg."""
        os.environ["APPZONES_T_HELPER"] = os.environ["ZONES_T_HELPER"] = "x"
        try:
            sys.path.insert(0, str(HERE))
            import zone_mount_helper
            importlib.reload(zone_mount_helper)
            self.assertNotIn("APPZONES_T_HELPER", os.environ)
            self.assertNotIn("ZONES_T_HELPER", os.environ)
        finally:
            for k in ("APPZONES_T_HELPER", "ZONES_T_HELPER"):
                os.environ.pop(k, None)
            # it also cleared this test's own APPZONES_* settings; tearDown and the next setUp redo them

    def test_mount_helper_state_dir(self):
        """The helper's mnt dir: ~/.local/share/appzones; the pre-0.6 ~/.local/share/zones only while the
        new one does not exist (as zone.py chooses). Refusing a symlink there is the VM test's job."""
        sys.path.insert(0, str(HERE))
        import zone_mount_helper as h
        c = h.Caller.__new__(h.Caller)  # __init__ needs root; only the home matters here
        c.home = str(self.home)
        new, old = self.home / ".local/share/appzones", self.home / ".local/share/zones"
        self.assertEqual(c.mnt_dir("ta"), f"{new}/ta/mnt")
        old.mkdir(parents=True)
        self.assertEqual(c.mnt_dir("ta"), f"{old}/ta/mnt")
        new.symlink_to(old)  # a symlink counts as there: chosen, and then refused by open_owned
        self.assertEqual(c.mnt_dir("ta"), f"{new}/ta/mnt")
        new.unlink()
        new.mkdir()
        self.assertEqual(c.mnt_dir("ta"), f"{new}/ta/mnt")
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            c.mnt_dir("../x")

    @unittest.skipUnless((HERE.parent / "docs/index.md").exists(), "no docs in this tree (package build)")
    def test_llms_txt_lists_every_docs_page(self):
        """docs/_data/llms.yml (llms.txt, llms-full.txt) names every guide page, and only pages that exist."""
        docs = HERE.parent / "docs"
        catalog = docs / "_data/llms.yml"
        self.assertTrue(catalog.exists(), "docs/_data/llms.yml is missing")
        pages = {str(p.relative_to(docs)) for p in docs.rglob("*.md")} - {"index.md", "guide.md"}
        listed = set()
        for line in catalog.read_text().splitlines():
            m = re.match(r"^    - file:\s*([^#]+?)\s*(#.*)?$", line)  # a page entry, at its own indentation
            if m:
                listed.add(m.group(1).strip("\"'"))
        self.assertEqual(sorted(pages - listed), [], "docs pages missing from docs/_data/llms.yml")
        self.assertEqual(sorted(listed - pages), [], "docs/_data/llms.yml names pages that do not exist")

    def test_hyprland_zones_lua_is_a_copy(self):
        """contrib/hyprland/zones.lua (pre-0.6 name) is appzones.lua's code, past each file's header comment."""
        def code(name):
            lines = (HERE.parent / "contrib/hyprland" / name).read_text().splitlines()
            while lines and lines[0].startswith("--"):
                lines.pop(0)
            return lines
        self.assertEqual(code("zones.lua"), code("appzones.lua"))

    def test_state_moves_once_nothing_runs(self):
        """The pre-0.6 ~/.local/share/zones is used until it can be moved: no supervisor, nothing mounted."""
        legacy = self.home / ".local/share/zones"
        (legacy / "ta/home").mkdir(parents=True)
        (legacy / "ta/home/kept").write_text("x")
        real_home = os.environ.get("HOME")
        os.environ["HOME"] = str(self.home)
        os.environ.pop("APPZONES_STATE", None)
        try:
            zone = self._zone_mod()
            self.assertEqual(zone.STATE, legacy)
            zone.supervisors = lambda: ["python3 /nix/store/x/libexec/appzones/zone.py _supervise ta"]
            with contextlib.redirect_stderr(io.StringIO()) as err:
                zone.migrate_state()
            self.assertEqual(zone.STATE, legacy)
            self.assertIn("once no zone runs", err.getvalue())
            zone.supervisors = lambda: []
            zone.mounted_below = lambda p: False
            with contextlib.redirect_stderr(io.StringIO()) as err:
                zone.migrate_state()
            new = self.home / ".local/share/appzones"
            self.assertEqual((zone.STATE, zone.LAUNCHER), (new, new / "launcher/share/applications"))
            self.assertEqual((new / "ta/home/kept").read_text(), "x")
            self.assertFalse(legacy.exists())
            self.assertIn("moved", err.getvalue())
        finally:
            if real_home is None:
                os.environ.pop("HOME", None)
            else:
                os.environ["HOME"] = real_home
            self._zone_mod()

    def _zone_mod(self):
        """The appzones modules, freshly loaded (they read the environment at import), as one namespace."""
        sys.path.insert(0, str(HERE))
        import appzones
        mods = []
        for name in ZONE_MODULES:
            m = importlib.import_module(f"appzones.{name}")
            mods.append(importlib.reload(m))
        return _Zone(mods)

    def test_nft_rules(self):
        """Every host address refused (never the host itself), NetBird networks allowed only with netbird,
        every connected network refused: a LAN with global IPv6 or public IPv4 addresses too."""
        import ipaddress as ip
        zone = self._zone_mod()
        addrs = [("lo", ip.ip_interface("127.0.0.1/8")), ("lo", ip.ip_interface("::1/128")),
                 ("eth0", ip.ip_interface("203.0.113.7/24")),             # a LAN with public IPv4
                 ("eth0", ip.ip_interface("2001:db8:1::7/64")),            # and global IPv6
                 ("eth0", ip.ip_interface("fe80::1/64")),
                 ("wt0", ip.ip_interface("100.70.12.34/16")), ("wt0", ip.ip_interface("100.70.99.1/16"))]
        zone.host_addrs = lambda: addrs
        zone.NETBIRD_IFACE = "wt0"
        plain = zone.nft_rules(self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\n').zones["tt"])
        nb = zone.nft_rules(self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\nnetbird = true\n').zones["tt"])
        for rules in (plain, nb):
            self.assertIn("ip daddr { 100.70.12.34, 100.70.99.1, 203.0.113.7 } reject", rules)
            self.assertIn("ip6 daddr { 2001:db8:1::7, fe80::1 } reject", rules)
            self.assertIn("ip6 daddr { 2001:db8:1::/64 } reject", rules)
            self.assertNotIn("127.0.0.1,", rules)
        self.assertIn("ip daddr { 100.70.0.0/16, 203.0.113.0/24 } reject", plain)
        self.assertNotIn("} accept", plain)
        # netbird: both host addresses refused first, then the mesh accepted, then the LAN refused
        self.assertIn("ip daddr { 100.70.0.0/16 } accept", nb)
        self.assertIn("ip daddr { 203.0.113.0/24 } reject", nb)
        self.assertLess(nb.index("100.70.99.1, 203.0.113.7 } reject"), nb.index("100.70.0.0/16 } accept"))
        self.assertLess(nb.index("100.70.0.0/16 } accept"), nb.index("203.0.113.0/24 } reject"))
        # no NetBird interface: nothing accepted
        zone.host_addrs = lambda: [a for a in addrs if a[0] != "wt0"]
        self.assertNotIn("} accept", zone.nft_rules(
            self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\nnetbird = true\n').zones["tt"]))

    def test_host_resolver(self):
        zone = self._zone_mod()
        rc = Path(self.tmp.name) / "resolv.conf"
        rc.write_text("# generated\nsearch lan\nnameserver 127.0.0.1\nnameserver 192.0.2.53\n")
        os.environ.pop("APPZONES_DNS_HOST", None)
        self.assertEqual(zone.host_resolver(str(rc)), "127.0.0.1")
        rc.write_text("nameserver fe80::1%eth0\n")
        self.assertEqual(zone.host_resolver(str(rc)), "fe80::1%eth0")
        rc.write_text("nameserver not-an-ip\n")
        self.assertEqual(zone.host_resolver(str(rc)), "127.0.0.53")
        self.assertEqual(zone.host_resolver(str(Path(self.tmp.name) / "none")), "127.0.0.53")
        os.environ["APPZONES_DNS_HOST"] = "192.0.2.1"
        try:
            self.assertEqual(zone.host_resolver(str(rc)), "192.0.2.1")
        finally:
            os.environ.pop("APPZONES_DNS_HOST")

    def test_readline_keeps_the_rest(self):
        """Two replies in one read: the second is not lost (appzones run --wait's exit code)."""
        import socket
        zone = self._zone_mod()
        a, b = socket.socketpair()
        with a, b:
            a.sendall(b'{"ok": true, "pid": 7}\n{"exit": 0}\n')
            a.shutdown(socket.SHUT_WR)
            first, rest = zone.readline(b)
            self.assertEqual(first, {"ok": True, "pid": 7})
            self.assertEqual(zone.readline(b, rest)[0], {"exit": 0})

    def test_write_colours_concurrent(self):
        import threading
        zone = self._zone_mod()
        zone.RT = Path(self.tmp.name) / "rt"
        cfg = self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\n')
        errs = []
        def w():
            try:
                for _ in range(50):
                    zone.write_colours(cfg)
            except Exception as e:  # noqa: BLE001
                errs.append(e)
        ts = [threading.Thread(target=w) for _ in range(8)]
        [t.start() for t in ts]
        [t.join() for t in ts]
        self.assertEqual(errs, [])
        self.assertEqual((zone.RT / "colours").read_text(), "tt #112233\n")
        self.assertEqual([p.name for p in zone.RT.iterdir()], ["colours"])

    def test_theme_copy_refuses_never_list_links(self):
        """A link planted in a theme directory never copies a never-list file into a zone."""
        zone = self._zone_mod()
        zone.HOME = self.home
        (self.home / ".ssh/id_test").write_text("SECRET")
        icons = self.home / ".icons/theme"
        icons.mkdir(parents=True)
        (icons / "index.theme").write_text("[Icon Theme]\n")
        (icons / "evil.png").symlink_to(self.home / ".ssh/id_test")
        (icons / "evil-dir").symlink_to(self.home / ".ssh")
        (self.home / ".gtkrc-2.0").symlink_to(self.home / ".ssh/id_test")
        zdir = Path(self.tmp.name) / "zone-state"
        with contextlib.redirect_stderr(io.StringIO()):
            pairs = zone.copy_theme(zdir)
        copied = [p for p in (zdir / "theme").rglob("*") if p.is_file()]
        self.assertEqual([p.name for p in copied], ["index.theme"])
        self.assertFalse(any("SECRET" in p.read_text() for p in copied))
        self.assertEqual([d for _, d in pairs], [str(self.home / ".icons")])

    def test_theme_budgets(self):
        """Over the byte budget (bytes actually read) or the entry budget (every entry visited): skipped whole."""
        zone = self._zone_mod()
        zone.HOME = self.home
        (self.home / ".icons/big").mkdir(parents=True)
        (self.home / ".icons/big/blob").write_bytes(b"x" * 4096)
        (self.home / ".local/share/fonts").mkdir(parents=True)
        for i in range(30):
            (self.home / f".local/share/fonts/empty{i}").mkdir()
        (self.home / ".gtkrc-2.0").write_text("gtk-theme-name = \"Adwaita\"\n")
        zone.THEME_MAX, zone.THEME_MAX_FILES = 1024, 20
        zdir = Path(self.tmp.name) / "zone-state"
        with contextlib.redirect_stderr(io.StringIO()) as err:
            pairs = zone.copy_theme(zdir)
        self.assertEqual([d for _, d in pairs], [str(self.home / ".gtkrc-2.0")])
        self.assertIn("skipping ~/.icons", err.getvalue())
        self.assertIn("skipping ~/.local/share/fonts", err.getvalue())
        self.assertFalse((zdir / "theme/.icons").exists())

    def test_resolv_conf(self):
        zone = self._zone_mod()
        os.environ.pop("APPZONES_DNS_HOST", None)
        self.assertEqual(zone.resolv_conf({"network": "internet", "dns": "public"}), f"nameserver {zone.DNS_ADDR}\n")
        self.assertEqual(zone.resolv_conf({"network": "host", "dns": "public"}), "nameserver 1.1.1.1\n")
        self.assertEqual(zone.resolv_conf({"network": "none", "dns": "host"}), "")
        os.environ["APPZONES_DNS_HOST"] = "192.0.2.53"
        try:
            self.assertEqual(zone.resolv_conf({"network": "host", "dns": "host"}), "nameserver 192.0.2.53\n")
        finally:
            os.environ.pop("APPZONES_DNS_HOST")

    def test_zone_new_concurrent(self):
        """Two `appzones new -y` at once: both zones end up in the file, which is never left half-written."""
        import subprocess
        cfgp = Path(self.tmp.name) / "conc/zones.toml"
        env = dict(os.environ, APPZONES_CONFIG=str(cfgp), HOME=str(self.home))
        procs = [subprocess.Popen([sys.executable, "-B", str(HERE / "zone.py"), "new", n, "-y"], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE) for n in ("one", "two", "three")]
        for pr in procs:
            _, e = pr.communicate(timeout=30)
            self.assertEqual(pr.returncode, 0, e)
        cfg = self.z.load(cfgp)
        self.assertEqual(cfg.errors, [])
        self.assertEqual(set(cfg.zones), {"one", "two", "three"})
        self.assertEqual(len({z.settings["colour"] for z in cfg.zones.values()}), 3)

    def test_unlinked_descriptor_never_passes(self):
        """A file replaced after it was opened: its descriptor path ends in " (deleted)" and is refused,
        so a denied single file (~/.gitconfig) cannot slip through theme copying or bind pinning."""
        zone = self._zone_mod()
        zone.HOME = self.home
        secret = self.home / ".gitconfig"
        secret.write_text("SECRET")
        fd = os.open(secret, os.O_RDONLY)
        try:
            self.assertIsNotNone(self.z.fd_path(fd))
            tmp = self.home / ".gitconfig.new"
            tmp.write_text("new")
            os.replace(tmp, secret)  # an atomic save: the old inode is unlinked
            self.assertIsNone(self.z.fd_path(fd))
            self.assertFalse(zone._theme_target_ok(fd))
        finally:
            os.close(fd)
        # pin_binds: an O_PATH descriptor of a directory removed after it was opened
        d = self.home / "Docs/gone"
        d.mkdir()
        pfd = os.open(d, os.O_PATH)
        d.rmdir()
        try:
            self.assertIsNone(self.z.fd_path(pfd))
        finally:
            os.close(pfd)

    def test_theme_entry_budget_counts_files_once(self):
        zone = self._zone_mod()
        zone.HOME = self.home
        (self.home / ".icons/t").mkdir(parents=True)
        for i in range(9):
            (self.home / f".icons/t/f{i}").write_text("x")
        zone.THEME_MAX_FILES = 10  # the "t" entry + 9 files: exactly the budget
        zdir = Path(self.tmp.name) / "zone-state"
        with contextlib.redirect_stderr(io.StringIO()) as err:
            pairs = zone.copy_theme(zdir)
        self.assertEqual([d for _, d in pairs], [str(self.home / ".icons")], err.getvalue())
        self.assertEqual(len(list((zdir / "theme/.icons/t").iterdir())), 9)

    def test_dbus_service_dir_override_fails_closed(self):
        for bad in ("missing", "a-file"):
            with self.subTest(bad=bad):
                p = Path(self.tmp.name) / bad
                if bad == "a-file":
                    p.write_text("")
                os.environ["APPZONES_DBUS_SERVICE_DIRS"] = str(p)
                try:
                    self.z = importlib.reload(self.z)
                    self.assertRefused(self.errs('dbus = { talk = ["com.example.Other"] }'), "cannot ask the session bus")
                finally:
                    os.environ["APPZONES_DBUS_SERVICE_DIRS"] = ""

    def test_stdio_fd_ok(self):
        zone = self._zone_mod()
        r, w = os.pipe()
        f = os.open(Path(self.tmp.name) / "f", os.O_RDWR | os.O_CREAT)
        d = os.open(self.tmp.name, os.O_RDONLY)
        null = os.open("/dev/null", os.O_RDONLY)
        import socket
        a, b = socket.socketpair()
        try:
            self.assertTrue(all(zone.stdio_fd_ok(x) for x in (r, w, f, null)))
            self.assertFalse(zone.stdio_fd_ok(d))
            self.assertFalse(zone.stdio_fd_ok(a.fileno()))
        finally:
            for x in (r, w, f, d, null):
                os.close(x)
            a.close(); b.close()

    def test_pin_binds(self):
        """The supervisor's last check: on the descriptor it hands to bwrap."""
        zone = self._zone_mod()
        (self.home / "Docs/proj").mkdir()
        out = zone.pin_binds(["bwrap", zone.PIN, "ro", "~/Docs/proj", "/home/x/Docs/proj", "--", "true"])
        self.assertEqual(out[0], "bwrap")
        self.assertEqual(out[1], "--ro-bind-fd")
        self.assertEqual(os.readlink(f"/proc/self/fd/{out[2]}"), str(self.home / "Docs/proj"))
        self.assertEqual(out[3:], ["/home/x/Docs/proj", "--", "true"])
        os.close(int(out[2]))
        # a symlink swapped in after validation: refused on what was opened
        (self.home / "Docs/proj").rmdir()
        (self.home / "Docs/proj").symlink_to(self.home / ".ssh")
        with contextlib.redirect_stderr(io.StringIO()) as err, self.assertRaises(SystemExit):
            zone.pin_binds(["bwrap", zone.PIN, "rw", "~/Docs/proj", "/x"])
        self.assertIn(".ssh", err.getvalue())

    def test_home_checked_before_it_exists(self):
        for home, why in [("~/.config/autostart", "is"), ("~/.config/autostart/zone", "is inside"),
                          ("~/.local/share/zones/other/home", "is inside"), ("/run/user/1000/x", "is inside"),
                          ("/var/lib/thing", "is inside /var"), ("/srv/code/new", "is inside /srv/code")]:
            with self.subTest(home=home):
                self.assertRefused(self.errs(f'home = "{home}"'), why)
        # a symlinked parent: checked where the new directory would land
        (self.home / "Docs/sshlink2").symlink_to(self.home / ".ssh")
        self.assertRefused(self.errs('home = "~/Docs/sshlink2/newhome"'), "resolves to")
        self.assertEqual(self.errs('home = "~/Docs/zonehome"'), [])  # created at start
        (self.home / "Docs/file").write_text("")
        self.assertRefused(self.errs('home = "~/Docs/file"'), "not a directory")

    def test_binds_never_system_trees_or_sockets(self):
        import socket
        for path, why in [("/run/dbus/system_bus_socket", "is inside /run"), ("/tmp/.X11-unix", "is inside /tmp"),
                          ("/proc/cpuinfo", "is inside /proc"), ("/dev/shm", "is inside /dev"),
                          ("/var/lib/foo", "is inside /var"), ("/etc/passwd", "is inside /etc"), ("/var", "whole")]:
            with self.subTest(path=path):
                self.assertRefused(self.errs(f'binds = [{{ path = "{path}", mode = "ro" }}]'), why)
        sock = self.home / "Docs/s.sock"
        with socket.socket(socket.AF_UNIX) as srv:
            srv.bind(str(sock))
            self.assertRefused(self.errs('binds = [{ path = "~/Docs/s.sock", mode = "ro" }]'), "is a socket")
        os.mkfifo(self.home / "Docs/fifo")
        self.assertRefused(self.errs('binds = [{ path = "~/Docs/fifo" }]'), "is a FIFO")
        (self.home / "Docs/note.txt").write_text("")
        self.assertEqual(self.errs('binds = [{ path = "~/Docs/note.txt", mode = "ro" }]'), [])

    def test_malformed_config_is_an_error_not_a_crash(self):
        for body, why in [('dbus = { talk = ["org.example.Foo"] }', None),  # a partial table is fine
                          ('dbus = { own = ["org.example.player.app"] }', None),
                          ("links = 42", "links must be"),
                          ("binds = [{ path = 42 }]", "path must be a non-empty string"),
                          ('binds = [{ path = "" }]', "path must be a non-empty string"),
                          ("home = 5", "home must be a string")]:
            with self.subTest(body=body):
                e = self.errs(body)
                if why is None:
                    self.assertEqual(e, [])
                else:
                    self.assertRefused(e, why)
        for top, why in [('version = 1\nzones = 3\n', "zones must be a table"),
                         ('version = 1\ndefaults = "x"\n', "defaults must be a table"),
                         ('version = 1\napps = []\n', "apps must be a table")]:
            with self.subTest(top=top):
                self.assertRefused(self.cfg(top).errors, why)

    def test_dbus_own_overlap_and_zone_names(self):
        cfg = self.cfg('version = 1\n[zones.aa]\ncolour = "#112233"\ndbus = { own = ["org.example.player.*"] }\n'
                       '[zones.bb]\ncolour = "#223344"\ndbus = { own = ["org.example.player.app"] }\n')
        self.assertRefused(cfg.errors, "in zones aa, bb")
        cfg = self.cfg('version = 1\n[zones.aa]\ncolour = "#112233"\ndbus = { own = ["org.example.one.*"] }\n'
                       '[zones.bb]\ncolour = "#223344"\ndbus = { own = ["org.example.two.*"] }\n')
        self.assertEqual(cfg.errors, [])
        for name in ["dev.mirql.zone.other.*", "org.mozilla.firefox_zone_other.*", "org.mozilla.foo_zone_x.y"]:
            with self.subTest(name=name):
                self.assertRefused(self.errs(f'dbus = {{ own = ["{name}"] }}'), "never granted")

    def test_dbus_activatable_names_from_the_bus(self):
        """The running bus's ListActivatableNames is the truth; no answer: extra grants refused."""
        os.environ.pop("APPZONES_DBUS_SERVICE_DIRS")
        try:
            self.z = importlib.reload(self.z)
            self.z.bus_activatable_names = lambda: {"com.example.OnlyOnBus"}
            self.assertRefused(self.errs('dbus = { talk = ["com.example.OnlyOnBus"] }'), "started on the host bus")
            self.assertRefused(self.errs('dbus = { own = ["com.example.OnlyOnBus.*"] }'), "started on the host bus")
            self.assertEqual(self.errs('dbus = { talk = ["com.example.Other"] }'), [])
            self.z = importlib.reload(self.z)
            self.z.bus_activatable_names = lambda: None  # no session bus, no busctl
            self.assertRefused(self.errs('dbus = { talk = ["com.example.Other"] }'), "cannot ask the session bus")
            self.assertEqual(self.errs(""), [])  # nothing extra asked: fine
        finally:
            os.environ["APPZONES_DBUS_SERVICE_DIRS"] = ""

    def test_dbus_unreadable_service_dir_fails_closed(self):
        d = Path(self.tmp.name) / "svc"
        d.mkdir()
        (d / "x.service").mkdir()  # unreadable as a file
        os.environ["APPZONES_DBUS_SERVICE_DIRS"] = str(d)
        try:
            self.z = importlib.reload(self.z)
            self.assertRefused(self.errs('dbus = { talk = ["com.example.Other"] }'), "cannot ask the session bus")
        finally:
            os.environ["APPZONES_DBUS_SERVICE_DIRS"] = ""

    def test_fuzz_wrong_types_are_errors_not_crashes(self):
        """Every key, every wrong type, in a zone, in [defaults] and in an app: errors, never a traceback."""
        vals = ['42', '"x"', '[]', '[[]]', '[42]', '{}', '{ a = 1 }', 'true', '1.5', '[{}]', '[{ path = 42 }]']
        texts = []
        for k in self.z.KEYS:
            for v in vals:
                texts.append(f'version = 1\n[zones.tt]\ncolour = "#112233"\n{k} = {v}\n')
                texts.append(f'version = 1\n[defaults]\n{k} = {v}\n[zones.tt]\ncolour = "#112233"\n')
        for k in ("command", "zones", "args", "bus", "desktop"):
            for v in vals:
                rest = "".join(f'{x} = {d}\n' for x, d in (("command", '["x"]'), ("zones", '["tt"]')) if x != k)
                texts.append(f'version = 1\n[zones.tt]\ncolour = "#112233"\nautostart_apps = ["aa"]\n'
                             f'[apps.aa]\n{rest}{k} = {v}\n')
        for v in vals:
            texts += [f'version = 1\n[zones.tt]\ncolour = "#112233"\nautostart_apps = ["aa"]\n[apps]\naa = {v}\n',
                      f'version = 1\n[zones]\ntt = {v}\n', f'version = 1\nrequires = {v}\n', f'version = {v}\n']
        for t in texts:
            with self.subTest(toml=t):
                cfg = self.cfg(t)  # must not raise
                self.assertIsInstance(cfg.errors, list)

    def test_unknown_keys(self):
        self.assertRefused(self.errs('netwrok = "none"'), "unknown key 'netwrok' (did you mean 'network'?)")
        self.assertRefused(self.cfg('version = 1\nzone = {}\n').errors, "top level: unknown key 'zone'")

    def test_requires_newer_release(self):
        newer = self.z.RELEASE + 1
        e = self.cfg(f'version = 1\nrequires = {newer}\n[defaults]\nsandwich = true\n'
                     '[zones.tt]\ncolour = "#112233"\n').errors
        self.assertTrue(e[0].endswith(f"is release {self.z.RELEASE}: update appzones (and rebuild) to get the "
                                      f"newer one. Until then, what this zone does not know:"), e)
        self.assertIn("unknown key 'sandwich'", e[1])
        # a typo in a file this release can read stays a plain error
        e = self.cfg(f'version = 1\nrequires = {self.z.RELEASE}\n[defaults]\nsandwich = true\n').errors
        self.assertRefused(e, "[defaults] unknown key 'sandwich'")
        self.assertFalse(any("update appzones" in x for x in e), e)
        self.assertRefused(self.cfg('version = 1\nrequires = "two"\n').errors, "requires must be")

    def test_requires_matches_what_the_file_uses(self):
        # the file must name the release its settings need, so an older tool can say "update"
        self.assertRefused(self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\nbus = "zone"\n').errors,
                           "set requires = 2")
        self.assertRefused(self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\n[apps.xx]\ncommand = ["x"]\n'
                                    'zones = ["tt"]\nbus = "zone"\n').errors, "set requires = 2")
        self.assertEqual(self.cfg('version = 1\nrequires = 2\n[zones.tt]\ncolour = "#112233"\nbus = "zone"\n').errors, [])

    def test_enums(self):
        self.assertRefused(self.errs('dns = "8.8.8.8"'), "dns must be one of")
        self.assertRefused(self.errs('links = "zone:nosuch"'), "unknown zone 'nosuch'")
        self.assertRefused(self.errs('links = "elsewhere"'), "links must be")
        self.assertRefused(self.errs('keyring = "kwallet"'), "keyring must be one of")
        self.assertRefused(self.errs('devices = ["gpu", "keyboard"]'), "unknown device")
        self.assertRefused(self.errs('network = "none"\nnetbird = true'), "netbird = true needs")

    def test_names_and_colours(self):
        self.assertRefused(self.cfg('version = 1\n[zones.Bad_Name]\ncolour = "#112233"\n').errors, "must match")
        e = self.cfg('version = 1\n[zones.aa]\ncolour = "#112233"\n[zones.bb]\ncolour = "#112233"\n').errors
        self.assertRefused(e, "already used")
        self.assertRefused(self.cfg('version = 1\n[zones.aa]\n').errors, "colour is required")

    def test_untrusted_host_network(self):
        base = 'version = 1\n[zones.untrusted]\ncolour = "#112233"\nnetwork = "host"\n'
        self.assertRefused(self.cfg(base).errors, "needs force_host")
        self.assertEqual(self.cfg(base + "force_host = true\n").errors, [])

    def test_binds_never_list(self):
        for path, why in [("~/.ssh", "is"), ("~", "never allowed"), ("/", "never allowed"),
                          ("/home", "never allowed"), ("~/.config", "contains"),
                          ("~/.config/opencode", "is"), ("~/.local/share", "contains"),
                          ("~/.local/share/opencode", "is"), ("~/.claude", "is"),
                          ("~/Docs/sshlink", "resolves to"), ("~/dotlink", "never allowed"),
                          ("/srv/code", "is"), ("/run/user", "is /run/user"), ("/run/user/1000/bus", "is inside"), ("~/nonexistent", "does not exist")]:
            with self.subTest(path=path):
                self.assertRefused(self.errs(f'binds = [{{ path = "{path}" }}]'), why)
        self.assertEqual(self.errs('binds = [{ path = "~/Downloads", mode = "rw" }, { path = "~/Pictures", mode = "ro" }]'), [])
        self.assertRefused(self.errs('binds = [{ path = "~/Pictures", mode = "rx" }]'), "mode must be")

    def test_dbus_never_list(self):
        for name in ["org.freedesktop.systemd1", "org.freedesktop.*", "*", "org.*", "org.kde.kwalletd6",
                     "org.freedesktop.secrets", "org.freedesktop.Flatpak", "org.freedesktop.impl.portal.FileChooser"]:
            with self.subTest(name=name):
                self.assertRefused(self.errs(f'dbus = {{ talk = ["{name}"] }}'), "never granted")
        self.assertEqual(self.errs('dbus = { own = ["org.example.player.*"], talk = ["org.kde.StatusNotifierWatcher"] }'), [])

    def test_dbus_own_on_host_bus(self):
        # notifications, portals, session names; and wildcards above an app's prefix
        for name in ["org.freedesktop.Notifications", "org.freedesktop.portal.Desktop", "org.kde.StatusNotifierWatcher",
                     "org.gnome.SessionManager", "org.xfce.Thunar", "org.freedesktop.FileManager1"]:
            with self.subTest(name=name):
                self.assertRefused(self.errs(f'dbus = {{ own = ["{name}"] }}'), "never granted")
        self.assertRefused(self.errs('dbus = { own = ["org.mozilla.*"] }'), "three labels")
        # the host's Firefox and every zone's share org.mozilla.firefox.*; zones get their own name
        for name in ["org.mozilla.firefox.*", "org.mozilla.firefox.ZGVmYXVsdC1yZWxlYXNl", "org.mozilla.firefox_zone_x.*"]:
            with self.subTest(name=name):
                self.assertRefused(self.errs(f'dbus = {{ own = ["{name}"] }}'), "never granted")
        # host daemons that act with host rights: never talked to
        for name in ["org.gtk.vfs.Daemon", "org.freedesktop.thumbnails.Thumbnailer1", "org.xfce.Xfconf",
                     "org.xfce.FileManager"]:
            with self.subTest(name=name):
                self.assertRefused(self.errs(f'dbus = {{ talk = ["{name}"] }}'), "never granted")

    def test_dbus_host_activatable_refused(self):
        svc = Path(self.tmp.name) / "services"
        svc.mkdir()
        (svc / "x.service").write_text("[D-BUS Service]\nName=com.example.Viewer\nExec=/bin/viewer\n")
        os.environ["APPZONES_DBUS_SERVICE_DIRS"] = str(svc)
        try:
            self.z = importlib.reload(self.z)
            self.assertRefused(self.errs('dbus = { own = ["com.example.Viewer"] }'), "started on the host bus")
            self.assertRefused(self.errs('dbus = { own = ["com.example.Viewer.*"] }'), "started on the host bus")
            self.assertRefused(self.errs('dbus = { talk = ["com.example.Viewer"] }'), "bus = \"zone\"")
            self.assertEqual(self.errs('dbus = { own = ["com.example.Other"] }'), [])
        finally:
            os.environ["APPZONES_DBUS_SERVICE_DIRS"] = ""

    def test_dbus_own_in_two_zones_refused(self):
        cfg = self.cfg('version = 1\n[zones.aa]\ncolour = "#112233"\ndbus = { own = ["org.example.player.*"] }\n'
                       '[zones.bb]\ncolour = "#223344"\ndbus = { own = ["org.example.player.*"] }\n')
        self.assertRefused(cfg.errors, "in zones aa, bb")

    def test_bus(self):
        extra = ('[apps.thunar]\ncommand = ["thunar"]\nzones = ["tt"]\nbus = "zone"\n'
                 '[apps.ff]\ncommand = ["/x/bin/firefox"]\nzones = ["tt"]\n')
        cfg = self.cfg(f'version = 1\nrequires = 2\n[zones.tt]\ncolour = "#112233"\n{extra}')
        self.assertEqual(cfg.errors, [])
        self.assertEqual(self.z.app_bus(cfg, "tt", "thunar"), "zone")
        self.assertEqual(self.z.app_bus(cfg, "tt", "ff"), "host")
        self.assertEqual(self.z.app_bus(cfg, "tt", None), "host")
        self.assertEqual(self.z.app_for_command(cfg, "/run/current-system/sw/bin/thunar"), "thunar")
        self.assertEqual(self.z.app_for_command(cfg, "firefox"), "ff")
        self.assertIsNone(self.z.app_for_command(cfg, "mousepad"))
        self.assertRefused(self.errs('bus = "session"'), "bus must be one of")
        self.assertRefused(self.errs("", '[apps.x]\ncommand = ["x"]\nzones = ["tt"]\nbus = "private"\n'),
                           "bus must be one of")
        self.assertRefused(self.errs('userns = false', '[apps.x]\ncommand = ["x"]\nzones = ["tt"]\nbus = "none"\n'),
                           "needs userns = true")
        self.assertRefused(self.errs('userns = false\nbus = "none"'), "needs userns = true")
        cfg = self.cfg('version = 1\nrequires = 2\n[zones.tt]\ncolour = "#112233"\nbus = "zone"\n')
        self.assertEqual(self.z.app_bus(cfg, "tt", None), "zone")

    def test_nix_ld(self):
        self.assertEqual(self.errs("nix_ld = true"), [])
        self.assertEqual(self.z.host_nix_ld()[1], os.environ["APPZONES_NIX_LD_LIB"])
        self.assertRefused(self.errs('nix_ld = "yes"'), "nix_ld must be true or false")
        # release 3: an older `appzones` must say "update appzones", not "unknown key"
        self.assertRefused(self.cfg('version = 1\nrequires = 2\n[zones.tt]\ncolour = "#112233"\nnix_ld = true\n')
                           .errors, "set requires = 3")
        # a host without nix-ld: refused, saying what to enable
        (Path(self.tmp.name) / "lib64/ld-linux-x86-64.so.2").unlink()
        (Path(self.tmp.name) / "lib64/ld-linux-x86-64.so.2").symlink_to("/nonexistent/glibc/ld-linux-x86-64.so.2")
        self.assertIsNone(self.z.host_nix_ld())
        self.assertRefused(self.errs("nix_ld = true"), "programs.nix-ld.enable")
        self.assertEqual(self.errs("nix_ld = false"), [])

    def test_tmp(self):
        self.assertEqual(self.errs('tmp = "disk"'), [])
        self.assertEqual(self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\n').zones["tt"].settings["tmp"], "ram")
        self.assertRefused(self.errs('tmp = "ssd"'), "tmp must be one of 'ram', 'disk'")
        self.assertRefused(self.cfg('version = 1\nrequires = 2\n[zones.tt]\ncolour = "#112233"\ntmp = "disk"\n')
                           .errors, "set requires = 3")

    def test_usb(self):
        self.assertEqual(self.errs('devices = ["usb"]\nusb = ["abcd", "abcd:1234"]'), [])
        s = self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\n').zones["tt"].settings
        self.assertEqual(s["usb"], [])
        for bad in ('"ABCD"', '"abcd:"', '"abc"', '"abcd:1234:00"', '"*"', '"0xabcd"', '1'):
            with self.subTest(usb=bad):
                self.assertRefused(self.errs(f'devices = ["usb"]\nusb = [{bad}]'), "usb ")
        self.assertRefused(self.errs('usb = ["abcd"]'), "usb is set but \"usb\" is not in devices")
        self.assertRefused(self.errs('devices = ["usb"]'), "usb is empty")
        # in [defaults] only: a zone without the device does not get it, and is not refused
        cfg = self.cfg(f'version = 1\nrequires = {self.z.RELEASE}\n[defaults]\nusb = ["abcd"]\n'
                       '[zones.tt]\ncolour = "#112233"\n')
        self.assertEqual(cfg.errors, [])
        # release 4: an older `appzones` must say "update appzones", not "unknown key"
        self.assertRefused(self.cfg('version = 1\nrequires = 3\n[zones.tt]\ncolour = "#112233"\n'
                                    'devices = ["usb"]\nusb = ["abcd"]\n').errors, "set requires = 4")

    def test_usb_binds(self):
        """zone.usb_binds on a fake sysfs: only the matching device, its hidraw, never the others."""
        root = Path(self.tmp.name) / "root"
        devs = root / "sys/devices/pci0/usb5"

        def usbdev(name, vid, pid, bus, dev, hid=None):
            d = devs / name
            d.mkdir(parents=True)
            # no "dev" (MAJ:MIN): the fake nodes are plain files, which the rdev check would refuse
            for k, v in (("idVendor", vid), ("idProduct", pid), ("busnum", bus), ("devnum", dev),
                         ("product", name)):
                (d / k).write_text(v + "\n")
            (root / "sys/bus/usb/devices").mkdir(parents=True, exist_ok=True)
            (root / "sys/bus/usb/devices" / name).symlink_to(d)
            node = root / f"dev/bus/usb/{int(bus):03d}/{int(dev):03d}"
            node.parent.mkdir(parents=True, exist_ok=True)
            node.write_text("")
            if hid:
                h = d / f"{name}:1.0/0003:{vid.upper()}:{pid.upper()}.0001"
                h.mkdir(parents=True)
                (root / "sys/class/hidraw" / hid).mkdir(parents=True)
                (root / "sys/class/hidraw" / hid / "device").symlink_to(h)
                (root / "sys/class/hidraw" / hid / "dev").write_text(f"243:{hid[-1]}\n")
                (root / "dev" / hid).write_text("")

        usbdev("5-1", "abcd", "1234", "5", "2", hid="hidraw1")
        usbdev("5-2", "1050", "0407", "5", "3", hid="hidraw2")  # a YubiKey: never
        (root / "sys/bus/usb/devices/usb5").symlink_to(devs)  # the root hub: no idVendor here
        sys.path.insert(0, str(HERE))
        zone = self._zone_mod()
        a, found = zone.usb_binds(["abcd"], root=str(root))
        real = "/" + os.path.relpath(os.path.realpath(devs / "5-1"), os.path.realpath(root))
        self.assertEqual(len(found), 1, found)
        self.assertIn("abcd:1234", found[0])
        triples = [tuple(a[i:i + 3]) for i in range(0, len(a), 3)]
        self.assertIn(("--dev-bind", "/dev/bus/usb/005/002", "/dev/bus/usb/005/002"), triples)
        self.assertIn(("--ro-bind", real, real), triples)
        self.assertIn(("--dev-bind", "/dev/hidraw1", "/dev/hidraw1"), triples)
        self.assertIn(("--ro-bind-try", "/sys/bus/usb", "/sys/bus/usb"), triples)
        self.assertIn(("--ro-bind-try", "/run/udev/data/c243:1", "/run/udev/data/c243:1"), triples)
        flat = " ".join(a)
        for never in ("/dev/bus/usb/005/003", "/dev/hidraw2", "5-2", "/dev/uinput"):
            self.assertNotIn(never, flat)
        self.assertNotIn(("--dev-bind", "/dev/bus/usb", "/dev/bus/usb"), triples)
        # vendor:product must match both; nothing matching: nothing bound
        self.assertEqual(zone.usb_binds(["abcd:1234"], root=str(root))[1], found)
        self.assertEqual(zone.usb_binds(["abcd:0001"], root=str(root)), ([], []))
        self.assertEqual(zone.usb_binds(["dead"], root=str(root)), ([], []))
        # a node that is not the device sysfs names (another took its number): skipped
        (devs / "5-1/dev").write_text("189:513\n")
        with contextlib.redirect_stderr(io.StringIO()) as err:
            self.assertEqual(zone.usb_binds(["abcd"], root=str(root)), ([], []))
        self.assertIn("is not abcd:1234", err.getvalue())

    def test_mount_sources(self):
        h = self.home
        (h / "Docs/proj").mkdir()
        (h / "Docs/file").write_text("")
        self.assertIsNone(self.z.mount_source_error(str(h / "Docs/proj")))
        self.assertIsNone(self.z.mount_source_error("~/Docs"))
        for path, why in [("~/.ssh", "is"), ("~/Docs/sshlink", "resolves to"), ("~", "never allowed"),
                          ("~/.config", "contains"), ("~/.config/opencode", "is"), ("/srv/code", "is"),
                          ("/etc", "is inside /etc"), ("/run/user/1000", "is inside /run"), ("/nix/store", "/nix"),
                          ("/boot", "is inside /boot"), ("/proc/1", "is inside /proc"), ("/", "never allowed"),
                          ("~/Docs/file", "not a directory"), ("~/nonexistent", "does not exist"),
                          ("relative/dir", "must be absolute")]:
            with self.subTest(path=path):
                e = self.z.mount_source_error(path)
                self.assertIsNotNone(e, path)
                self.assertIn(why, e)
        for name in ("proj", "my.dir_2", "A-b"):
            self.assertTrue(self.z.MOUNT_NAME_RE.match(name), name)
        for name in ("..", ".", ".hidden", "a/b", "", "x" * 65, "-x"):
            self.assertFalse(self.z.MOUNT_NAME_RE.match(name), name)

    def test_env_never_list(self):
        for name in ["SSH_AUTH_SOCK", "GITHUB_TOKEN", "OPENAI_API_KEY", "HYPRLAND_INSTANCE_SIGNATURE", "LD_PRELOAD"]:
            with self.subTest(name=name):
                self.assertRefused(self.errs(f'env = {{ {name} = "x" }}'), "never passed")
        self.assertEqual(self.errs('env = { GTK_THEME = "Adwaita:dark" }'), [])

    def test_userns_false_warns(self):
        cfg = self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\nuserns = false\n')
        self.assertEqual(cfg.errors, [])
        self.assertTrue(any("weaker" in w for w in cfg.warnings), cfg.warnings)

    def test_binds_of_host_code_refused(self):
        # a writable bind of what the host runs at login would be a way out
        for d in (".config/hypr", ".config/appzones", ".local/share/appzones/other/home", ".config/zones",
                  ".local/share/zones/other/home", ".config/systemd"):
            (self.home / d).mkdir(parents=True)
            with self.subTest(path=d):
                self.assertRefused(self.errs(f'binds = [{{ path = "~/{d}" }}]'), "never enters a zone")
        (self.home / ".bashrc").write_text("")
        self.assertRefused(self.errs('binds = [{ path = "~/.bashrc", mode = "ro" }]'), "never enters a zone")

    def test_reserved_and_bad_tables(self):
        self.assertRefused(self.cfg('version = 1\n[zones.launcher]\ncolour = "#112233"\n').errors, "reserved")
        self.assertRefused(self.cfg('version = 1\nzones = { aa = 1 }\n').errors, "must be a table")
        self.assertRefused(self.errs("", '[apps.xx]\ncommand = ["x"]\nzones = "tt"\n'), "must be a table")

    def test_resolve_app(self):
        cfg = self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\n[zones.uu]\ncolour = "#223344"\n'
                       '[apps.ff]\ncommand = ["firefox"]\nzones = ["tt"]\nargs = { tt = ["-P", "x"] }\n')
        self.assertEqual(cfg.errors, [])
        self.assertEqual(self.z.resolve_app(cfg, "tt", "ff"), ["firefox", "-P", "x"])
        self.assertIsNone(self.z.resolve_app(cfg, "tt", "nope"))
        with self.assertRaises(ValueError):
            self.z.resolve_app(cfg, "uu", "ff")

    def test_not_built_warnings(self):
        w = self.cfg('version = 1\n[zones.tt]\ncolour = "#112233"\nkeyring = "zone"\nnetwork = "host"\n').warnings
        self.assertTrue(any("not built yet" in x for x in w), w)
        self.assertTrue(any("localhost" in x for x in w), w)

    def test_apps(self):
        extra = '[apps.ff]\ncommand = ["firefox"]\nzones = ["tt", "nope"]\nargs = { other = ["-x"] }\n'
        e = self.errs('autostart_apps = ["ghost"]', extra)
        self.assertRefused(e, "unknown zone 'nope'")
        self.assertRefused(e, "args for 'other'")
        self.assertRefused(e, "undeclared app 'ghost'")

    def test_list_json(self):
        """zone_list --json outputs valid JSON with correct fields for running and stopped zones."""
        zone = self._zone_mod()
        # Stub the functions that would call real system resources
        zone.ping = lambda n: {"ok": True} if n == "daily" else None
        zone.zone_procs = lambda n: [1, 2]
        zone.stale = lambda n, z: False
        zone.load_cfg = lambda quiet=False: self.cfg('version = 1\nrequires = ' + str(self.z.RELEASE) + '\n'
                                                      '[zones.daily]\ncolour = "#a6e3a1"\nnetwork = "internet"\n'
                                                      '[zones.work]\ncolour = "#f38ba8"\nnetwork = "none"\n')
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            zone.zone_list(as_json=True)
        data = json.loads(out.getvalue())
        self.assertIsInstance(data, list)
        self.assertEqual(len(data), 2)
        # Find zones by name
        daily = next((z for z in data if z["name"] == "daily"), None)
        work = next((z for z in data if z["name"] == "work"), None)
        self.assertIsNotNone(daily)
        self.assertIsNotNone(work)
        # Check daily zone (running)
        self.assertEqual(daily["name"], "daily")
        self.assertEqual(daily["state"], "running")
        self.assertEqual(daily["network"], "internet")
        self.assertEqual(daily["dns"], "host")
        self.assertEqual(daily["devices"], ["gpu", "audio"])
        self.assertEqual(daily["apps"], 2)
        self.assertEqual(daily["colour"], "#a6e3a1")
        # Check work zone (stopped)
        self.assertEqual(work["name"], "work")
        self.assertEqual(work["state"], "stopped")
        self.assertEqual(work["network"], "none")
        self.assertIsNone(work["apps"])
        # Verify table output also works
        out2 = io.StringIO()
        with contextlib.redirect_stdout(out2):
            zone.zone_list(as_json=False)
        table = out2.getvalue()
        self.assertIn("ZONE", table.split("\n")[0])
        self.assertIn("running", table)
        self.assertIn(" 2 ", table)


if __name__ == "__main__":
    unittest.main()
