#!/bin/sh
# Zones on macOS, end to end (design/macos.md, stage 4): run on a Mac, with the binary
# this checkout built. Everything lives in a temporary folder of the home (config, state,
# logs, runtime directory), never the user's own zones; the folder is removed at the end.
#
#   sh tests/macos.sh helper/target/release/appzones-helper
#
# Three zones (network none with binds, internet, host): `appzones check --probe` for each
# (every probe must pass), then what the probes cannot see from inside: binds rw/ro/nested,
# a zone's processes ended with it (stop, its server killed, zone-init killed), a refused
# setting refuses its zone alone. Exit status: the number of failures (0: all passed).
set -u
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
[ -x "$BIN" ] || { echo "usage: sh tests/macos.sh PATH-TO-appzones-helper" >&2; exit 2; }
[ "$(uname)" = Darwin ] || { echo "tests/macos.sh runs on macOS" >&2; exit 2; }
T=$(mktemp -d "$HOME/.appzones-macos-test.XXXXXX")
export APPZONES_CONFIG="$T/zones.toml" APPZONES_STATE="$T/state" XDG_RUNTIME_DIR="$T/rt" XDG_STATE_HOME="$T/log"
mkdir -p "$T/rt" "$T/proj/vendor" "$T/docs"
echo doc > "$T/docs/readme"
az() { "$BIN" cli "$@"; }
fails=0
ok() { echo "PASS  $1"; }
no() { echo "FAIL  $1"; fails=$((fails + 1)); }
check() { if eval "$2" >/dev/null 2>&1; then ok "$1"; else no "$1"; fi; }
# this run's processes only: a server or zone-init of this binary whose environment names
# this run's config (never another zone of the user's, never this shell)
ours() {
    for p in $(pgrep -f "$1"); do
        [ "$p" = $$ ] && continue
        ps -E -o command= -p "$p" 2>/dev/null | grep -q -F "APPZONES_CONFIG=$APPZONES_CONFIG" && echo "$p"
    done
}
servers() { ours "$BIN cli serve $1"; }
# zone-init's environment is the zone's (no APPZONES_CONFIG): it is ours when its parent is
# one of our servers, or it was (an orphan, its server killed) and it holds our runtime dir
inits() {
    for p in $(pgrep -f "$BIN zone-init"); do
        pp=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' ')
        if servers "" | grep -q -x "$pp" || lsof -a -p "$p" -d cwd,0-20 2>/dev/null | grep -q -F "$T"; then echo "$p"; fi
    done
}
cleanup() {
    az stop --all >/dev/null 2>&1
    for p in $(servers "") $(inits); do kill -9 "$p" 2>/dev/null; done
    rm -rf "$T"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cat > "$APPZONES_CONFIG" <<EOF
version = 1
requires = 7
[zones.t-none]
colour = "#336699"
network = "none"
binds = [{ path = "$T/proj/vendor", mode = "ro" }, { path = "$T/proj", mode = "rw" }, { path = "$T/docs", mode = "ro" }]
[zones.t-inet]
colour = "#339966"
network = "internet"
[zones.t-host]
colour = "#993366"
network = "host"
[zones.t-allow]
colour = "#996633"
allow = ["example.com"]
EOF
check "the config is valid" 'az check'

echo "--- check --probe"
for z in t-none t-inet t-host t-allow; do
    out=$(az check --probe "$z" 2>&1); rc=$?
    echo "$out" | grep -E '^(FAIL|SKIP)' | sed "s/^/  $z: /"
    if [ $rc -eq 0 ] && ! echo "$out" | grep -q '^FAIL' && echo "$out" | grep -q '^PASS'; then
        ok "every probe passes in $z ($(echo "$out" | grep -c '^PASS') probes)"
    else
        no "the probes in $z (status $rc)"
    fi
done

echo "--- allow"
code() { az run --wait t-allow -- /bin/sh -c "curl -s -m 20 -o /dev/null -w '%{http_code}' $1" </dev/null 2>/dev/null; }
check "a name on the list, through the proxy"  '[ "$(code https://example.com/)" = 200 ] || [ "$(code http://example.com/)" = 200 ]'
check "a name off the list: 403"               '[ "$(code http://example.org/)" = 403 ]'
check "an address: 403"                        '[ "$(code http://1.1.1.1/)" = 403 ]'
check "the refusal is in the zone's log"       'az logs t-allow 2>&1 | grep -q "refused example.org:80 (not on the zone.s allow list)"'

az stop t-inet >/dev/null 2>&1; az stop t-host >/dev/null 2>&1; az stop t-allow >/dev/null 2>&1   # (only t-none runs from here)

echo "--- binds"
inz() { az run --wait t-none -- /bin/sh -c "$1" </dev/null >/dev/null 2>&1; }
check "an rw bind is writable"            "inz 'touch $T/proj/w'"
check "an ro bind is readable"            "inz 'cat $T/docs/readme'"
check "an ro bind is not writable"        "! inz 'touch $T/docs/w'"
check "an ro bind inside an rw one is not writable" "! inz 'touch $T/proj/vendor/w'"
check "beside the binds is closed"        "! inz 'ls $T'"

echo "--- a zone's processes end with it"
az run t-none -- sleep 401 >/dev/null 2>&1
az run t-none -- /bin/sh -c 'sleep 402 & (setsid sleep 403 2>/dev/null || sleep 403) &' >/dev/null 2>&1
sleep 1
check "they run"                          '[ "$(pgrep -f "sleep 40[123]" | wc -l)" -ge 3 ]'
az stop t-none >/dev/null 2>&1; sleep 1
check "stop ends them, detached ones too" '[ -z "$(pgrep -f "sleep 40[123]")" ]'
# each: started and seen running first, so an end that did not happen cannot pass
running() { az up t-none >/dev/null 2>&1 && az run t-none -- sleep "$1" >/dev/null 2>&1 && sleep 0.5 \
            && [ -n "$(pgrep -f "sleep $1")" ] && [ -n "$(servers t-none)" ] && [ -n "$(inits)" ]; }
if running 404; then
    for p in $(servers t-none); do kill -9 "$p"; done; sleep 2
    check "its server killed: the zone ends" '[ -z "$(pgrep -f "sleep 404")" ] && [ -z "$(inits)" ]'
else
    no "its server killed: the zone did not start"
fi
if running 405; then
    for p in $(inits); do kill -9 "$p"; done; sleep 2
    check "zone-init killed: the zone ends" '[ -z "$(pgrep -f "sleep 405")" ] && [ -z "$(servers t-none)" ]'
else
    no "zone-init killed: the zone did not start"
fi

echo "--- refusals"
cat >> "$APPZONES_CONFIG" <<EOF
[zones.t-bad]
colour = "#444444"
dns = "public"
EOF
check "check refuses what macOS cannot give" '! az check'
check "that zone does not start"          '! az up t-bad'
check "the others still do"               'az up t-none'

az stop --all >/dev/null 2>&1
echo "--- $fails failure(s)"
exit "$fails"
