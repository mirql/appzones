# appzones outside Nix (Ubuntu, Debian, Arch, ...). On NixOS use the flake's
# package and module instead.
#
#   make                  build zone-wayland (needs a C compiler, pkg-config,
#                         libwayland-dev and wayland-scanner; skipped with a
#                         note when they are missing: zones then get no Wayland),
#                         and the Rust helpers where cargo is present
#   sudo make install HELPER=rust   the same with the Rust helpers (design/rust.md)
#   sudo make install     install under PREFIX (/usr/local), the sudo rule for
#                         `appzones mount`, the AppArmor profile where the kernel
#                         restricts user namespaces (Ubuntu 23.10+), the systemd
#                         user units where there is systemd
#   sudo make uninstall   remove all of it
#
# Variables: PREFIX (/usr/local; under /usr, which every zone sees, unless HELPER=rust),
# DESTDIR (staging for packages), PYTHON (python3, 3.11 or newer),
# MOUNT_USERS (who may run the mount helper: a user, or %group; default: the
# user who ran sudo, else %users), SUDOERS=no, APPARMOR=no, SYSTEMD=no.

PREFIX ?= /usr/local
DESTDIR ?=
PYTHON ?= $(shell command -v python3)
LIBDIR = $(PREFIX)/lib/appzones
BINDIR = $(PREFIX)/bin
DATADIR = $(PREFIX)/share/appzones
# where systemd looks for user units: /usr/lib for PREFIX=/usr, else /usr/local/lib
UNITDIR ?= $(if $(filter /usr,$(PREFIX)),/usr/lib/systemd/user,/usr/local/lib/systemd/user)
SUDOERSDIR = /etc/sudoers.d
APPARMORDIR = /etc/apparmor.d
MOUNT_USERS ?= $(if $(SUDO_USER),$(SUDO_USER),%users)
SUDOERS ?= yes
APPARMOR ?= auto
SYSTEMD ?= auto
# rust: appzones runs the Rust helper (build/appzones-helper, or HELPER_BIN=FILE, a path) for
# what it has replaced so far (design/rust.md); empty: the Python and C ones
HELPER ?=
HELPER_BIN ?= build/appzones-helper
# the Rust root step of `appzones mount`, with HELPER=rust when it is there (else the Python one)
MOUNT_HELPER_BIN ?= build/appzones-mount-helper-rust
VERSION = $(shell sed -n 's/^  version = "\(.*\)";/\1/p' nix/package.nix)
CC ?= cc
# a distribution's own flags (hardening) when it sets them; -Werror only in our default
CFLAGS ?= -O2 -Wall -Werror
# the protocol (from wayland-protocols 1.32+; a copy here, since Debian 12 has 1.31)
WAYLAND_XML = src/zone-wayland/security-context-v1.xml

USE_APPARMOR = $(shell [ "$(APPARMOR)" = yes ] || { [ "$(APPARMOR)" = auto ] && [ "$$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null)" = 1 ]; } && echo yes)
USE_SYSTEMD = $(shell [ "$(SYSTEMD)" = yes ] || { [ "$(SYSTEMD)" = auto ] && [ -d /run/systemd/system ]; } && echo yes)

.PHONY: all install uninstall preflight clean zone-wayland helper

all: zone-wayland helper

# the Rust helpers (helper/, design/rust.md): built where cargo is present and new
# enough, else a note; used only with `make install HELPER=rust`
helper:
	@mkdir -p build
	@if command -v cargo >/dev/null && cargo build --release --locked --quiet --manifest-path helper/Cargo.toml \
		--target-dir build/cargo 2>build/helper.log; then \
		cp build/cargo/release/appzones-helper build/appzones-helper && echo "built build/appzones-helper"; \
		cp build/cargo/release/appzones-mount-helper build/appzones-mount-helper-rust; \
	elif [ -f build/appzones-helper ]; then \
		echo "using build/appzones-helper, built before (cargo is missing or failed here, as under sudo)"; \
	else \
		command -v cargo >/dev/null || echo "cargo: not found" >build/helper.log; \
		echo "note: appzones-helper not built (needs cargo and rustc 1.85 or newer; see build/helper.log)"; \
	fi

# always tried again (cheap): after a source change, or once its build dependencies are installed
zone-wayland:
	@mkdir -p build
	@if pkg-config --exists wayland-client 2>/dev/null && command -v wayland-scanner >/dev/null; then \
		wayland-scanner client-header "$(WAYLAND_XML)" build/security-context-v1-client-protocol.h && \
		wayland-scanner private-code "$(WAYLAND_XML)" build/security-context-v1-protocol.c && \
		$(CC) $(CPPFLAGS) $(CFLAGS) -Ibuild -o build/zone-wayland src/zone-wayland/zone-wayland.c \
			build/security-context-v1-protocol.c $$(pkg-config --cflags --libs wayland-client) $(LDFLAGS) && \
		echo "built build/zone-wayland"; \
	else \
		rm -f build/zone-wayland; \
		echo "note: zone-wayland not built (needs a C compiler, pkg-config, libwayland-dev, wayland-scanner):"; \
		echo "      zones will start without a Wayland socket; command-line use works"; \
	fi

