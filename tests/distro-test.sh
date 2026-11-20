#!/bin/sh
# appzones on another distribution, inside its disk image (tests/distro.nix):
# `make install` (or the .deb) as root, then zones as an ordinary user, without systemd (zones
# start on demand) and without a network (a dummy link gives pasta a route; a
# local dnsmasq is the host's resolver). Prints "PASS ..." per check, exits non-zero
# on the first failure.
set -eu
src=$1
mflags=${2:-}  # extra make arguments (tests/distro.nix makeFlags)
deb=${3:-}     # a .deb to install instead of `make install` (tests/distro.nix deb)
# where it installs (make's PREFIX, /usr for the .deb), and whether with the Rust helper
prefix=/usr/local
for f in $mflags; do case $f in PREFIX=*) prefix=${f#PREFIX=};; esac; done
[ -z "$deb" ] || prefix=/usr
rust=no
case " $mflags " in *" HELPER=rust "*) rust=yes;; esac
export PATH=$prefix/bin:/usr/sbin:/usr/bin:/sbin:/bin
fail() { echo "FAIL $*"; exit 1; }
pass() { echo "PASS $*"; }
U="env -i HOME=/home/alice USER=alice LOGNAME=alice XDG_RUNTIME_DIR=/run/user/1000 PATH=$prefix/bin:/usr/bin:/bin LANG=C.UTF-8"
# every step as alice gets a time limit: a hang fails with a message, not a stuck build
as_alice() {
  rc=0; timeout 120 su alice -s /bin/sh -c "$U sh -c '$1'" || rc=$?
  [ $rc != 124 ] || fail "timed out: $1"  # never a refusal that passes
  return $rc
}

head -1 /etc/os-release
useradd -m -u 1000 alice
install -d -m 700 -o alice -g alice /run/user/1000

# the "host": a network the zone must not reach, and its resolver on loopback
ip link set lo up
# the build VM's /dev is minimal: pasta's device, as udev makes it on a real system
[ -e /dev/net/tun ] || { mkdir -p /dev/net && mknod /dev/net/tun c 10 200; }
# 0666 is the mode udev gives /dev/net/tun; this is the throw-away test VM
chmod 666 /dev/net/tun  # NOSONAR
ip link add az0 type dummy && ip link set az0 up
ip addr add 192.0.2.1/24 dev az0 && ip route add default via 192.0.2.254 dev az0
dnsmasq --no-resolv --listen-address=127.0.0.1 --bind-interfaces --address=/zone-test.example/192.0.2.99 \
  --pid-file=/run/dnsmasq.pid </dev/null >/dev/null 2>&1
echo "nameserver 127.0.0.1" > /etc/resolv.conf
python3 -c 'import socket,os
s=socket.socket(); s.bind(("192.0.2.1",8080)); s.listen(4)
if os.fork(): os._exit(0)
while True: s.accept()[0].close()' </dev/null >/dev/null 2>&1  # background: no pipe of ours held open

# install as root: the .deb, or from a copy of the checkout
if [ -n "$deb" ]; then
  dpkg -i "$deb" >/tmp/install.log 2>&1 || { cat /tmp/install.log; fail "dpkg -i"; }
  getent group appzones >/dev/null || fail "no group appzones"
  usermod -aG appzones alice
  [ -x /usr/lib/appzones/zone-wayland ] || fail "no zone-wayland in the package"
  if [ -e /etc/apparmor.d/abi/4.0 ]; then
    # what postinst installs where user namespaces are restricted (not in this VM's kernel)
    /usr/lib/appzones/apparmor-profile >/tmp/aa-profile || fail "apparmor-profile"
    apparmor_parser -Q -K /tmp/aa-profile || fail "the AppArmor profile does not parse"
  fi
  [ ! -e /etc/apparmor.d/appzones ] || fail "an AppArmor profile where user namespaces are not restricted"
  pass "dpkg -i (dependencies there, group appzones; the AppArmor profile only where needed, and it parses)"
else
cp -r "$src" /tmp/appzones && chmod -R u+w /tmp/appzones && cd /tmp/appzones
make $mflags >/tmp/make.log 2>&1 || { cat /tmp/make.log; fail "make"; }
grep -q "built build/zone-wayland" /tmp/make.log || fail "zone-wayland not built: $(tail -2 /tmp/make.log)"
# the profile needs AppArmor 4 (Ubuntu 24.04); where there is no restriction (Debian 12) none is installed
if [ -e /etc/apparmor.d/abi/4.0 ]; then aa=yes; else aa=auto; fi
make install $mflags MOUNT_USERS=alice APPARMOR=$aa SYSTEMD=no >/tmp/install.log 2>&1 || { cat /tmp/install.log; fail "make install"; }
if [ $aa = yes ]; then
  apparmor_parser -Q -K /etc/apparmor.d/appzones || fail "AppArmor profile does not parse"
  pass "make; make install (zone-wayland built, the AppArmor profile parses)"
