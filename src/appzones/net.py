"""The zone's network: the nft deny list, pasta, DNS."""
from __future__ import annotations

import ipaddress
import json
import subprocess
from pathlib import Path

import zonecfg

from .common import tool
from . import common


# ── network ────────────────────────────────────────────────────────────────
def host_addrs() -> list[tuple[str, ipaddress.IPv4Interface | ipaddress.IPv6Interface]]:
    """(interface, address/prefix) for every address of the host, from `ip -j addr`."""
    r = subprocess.run(["ip", "-j", "addr", "show"], capture_output=True, text=True)
    if r.returncode != 0:
        raise OSError(f"ip -j addr show: {r.stderr.strip() or r.returncode}")
    out = []
    for link in json.loads(r.stdout or "[]"):
        for a in link.get("addr_info", []):
            try:
                out.append((link.get("ifname", ""), ipaddress.ip_interface(f"{a['local']}/{a['prefixlen']}")))
            except (KeyError, ValueError):
                continue
    return out


def _nft_set(items) -> str:
    return ", ".join(str(i) for i in items)


def nft_rules(z: zonecfg.Zone) -> str:
    """The deny list loaded inside the zone's own netns: pasta turns zone
    traffic into host sockets, so without it the zone reaches the LAN, the
    tailnet and the host's own non-loopback listeners. Computed at zone
    start from the host's addresses: every address of the host is refused
    (never the host itself, also with netbird), then the NetBird networks
    are allowed (netbird = true), then every network the host is directly
    connected to (a LAN with global IPv6 or public IPv4 addresses too) and
    the private, CGNAT, link-local and multicast ranges are refused."""
    addrs = [(i, a) for i, a in host_addrs() if not a.ip.is_loopback]
    mine4 = sorted({a.ip for _, a in addrs if a.version == 4})
    mine6 = sorted({a.ip for _, a in addrs if a.version == 6})
    nets = [a.network for _, a in addrs if not a.ip.is_link_local]
    nb4 = nb6 = []
    if z.settings["netbird"]:
        nbn = [a.network for i, a in addrs if i == common.NETBIRD_IFACE]
        nb4 = list(ipaddress.collapse_addresses(n for n in nbn if n.version == 4))
        nb6 = list(ipaddress.collapse_addresses(n for n in nbn if n.version == 6))
    lan4 = list(ipaddress.collapse_addresses(n for n in nets if n.version == 4 and n not in nb4))
    lan6 = list(ipaddress.collapse_addresses(n for n in nets if n.version == 6 and n not in nb6))
    rules = ""
    if mine4:
        rules += f"    ip daddr {{ {_nft_set(mine4)} }} reject\n"
    if mine6:
        rules += f"    ip6 daddr {{ {_nft_set(mine6)} }} reject\n"
    if nb4:
        rules += f"    ip daddr {{ {_nft_set(nb4)} }} accept\n"
    if nb6:
        rules += f"    ip6 daddr {{ {_nft_set(nb6)} }} accept\n"
    if lan4:
        rules += f"    ip daddr {{ {_nft_set(lan4)} }} reject\n"
    if lan6:
        rules += f"    ip6 daddr {{ {_nft_set(lan6)} }} reject\n"
    return f"""table inet zone {{
  chain out {{
    type filter hook output priority 0; policy accept;
    oif "lo" accept
    ip daddr {common.DNS_ADDR} udp dport 53 accept
    ip daddr {common.DNS_ADDR} tcp dport 53 accept
{rules}    ip daddr {{ 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12,
               192.0.0.0/24, 192.168.0.0/16, 198.18.0.0/15, 224.0.0.0/4, 240.0.0.0/4 }} reject
    ip6 daddr {{ ::1, ::ffff:0:0/96, fc00::/7, fe80::/10, ff00::/8 }} reject
  }}
}}
"""


def pasta_argv(z: zonecfg.Zone) -> list[str]:
    a = [tool("pasta"), "--config-net", "--no-map-gw", "-t", "none", "-u", "none", "-T", "none", "-U", "none",
         "--dns-forward", common.DNS_ADDR, "-q", "-f"]
    # pasta skips loopback nameservers from resolv.conf ("Couldn't get any
    # nameserver address"), so name the resolver explicitly.
    a += ["--dns-host", "1.1.1.1" if z.settings["dns"] == "public" else host_resolver()]  # NOSONAR: the public resolver when dns = "public"
    return a + ["--"]


def resolv_conf(s: dict) -> str:
    """The zone's /etc/resolv.conf. internet: pasta's forwarder (which goes to
    host_resolver() or 1.1.1.1); host: the same choice, direct; none: empty."""
    if s["network"] == "internet":
        return f"nameserver {common.DNS_ADDR}\n"
    if s["network"] == "host":
        if s["dns"] == "public":
            return "nameserver 1.1.1.1\n"
        if zonecfg.env("DNS_HOST"):
            return f"nameserver {zonecfg.env('DNS_HOST')}\n"
        try:
            return Path("/etc/resolv.conf").read_text()
        except OSError:
            return ""
    return ""


def host_resolver(resolv: str = "/etc/resolv.conf") -> str:
    """The resolver for dns = "host": $APPZONES_DNS_HOST (the module's dnsServer),
    else the first nameserver in the host's resolv.conf (systemd-resolved's
    127.0.0.53, dnsmasq's 127.0.0.1, a LAN resolver, ...)."""
    if zonecfg.env("DNS_HOST"):
        return zonecfg.env("DNS_HOST")
    try:
        for line in Path(resolv).read_text().splitlines():
            parts = line.split()
            if len(parts) >= 2 and parts[0] == "nameserver":
                ipaddress.ip_address(parts[1].split("%")[0])
                return parts[1]
    except (OSError, ValueError):
        pass
    return "127.0.0.53"
