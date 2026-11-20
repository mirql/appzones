"""Portals and links: flatpak-info, the D-Bus proxy, the in-zone xdg-open."""
from __future__ import annotations

import os
import socket
import subprocess
import sys
from pathlib import Path

import zonecfg

from .common import tool
from . import common, fs


# ── portals: flatpak-info and the D-Bus proxy ──────────────────────────────
def flatpak_info(z: zonecfg.Zone, instance: str) -> str:
    return (f"[Application]\nname=dev.mirql.zone.{z.name}\nruntime=runtime/dev.mirql.zones/x86_64/1\n\n"
            f"[Instance]\ninstance-id={instance}\nsession-bus-proxy=true\nsystem-bus-proxy=false\n"
            f"sandbox-engine=dev.mirql.zones\n")


# The portal interfaces a zone may call on org.freedesktop.portal.Desktop.
# Not flatpak's "org.freedesktop.portal.*" pair: that also grants
# org.freedesktop.portal.Flatpak, whose Spawn acts on the caller's
# .flatpak-info. Left out on purpose: RemoteDesktop and
# InputCapture (input injection), Background and DynamicLauncher (they write
# host autostart and launcher entries), Usb, Device, Clipboard.
PORTALS = ["FileChooser", "OpenURI", "Notification", "Settings", "Email", "Print", "Trash", "Inhibit",
           "NetworkMonitor", "ProxyResolver", "PowerProfileMonitor", "MemoryMonitor", "GameMode", "Realtime",
           "Secret", "Request", "Session", "GlobalShortcuts"]
PORTALS_BY_DEVICE = {"screencast": ["ScreenCast", "Screenshot"], "camera": ["Camera"]}
# zone-init gives these their own D-Bus app name per zone (MOZ_DBUS_APP_NAME) (zone_init.py MOZ_APPS)
MOZ_APPS = ("firefox", "firefox-esr", "firefox-beta", "firefox-devedition", "firefox-nightly", "librewolf")


def moz_remoting_name(zone: str, app: str) -> str:
    return f"{app.replace('-', '_')}_zone_{zone.replace('-', '_')}"


def dbus_rules(z: zonecfg.Zone) -> list[str]:
    devices = z.settings["devices"]
    allowed = list(dict.fromkeys(PORTALS + [p for d, ps in PORTALS_BY_DEVICE.items() if d in devices for p in ps]))
    rules = ["--call=org.freedesktop.portal.Desktop=org.freedesktop.DBus.Properties.*",
             "--call=org.freedesktop.portal.Desktop=org.freedesktop.DBus.Introspectable.*",
             "--call=org.freedesktop.portal.Desktop=org.freedesktop.DBus.Peer.*"]
    rules += [f"--call=org.freedesktop.portal.Desktop=org.freedesktop.portal.{p}.*" for p in allowed]
    rules += ["--broadcast=org.freedesktop.portal.Desktop=@/org/freedesktop/portal/*",
              "--talk=org.freedesktop.portal.Documents", "--talk=org.freedesktop.Notifications",
              "--talk=org.kde.StatusNotifierWatcher", f"--own=dev.mirql.zone.{z.name}.*"]
    # no org.mpris.MediaPlayer2.*: owning implies talking, and that name is
    # shared by the host's and every zone's players; a zone opts in per
    # player with dbus.own (docs/configuration/dbus.md)
    # each Mozilla app's remote name in this zone only: never org.mozilla.firefox.*,
    # which the host's Firefox and every other zone's would share
    rules += [f"--own=org.mozilla.{moz_remoting_name(z.name, a)}.*" for a in MOZ_APPS]
    rules += [f"--talk={n}" for n in z.settings["dbus"].get("talk", [])]
    rules += [f"--own={n}" for n in z.settings["dbus"].get("own", [])]
    if zonecfg.env("DBUS_LOG"):
        rules.append("--log")
    return rules