else
  [ ! -e /etc/apparmor.d/appzones ] || fail "an AppArmor profile where user namespaces are not restricted"
  pass "make; make install (zone-wayland built, no AppArmor profile: not needed here)"
fi
fi
v=$(as_alice "appzones --version")
echo "$v" | grep -q "^appzones [0-9][0-9.]* (config release [0-9]*)$" || fail "version: $v"
pass "$v"

# alice's files: a project, a folder to mount, and a secret that never enters a zone
as_alice 'mkdir -p ~/.config/appzones ~/proj ~/data ~/.ssh && echo hello >~/proj/f && echo data >~/data/x && echo secret >~/.ssh/id_test'
as_alice 'cat >~/.config/appzones/zones.toml <<EOF
version = 1
requires = 6
[defaults]
devices = []
[zones.off]
colour = "#a6e3a1"
network = "none"
binds = [{ path = "~/proj", mode = "rw" }]
[zones.web]
colour = "#89b4fa"
[zones.gui]
colour = "#f38ba8"
network = "none"
wayland = "host"
EOF'
as_alice "appzones check" | grep -q "^ok:" || fail "appzones check"
pass "appzones check"

out=$(as_alice 'appzones run --wait off -- sh -c "id -un; cat ~/proj/f; ls -A ~; test ! -e ~/.ssh && echo no-ssh" </dev/null')
echo "$out"
echo "$out" | grep -qx alice && echo "$out" | grep -qx hello && echo "$out" | grep -qx no-ssh || fail "zone off"
pass "a zone without network starts on demand: its own home, the bind, no ~/.ssh"
as_alice 'appzones run --wait off -- unshare -Ur true </dev/null' || fail "a sandbox inside the zone"
pass "a sandbox inside the zone (user namespace)"
if [ $rust = yes ]; then
  init=$(pgrep -u alice -x zone-init | head -1)
  cmd=$(tr '\0' ' ' </proc/$init/cmdline)
  [ "$cmd" = "/run/appzones-helper zone-init " ] || fail "zone-init is not the Rust helper: $cmd"
  inside=$(as_alice 'appzones run --wait off -- sh -c "cat /proc/[0-9]*/cmdline | tr \"\\000\" \" \""' </dev/null)
  if echo "$inside" | grep -q python; then fail "Python runs in the zone: $inside"; fi
  pass "the Rust helper: zone-init runs from /run/appzones-helper, no Python in the zone (installed under $prefix)"
  case " $mflags " in *" MOUNT_HELPER_BIN="*)
    [ "$(head -c 4 $prefix/lib/appzones/appzones-mount-helper | od -An -c | tr -d ' ')" = '177ELF' ] \
      || fail "the mount helper is not the Rust one"
    pass "the mount helper is the Rust one";;
  esac
fi
(sleep 2; printf 'echo in-shell-$ZONE\r'; sleep 1; printf 'exit\r') \
  | timeout 60 su alice -s /bin/sh -c "$U script -qfc 'appzones shell off' /dev/null" >/tmp/shell.out \
  || fail "appzones shell did not end: $(tr -d '\r' </tmp/shell.out)"
# (without the terminal's escapes: readline's bracketed paste where a terminfo is installed)
out=$(tr -d '\r' </tmp/shell.out | sed 's/\x1b\[[?0-9;]*[a-zA-Z]//g')
echo "$out" | grep -q "^in-shell-off" || fail "appzones shell: $out"
pass "appzones shell (a pty in the zone)"

# without systemd the host's mounts are private: a new mount does not reach the zone, and appzones says why
out=$(as_alice 'appzones mount off ~/data --ro' 2>&1) || fail "appzones mount: $out"
echo "$out" | grep -q "mount --make-rshared /" || fail "no hint for a private host mount: $out"
as_alice 'appzones umount off data' >/dev/null
pass "appzones mount on a host without systemd: names the fix (mount --make-rshared /)"
mount --make-rshared /  # what systemd does at boot
as_alice 'appzones restart off'
as_alice 'appzones mount off ~/data --ro' >/dev/null || fail "appzones mount"
got=$(as_alice 'appzones run --wait off -- cat /mnt/host/data/x </dev/null' 2>&1) || true
[ "$got" = data ] || { echo "$got"; fail "the mount in the zone"; }
if as_alice 'appzones run --wait off -- sh -c "echo x >/mnt/host/data/y" </dev/null' 2>/dev/null; then fail "read-only mount written"; fi
as_alice 'appzones umount off data' >/dev/null || fail "appzones umount"
pass "appzones mount --ro through the sudo rule, and umount"