# before anything is copied
preflight: $(if $(filter rust,$(HELPER)),helper)
	@[ "$(HELPER)" != rust ] || { [ -f "$(HELPER_BIN)" ] && "$(realpath $(HELPER_BIN))" --commands 2>/dev/null | grep -qx zone-wayland; } \
		|| { echo "HELPER=rust needs a Rust helper that implements zone-wayland (appzones-helper --commands):"; \
		     echo "  cargo for \`make\`, or HELPER_BIN=PATH"; exit 1; }
	@[ -n "$(PYTHON)" ] && "$(PYTHON)" -c 'import sys; sys.exit(sys.version_info < (3, 11))' \
		|| { echo "appzones needs Python 3.11 or newer (PYTHON=$(PYTHON))"; exit 1; }
	@# absolute and canonical (no . or .. or //), so the /usr checks below see where it really is
	@case "$(PREFIX)" in /*) ;; *) echo "PREFIX must be an absolute path (not $(PREFIX))"; exit 1;; esac
	@[ "$(PREFIX)" = "$(abspath $(PREFIX))" ] || { echo "PREFIX must be a plain path, without . or .. ($(PREFIX))"; exit 1; }
	@case "$(PREFIX)" in /usr|/usr/*) ;; *) [ "$(HELPER)" != rust ] || \
		{ "$(realpath $(HELPER_BIN))" --commands | grep -qx zone-init && "$(realpath $(HELPER_BIN))" --commands \
		  | grep -qx open-link; } || { echo "PREFIX outside /usr needs a Rust helper with zone-init and open-link"; \
		  exit 1; };; esac
	@case "$(PREFIX)" in /usr|/usr/*) ;; *) [ "$(HELPER)" = rust ] \
		|| { echo "PREFIX must be under /usr (a zone sees /usr, not $(PREFIX)), or HELPER=rust (the zone then needs"; \
		     echo "  only the helper, which appzones binds into it)"; exit 1; };; esac
	@[ "$(USE_APPARMOR)" != yes ] || { command -v bwrap >/dev/null && command -v pasta >/dev/null; } \
		|| { echo "install bubblewrap and passt first: the AppArmor profile names them (or APPARMOR=no)"; exit 1; }

install: all preflight
	install -d $(DESTDIR)$(LIBDIR)/appzones $(DESTDIR)$(BINDIR) $(DESTDIR)$(DATADIR)
	install -m 644 src/zone_init.py src/zonecfg.py $(DESTDIR)$(LIBDIR)/
	sed 's/@version@/$(VERSION)/' src/zone.py > build/zone.py
	install -m 644 build/zone.py $(DESTDIR)$(LIBDIR)/zone.py
	install -m 644 src/appzones/*.py $(DESTDIR)$(LIBDIR)/appzones/
	@# the root step of `appzones mount`: the Rust one with HELPER=rust, else isolated Python (-I)
	if [ "$(HELPER)" = rust ] && [ -f "$(MOUNT_HELPER_BIN)" ]; then \
		install -m 555 "$(MOUNT_HELPER_BIN)" $(DESTDIR)$(LIBDIR)/appzones-mount-helper; \
	else \
		{ echo "#!$(PYTHON) -IB"; cat src/zone_mount_helper.py; } > build/appzones-mount-helper && \
		install -m 555 build/appzones-mount-helper $(DESTDIR)$(LIBDIR)/appzones-mount-helper; \
	fi
	[ ! -f build/zone-wayland ] || install -m 755 build/zone-wayland $(DESTDIR)$(LIBDIR)/zone-wayland
	[ ! -f $(HELPER_BIN) ] || install -m 755 $(HELPER_BIN) $(DESTDIR)$(LIBDIR)/appzones-helper
	@# with the Rust helper, `appzones` is the helper's own command line (no Python runs)
	if [ "$(HELPER)" = rust ]; then \
		printf '#!/bin/sh\nAPPZONES_HELPER=rust export APPZONES_HELPER; exec %s/appzones-helper cli "$$@"\n' "$(LIBDIR)" > build/appzones; \
	else \
		printf '#!/bin/sh\nexec %s -B %s/zone.py "$$@"\n' "$(PYTHON)" "$(LIBDIR)" > build/appzones; \
	fi
	install -m 755 build/appzones $(DESTDIR)$(BINDIR)/appzones
	install -m 644 examples/zones.toml $(DESTDIR)$(DATADIR)/zones.example.toml
	install -m 644 examples/minimal.toml $(DESTDIR)$(DATADIR)/zones.minimal.toml
	@# never writable by others, whatever the checkout's modes (appzones-sway is run from a sway
	@# config): copied into a directory only we can enter, made so there, then moved into place
	rm -rf $(DESTDIR)$(DATADIR)/contrib $(DESTDIR)$(DATADIR)/.contrib.new
	mkdir -m 700 $(DESTDIR)$(DATADIR)/.contrib.new
	cp -r contrib/. $(DESTDIR)$(DATADIR)/.contrib.new/
	chmod -R go-w $(DESTDIR)$(DATADIR)/.contrib.new && chmod 755 $(DESTDIR)$(DATADIR)/.contrib.new
	mv $(DESTDIR)$(DATADIR)/.contrib.new $(DESTDIR)$(DATADIR)/contrib
	@if [ "$(USE_SYSTEMD)" = yes ]; then \
		install -d $(DESTDIR)$(UNITDIR) && \
		for u in appzones@.service appzones-autostart.service appzones.slice; do \
			sed 's|^ExecStart=appzones |ExecStart=$(BINDIR)/appzones |' contrib/systemd/$$u > build/$$u && \
			install -m 644 build/$$u $(DESTDIR)$(UNITDIR)/$$u || exit 1; \
		done && \
		echo "systemd user units in $(UNITDIR) (each user: systemctl --user daemon-reload)"; \
	fi
	@# the sudo rule names a file root runs: outside /usr only when that file and every
	@# directory above it belong to root and nobody else can write them (a real install)
	@if [ "$(SUDOERS)" = yes ] && case "$(PREFIX)" in /usr|/usr/*) false;; esac; then \
		safe=no; \
		if [ -z "$(DESTDIR)" ]; then \
			safe=yes; d="$(LIBDIR)/appzones-mount-helper"; \
			while :; do \
				set -- $$(stat -c '%u %a' "$$d" 2>/dev/null || echo "x 777"); \
				{ [ "$$1" = 0 ] && [ $$(( 0$$2 & 022 )) = 0 ]; } || safe=no; \
				[ "$$d" = / ] && break; d=$$(dirname "$$d"); \
			done; \
		fi; \
		if [ $$safe = no ]; then \
			echo "no sudo rule: $(LIBDIR) is outside /usr and not owned by root alone (appzones mount needs one)"; \
			exit 0; \
		fi; \
	fi; \
	if [ "$(SUDOERS)" = yes ]; then \
		install -d -m 750 $(DESTDIR)$(SUDOERSDIR) && \
		printf '# appzones mount: exactly the mount helper, as root (it checks its arguments itself)\n%s ALL=(root) NOPASSWD: %s/appzones-mount-helper\n' \
			"$(MOUNT_USERS)" "$(LIBDIR)" > $(DESTDIR)$(SUDOERSDIR)/appzones.tmp && \
		chmod 440 $(DESTDIR)$(SUDOERSDIR)/appzones.tmp && \
		{ ! command -v visudo >/dev/null || visudo -cqf $(DESTDIR)$(SUDOERSDIR)/appzones.tmp; } && \
		mv $(DESTDIR)$(SUDOERSDIR)/appzones.tmp $(DESTDIR)$(SUDOERSDIR)/appzones && \
		echo "sudo rule for appzones mount: $(MOUNT_USERS) ($(SUDOERSDIR)/appzones)"; \
	fi
	@if [ "$(USE_APPARMOR)" = yes ]; then \
		bwrap=$$(command -v bwrap); pasta=$$(command -v pasta); \
		install -d $(DESTDIR)$(APPARMORDIR) && \
		sed -e "s|@BWRAP@|$$(readlink -f $$bwrap)|" -e "s|@PASTA@|$$(readlink -f $$pasta)|" contrib/apparmor/appzones \
			> $(DESTDIR)$(APPARMORDIR)/appzones && chmod 644 $(DESTDIR)$(APPARMORDIR)/appzones && \
		echo "AppArmor profile $(APPARMORDIR)/appzones: bwrap and pasta may create user namespaces" && \
		if [ -z "$(DESTDIR)" ] && [ -d /sys/kernel/security/apparmor ] && command -v apparmor_parser >/dev/null; then \
			apparmor_parser -r $(APPARMORDIR)/appzones; \
		else echo "  (not loaded: AppArmor is not active here; it loads at the next boot where it is)"; fi; \
	fi
	@echo "installed appzones $(VERSION) in $(PREFIX)"

uninstall:
	rm -rf $(DESTDIR)$(LIBDIR) $(DESTDIR)$(DATADIR)
	rm -f $(DESTDIR)$(BINDIR)/appzones
	rm -f $(DESTDIR)$(UNITDIR)/appzones@.service $(DESTDIR)$(UNITDIR)/appzones-autostart.service $(DESTDIR)$(UNITDIR)/appzones.slice
	rm -f $(DESTDIR)$(SUDOERSDIR)/appzones
	@if [ -f $(DESTDIR)$(APPARMORDIR)/appzones ]; then \
		if [ -z "$(DESTDIR)" ] && [ -d /sys/kernel/security/apparmor ] && command -v apparmor_parser >/dev/null; then \
			apparmor_parser -R $(APPARMORDIR)/appzones || true; fi; \
		rm -f $(DESTDIR)$(APPARMORDIR)/appzones; \
	fi

clean:
	rm -rf build
