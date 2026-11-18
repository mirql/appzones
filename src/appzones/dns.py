"""A zone's DNS (network = "internet"), the host side.

zone-init answers DNS inside the zone at common.DNS_ADDR and hands each query,
one connection per query, over the zone's dns.sock to this, in the server on
the host. Here the query goes to the host's resolver (dns = "host") or to
1.1.1.1 (dns = "public"), and the answer goes back. Only standard DNS queries
leave the zone this way, and only to that resolver: the zone does not choose
where they go. Works with any pasta and any resolver, also a loopback one like
systemd-resolved's 127.0.0.53, which the zone could never reach itself.

Framing on dns.sock, both ways: a 2-byte big-endian length, then the message
(as DNS over TCP).
"""
from __future__ import annotations

import socket
import struct
import threading
from pathlib import Path

import zonecfg

from . import net

PORT = 53
MAX_QUERY = 4096  # a query is small; anything bigger is refused
MAX_INFLIGHT = 64  # queries answered at once, per zone
UDP_TIMEOUT = 2.0
TCP_TIMEOUT = 5.0
CONN_TIMEOUT = 10.0  # for zone-init to send its query and read the answer


def upstreams(z: zonecfg.Zone) -> list[str]:
    """Where the zone's queries go: the host's resolver, or 1.1.1.1 and 1.0.0.1."""
    if z.settings["dns"] == "public":
        return ["1.1.1.1", "1.0.0.1"]  # NOSONAR: the public resolvers when dns = "public"
    return [net.host_resolver()]


def is_query(msg: bytes) -> bool:
    """A standard query: a header, QR = 0, opcode QUERY, at least one question."""
    return (12 <= len(msg) <= MAX_QUERY and not msg[2] & 0x80 and (msg[2] >> 3) & 0xF == 0
            and struct.unpack(">H", msg[4:6])[0] >= 1)


def recv_exact(s: socket.socket, n: int) -> bytes | None:
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def _ask_tcp(addr, family: int, msg: bytes) -> bytes | None:
    with socket.socket(family, socket.SOCK_STREAM) as s:
        s.settimeout(TCP_TIMEOUT)
        s.connect(addr)
        s.sendall(struct.pack(">H", len(msg)) + msg)
        head = recv_exact(s, 2)
        if not head:
            return None
        ans = recv_exact(s, struct.unpack(">H", head)[0])
    return ans if ans and ans[:2] == msg[:2] else None


def ask(server: str, msg: bytes) -> bytes | None:
    """MSG to SERVER over UDP, then over TCP when the answer was truncated; the
    answer, or None. Only an answer with the query's id counts."""
    try:
        infos = socket.getaddrinfo(server, PORT, type=socket.SOCK_DGRAM)
    except OSError:
        return None
    for family, _, _, _, addr in infos:
        try:
            with socket.socket(family, socket.SOCK_DGRAM) as s:
                s.settimeout(UDP_TIMEOUT)
                s.connect(addr)  # only the server's own replies come in
                s.send(msg)
                while True:
                    ans = s.recv(65535)
                    if len(ans) >= 12 and ans[:2] == msg[:2]:
                        break
            if ans[2] & 0x02:  # TC: the whole answer over TCP
                return _ask_tcp(addr, family, msg) or ans
            return ans
        except OSError:
            continue
    return None


def resolve(servers: list[str], msg: bytes) -> bytes | None:
    for _ in range(2):
        for server in servers:
            ans = ask(server, msg)
            if ans:
                return ans
    return None


def _handle(conn: socket.socket, servers: list[str], slots: threading.BoundedSemaphore) -> None:
    try:
        with conn:
            conn.settimeout(CONN_TIMEOUT)
            head = recv_exact(conn, 2)
            if not head:
                return
            n = struct.unpack(">H", head)[0]
            if n > MAX_QUERY:
                return
            msg = recv_exact(conn, n)
            if not msg or not is_query(msg):
                return
            ans = resolve(servers, msg)
            if ans:
                conn.sendall(struct.pack(">H", len(ans)) + ans)
    except OSError:
        pass
    finally:
        slots.release()


def serve(path: Path, servers: list[str]) -> socket.socket:
    """Listen on PATH (the zone's dns.sock, host side) and answer in threads
    until the process ends. Returns the listening socket."""
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(str(path))
    srv.listen(64)
    slots = threading.BoundedSemaphore(MAX_INFLIGHT)

    def loop() -> None:
        while True:
            try:
                conn, _ = srv.accept()
            except OSError:
                return  # closed: the zone is gone
            if not slots.acquire(blocking=False):
                conn.close()  # too many at once: the client retries
                continue
            threading.Thread(target=_handle, args=(conn, servers, slots), daemon=True).start()

    threading.Thread(target=loop, daemon=True, name="dns").start()
    return srv