out=$(as_alice 'appzones run --wait web -- getent hosts zone-test.example </dev/null')
echo "$out" | grep -q "^192.0.2.99" || fail "DNS in the zone: $out"
pass "an internet zone: names from the host's loopback resolver (zone-init, the server)"
if as_alice 'appzones run --wait web -- python3 -c "import socket; socket.create_connection((\"192.0.2.1\", 8080), 3)" </dev/null' 2>/dev/null; then
  fail "the host reachable from the zone"
fi
pass "an internet zone: the host is refused"

as_alice 'appzones stop off; appzones stop web'
# a server holds its zone's lock (whatever it is written in)
if fuser /run/user/1000/appzones/off.lock /run/user/1000/appzones/web.lock >/dev/null 2>&1; then fail "a server left after stop"; fi
if pgrep -u alice -x zone-init >/dev/null; then fail "a zone-init left after stop"; fi
[ -s /home/alice/.local/state/appzones/web.log ] || fail "no log in ~/.local/state/appzones"
pass "stop: no server left; logs in ~/.local/state/appzones"

# a compositor without the security-context protocol (Weston): the zone starts without a
# Wayland socket and says why; it never gets the host's socket
as_alice '(weston --backend=headless-backend.so --socket=wayland-9 --idle-time=0 >/tmp/weston-alice.log 2>&1 &); sleep 3; test -S /run/user/1000/wayland-9' \
  || fail "weston did not start: $(tail -3 /tmp/weston-alice.log 2>/dev/null)"
UW="$U WAYLAND_DISPLAY=wayland-9"
out=$(timeout 120 su alice -s /bin/sh -c "$UW sh -c 'appzones run --wait off -- sh -c \"echo works; echo wl=\\\$WAYLAND_DISPLAY; ls /run/user/1000\" </dev/null'" 2>&1) || fail "a zone with Weston: $out"
echo "$out"
echo "$out" | grep -qx works && echo "$out" | grep -qx "wl=" || fail "a zone with Weston: $out"
if echo "$out" | grep -q wayland; then fail "a Wayland socket in the zone: $out"; fi
grep -q "no security-context protocol" /home/alice/.local/state/appzones/off.log \
  || fail "not said why: $(tail -5 /home/alice/.local/state/appzones/off.log)"
as_alice 'appzones stop off'
pkill -u alice weston || true
pass "a compositor without the security-context protocol (Weston): the zone starts without Wayland, says why"
# wayland = "host" for such a compositor: the host's socket as the zone's wayland-0
as_alice '(weston --backend=headless-backend.so --socket=wayland-9 --idle-time=0 >/tmp/weston-alice.log 2>&1 &); sleep 3; test -S /run/user/1000/wayland-9' \
  || fail "weston did not start again"
out=$(timeout 120 su alice -s /bin/sh -c "$UW sh -c 'appzones run --wait gui -- sh -c \"echo wl=\\\$WAYLAND_DISPLAY; wayland-info | grep -c wl_compositor\" </dev/null'" 2>&1) \
  || fail "wayland = host: $out"
echo "$out"
echo "$out" | grep -qx "wl=wayland-0" && echo "$out" | grep -qx 1 || fail "wayland = host: $out"
grep -q 'wayland = "host": the host' /home/alice/.local/state/appzones/gui.log || fail "wayland = host not logged"
as_alice 'appzones stop gui'
pkill -u alice weston || true
pass "wayland = \"host\": the zone's apps reach the host's compositor (Weston) through wayland-0"

# a desktop: headless sway, a Wayland app from a zone. Where sway has the security-context
# protocol (Ubuntu 24.04: 1.9) the zone's client gets fewer globals than the host's
# (no screencopy) and its window opens; Debian 12's sway 1.7 has none (checked above, Weston)
cat >/tmp/desktop.sh <<'SH'
set -eu
export WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1
printf 'output HEADLESS-1 resolution 1280x800\n' >/tmp/sway-alice.conf
sway -c /tmp/sway-alice.conf >/tmp/sway-alice.log 2>&1 &
sway=$!
# both sockets: sway makes its Wayland socket before its IPC socket
W= I=
for _ in $(seq 50); do
  W=$(ls /run/user/1000 | grep -x 'wayland-[0-8]' | head -1) || true
  I=$(ls /run/user/1000/sway-ipc.*.sock 2>/dev/null | head -1) || true
  [ -n "$W" ] && [ -n "$I" ] && break; sleep 0.2