def dbus_proxy_argv(z: zonecfg.Zone, zrt: Path) -> list[str]:
    host_bus = os.environ["DBUS_SESSION_BUS_ADDRESS"]
    # The proxy runs in its own small bwrap that carries the zone's
    # /.flatpak-info: the portal reads /proc/<peer pid>/root/.flatpak-info,
    # and the peer it sees is the proxy (flatpak-run.c add_bwrap_wrapper).
    return [tool("bwrap"), "--unshare-all", "--share-net", "--die-with-parent",
            *fs.system_binds(), "--bind", str(common.HOST_RT), str(common.HOST_RT),
            "--ro-bind", str(zrt / "flatpak-info"), "/.flatpak-info",
            "--", tool("xdg-dbus-proxy"), host_bus, str(zrt / "bus"), "--filter", *dbus_rules(z)]


# ── links: an in-zone xdg-open that hands http(s) URLs to a zone's browser ─
XDG_OPEN = """#!/bin/sh
# zone xdg-open (links = "{links}"): http(s) URLs open in zone {target}'s
# browser through the zone's link socket; anything else goes to the real xdg-open.
case "$1" in
  http://*|https://*)
    exec {open_link} "$1" ;;
esac
exec {xdg_open} "$@"
"""


PY_OPEN_LINK = ("{python} -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(\"/run/zone/links.sock\"); "
                "s.sendall(sys.argv[1].encode()+b\"\\n\"); sys.exit(0 if s.recv(3)==b\"ok\\n\" else 1)'")


def open_link_command(python: str) -> str:
    """What the xdg-open shim runs for an http(s) link: with the Rust helper, its open-link
    from the read-only copy in the zone (fs.ZONE_HELPER); else PYTHON with a one-liner."""
    if common.zone_helper_has("open-link"):  # checked on the very helper bound into the zone
        return f"{fs.ZONE_HELPER} open-link"
    return PY_OPEN_LINK.format(python=python)


def real_xdg_open() -> str:
    """The zone's own xdg-open, which the shim hands anything but http(s) URLs."""
    return "/run/current-system/sw/bin/xdg-open" if zonecfg.host_layout() == "nixos" else "/usr/bin/xdg-open"


def link_target(z: zonecfg.Zone) -> str | None:
    links = z.settings["links"]
    if links == "portal":
        return None
    return z.name if links == "zone" else links[5:]


def browser_for(cfg: zonecfg.Config, zone: str) -> str | None:
    """The first declared app for ZONE that is a browser (firefox, chromium, ...)."""
    for aid, a in cfg.apps.items():
        cmd = os.path.basename(a["command"][0]).lower()
        if zone in a.get("zones", []) and any(b in cmd for b in ("firefox", "chromium", "chrome", "librewolf", "brave")):
            return aid
    return None


def serve_links(z: zonecfg.Zone, zrt: Path, target: str) -> socket.socket:
    """Listen on the zone's link socket (host side). Only an http(s) URL, as
    one argv element, ever leaves the zone this way."""
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    path = zrt / "ctl" / "links.sock"
    srv.bind(str(path))
    srv.listen(8)
    srv.setblocking(False)
    return srv


def handle_link(srv: socket.socket, target: str) -> None:
    try:
        conn, _ = srv.accept()
    except BlockingIOError:
        return
    with conn:
        conn.settimeout(2)
        try:
            url = conn.recv(8192).decode(errors="replace").strip()
        except OSError:
            return
        ok = (url.startswith(("http://", "https://")) and len(url) < 8000 and not any(c in url for c in "\n\r\0"))
        if ok:
            cfg = zonecfg.load(common.CONFIG)
            app = browser_for(cfg, target) if not cfg.errors else None
            if app:
                subprocess.Popen([sys.executable, str(common.HERE / "zone.py"), "run", target, app, url],
                                 stdin=subprocess.DEVNULL, start_new_session=True)
            else:
                ok = False
                print(f"[zone] links: no browser declared for zone {target}", flush=True)
        try:
            conn.sendall(b"ok\n" if ok else b"no\n")
        except OSError:
            pass
