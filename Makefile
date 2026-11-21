# appzones outside Nix (Ubuntu, Debian, Fedora, Arch, ...). On NixOS use the flake's
# package and module instead.
#
#   make                  build appzones-helper and appzones-mount-helper (cargo, rustc 1.85
#                         or newer), unless HELPER_BIN and MOUNT_HELPER_BIN name built ones
#   sudo make install     install under PREFIX (/usr/local), the sudo rule for
#                         `appzones mount`, the AppArmor profile where the kernel
#                         restricts user namespaces (Ubuntu 23.10+), the systemd
#                         user units where there is systemd
#   sudo make uninstall   remove all of it
#
# Variables: PREFIX (/usr/local; anywhere: appzones binds its one binary into each zone),
# LIBDIR (PREFIX/libexec/appzones), DESTDIR (staging for packages), HELPER_BIN and MOUNT_HELPER_BIN (built binaries, as a
# package's: nix/portable.nix), MOUNT_USERS (who may run the mount helper: a user, or
# %group; default: the user who ran sudo, else %users), SUDOERS=no, APPARMOR=no, SYSTEMD=no.

PREFIX ?= /usr/local
DESTDIR ?=
# the binaries: in libexec, labelled bin_t where SELinux runs (pasta may exec only such files)
LIBDIR ?= $(PREFIX)/libexec/appzones
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
# appzones itself (every command, the server, zone-init, zone-wayland) and the root step
# of `appzones mount`: built here by default
HELPER_BIN ?= build/appzones-helper
MOUNT_HELPER_BIN ?= build/appzones-mount-helper
VERSION = $(shell sed -n 's/^  version = "\(.*\)";/\1/p' nix/package.nix)

USE_APPARMOR = $(shell [ "$(APPARMOR)" = yes ] || { [ "$(APPARMOR)" = auto ] && [ "$$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null)" = 1 ]; } && echo yes)
USE_SYSTEMD = $(shell [ "$(SYSTEMD)" = yes ] || { [ "$(SYSTEMD)" = auto ] && [ -d /run/systemd/system ]; } && echo yes)

.PHONY: all install uninstall preflight clean build

all: build

# cargo builds both, unless they are given; `sudo make install` after `make` uses what
# `make` built (root often has no cargo)
build:
	@mkdir -p build
	@if [ "$(HELPER_BIN)" != build/appzones-helper ] || [ "$(MOUNT_HELPER_BIN)" != build/appzones-mount-helper ]; then \
		echo "using $(HELPER_BIN) and $(MOUNT_HELPER_BIN)"; \
	elif command -v cargo >/dev/null && cargo build --release --locked --quiet --manifest-path helper/Cargo.toml \
		--target-dir build/cargo; then \
		cp build/cargo/release/appzones-helper build/cargo/release/appzones-mount-helper build/ && \
		echo "built build/appzones-helper and build/appzones-mount-helper"; \
	elif [ -f build/appzones-helper ] && [ -f build/appzones-mount-helper ]; then \
		echo "using build/appzones-helper, built before (cargo is missing or failed here, as under sudo)"; \
	else \
		echo "appzones needs cargo and rustc 1.85 or newer to build (or HELPER_BIN and MOUNT_HELPER_BIN)"; exit 1; \
	fi

# before anything is copied
preflight: build
	@for b in "$(HELPER_BIN)" "$(MOUNT_HELPER_BIN)"; do [ -f "$$b" ] || { echo "$$b: not there"; exit 1; }; done
	@# (where it runs here: a package's binary may be built for other systems than this one)
	@if out=$$("$(realpath $(HELPER_BIN))" --commands 2>/dev/null); then echo "$$out" | grep -qx cli \
		|| { echo "$(HELPER_BIN) is not appzones' helper (appzones-helper --commands has no cli)"; exit 1; }; fi
	@# absolute and canonical (no . or .. or //), so the sudo rule's checks see where it really is
	@case "$(PREFIX)" in /*) ;; *) echo "PREFIX must be an absolute path (not $(PREFIX))"; exit 1;; esac
	@[ "$(PREFIX)" = "$(abspath $(PREFIX))" ] || { echo "PREFIX must be a plain path, without . or .. ($(PREFIX))"; exit 1; }
	@[ "$(USE_APPARMOR)" != yes ] || { command -v bwrap >/dev/null && command -v pasta >/dev/null; } \
		|| { echo "install bubblewrap and passt first: the AppArmor profile names them (or APPARMOR=no)"; exit 1; }

install: all preflight
	install -d $(DESTDIR)$(LIBDIR) $(DESTDIR)$(BINDIR) $(DESTDIR)$(DATADIR)
	install -m 755 "$(HELPER_BIN)" $(DESTDIR)$(LIBDIR)/appzones-helper
	@# the root step of `appzones mount` (the sudo rule names this file)
	install -m 555 "$(MOUNT_HELPER_BIN)" $(DESTDIR)$(LIBDIR)/appzones-mount-helper
	printf '#!/bin/sh\nexec %s/appzones-helper cli "$$@"\n' "$(LIBDIR)" > build/appzones
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