done
[ -n "$W" ] && [ -n "$I" ] || { echo "sway did not start: $(tail -3 /tmp/sway-alice.log)"; exit 1; }
export WAYLAND_DISPLAY=$W SWAYSOCK=$I
WAYLAND_DISPLAY=$W wayland-info >/tmp/wl-host.txt 2>&1
if ! grep -q wp_security_context_manager_v1 /tmp/wl-host.txt; then echo "$(sway --version)"; exit 3; fi
appzones run --wait off -- wayland-info </dev/null >/tmp/wl-zone.txt 2>&1 || { cat /tmp/wl-zone.txt; exit 1; }
h=$(grep -c "interface:" /tmp/wl-host.txt); z=$(grep -c "interface:" /tmp/wl-zone.txt)
echo "globals: host $h, zone $z"
grep -q zwlr_screencopy_manager_v1 /tmp/wl-host.txt || { echo "the host has no screencopy?"; exit 1; }
if grep -q -e zwlr_screencopy_manager_v1 -e zwlr_data_control_manager_v1 /tmp/wl-zone.txt; then
  echo "the zone's client sees screencopy or data-control"; exit 1
fi
appzones run off -- foot </dev/null >/dev/null 2>&1
for _ in $(seq 50); do swaymsg -t get_tree | grep -q '"app_id": "foot"' && break; sleep 0.2; done
swaymsg -t get_tree | grep -q '"app_id": "foot"' || { echo "no foot window: $(appzones logs off 2>&1 | tail -5)"; exit 1; }
appzones stop off
kill $sway
SH
rc=0; out=$(timeout 120 su alice -s /bin/sh -c "$U sh /tmp/desktop.sh" 2>&1) || rc=$?
echo "$out"
case $rc in
  0) pass "a desktop (headless sway): the zone's client in a security context (no screencopy, no data-control), its window opens";;
  3) pkill -u alice sway || true
     pass "a desktop (headless sway): this sway has no security-context protocol ($out), as Weston above";;
  *) fail "a desktop (headless sway): $out";;
esac

# a server without the desktop's parts: a session bus is set (as over SSH with systemd),
# but xdg-dbus-proxy and dbus-daemon are not there; and with the C zone-wayland also
# WAYLAND_DISPLAY without zone-wayland (the Rust one is zone-init's binary: always there)
mkdir /tmp/hidden && mv /usr/bin/xdg-dbus-proxy /usr/bin/dbus-daemon /tmp/hidden/
U="$U DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus"
if [ $rust = no ]; then
  mv $prefix/lib/appzones/zone-wayland /tmp/hidden/
  U="$U WAYLAND_DISPLAY=wayland-1"
fi
out=$(as_alice 'appzones run --wait off -- sh -c "echo works; echo bus=\$DBUS_SESSION_BUS_ADDRESS wl=\$WAYLAND_DISPLAY" </dev/null' 2>&1)
echo "$out"
echo "$out" | grep -qx works && echo "$out" | grep -qx "bus= wl=" || fail "a zone without the desktop's parts: $out"
log=/home/alice/.local/state/appzones/off.log
grep -q "xdg-dbus-proxy not found" $log || fail "no warning in the log: $(tail -5 $log)"
[ $rust = yes ] || grep -q "zone-wayland not found" $log || fail "no zone-wayland warning: $(tail -5 $log)"
if out=$(as_alice 'appzones run --wait --bus zone off -- true </dev/null' 2>&1); then fail "bus = zone without dbus-daemon: $out"; fi
echo "$out" | grep -q "no dbus-daemon" || fail "bus = zone without dbus-daemon, not said: $out"
as_alice 'appzones stop off'
[ $rust = yes ] || mv /tmp/hidden/zone-wayland $prefix/lib/appzones/
mv /tmp/hidden/xdg-dbus-proxy /tmp/hidden/dbus-daemon /usr/bin/
pass "a server without zone-wayland, xdg-dbus-proxy, dbus-daemon: the zone starts, says why, bus = zone says what is missing"

if [ -n "$deb" ]; then
  dpkg --purge appzones >/dev/null 2>&1 || fail "dpkg --purge"
else
  make uninstall $mflags >/dev/null 2>&1 || fail "make uninstall"
fi
left=$(ls -d $prefix/lib/appzones $prefix/bin/appzones $prefix/share/appzones /etc/sudoers.d/appzones \
  /etc/apparmor.d/appzones $prefix/lib/systemd/user/appzones@.service 2>/dev/null || true)
[ -z "$left" ] || fail "left after uninstall: $left"
pass "uninstall removes it all"
echo "ALL PASSED"
