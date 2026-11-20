# The RPM of appzones for Fedora (`just test-fedora` builds it from the checkout and tests
# it; `just release-rpm VERSION` attaches it to a release). Python and the C zone-wayland
# only, like the .deb: the optional Rust helpers are not packaged (yet, #121).
Name:           appzones
Version:        0.11.1
Release:        1%{?dist}
Summary:        Long-lived sandboxes for desktop apps, dev tools and AI agents
License:        MIT
URL:            https://github.com/mirql/appzones
Source0:        https://github.com/mirql/appzones/archive/refs/tags/v%{version}/%{name}-%{version}.tar.gz

BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  pkgconf-pkg-config
BuildRequires:  wayland-devel
BuildRequires:  python3
BuildRequires:  systemd-rpm-macros
# what a server needs; the desktop's parts are optional (a zone starts without them)
Requires:       python3 >= 3.11
Requires:       bubblewrap
Requires:       passt
Requires:       nftables
Requires:       iproute
Recommends:     sudo
Recommends:     xdg-dbus-proxy
Recommends:     dbus-daemon

# zone-wayland is small; no separate debuginfo package
%global debug_package %{nil}

%description
An app zone is a place apps live in, with its own home, its own network rules and its
own window border colour, declared in one TOML file. Built on bubblewrap, pasta (passt)
and nftables. Members of the group appzones may share folders with running zones
(appzones mount, through a sudo rule for exactly its helper).

%prep
%autosetup

%build
# the C helper only: the same package whether or not cargo is installed
%make_build zone-wayland CFLAGS="%{optflags}" LDFLAGS="%{build_ldflags}"
test -f build/zone-wayland

%install
make -o helper install DESTDIR=%{buildroot} PREFIX=/usr PYTHON=/usr/bin/python3 HELPER= \
  HELPER_BIN=build/none SYSTEMD=yes APPARMOR=no SUDOERS=yes MOUNT_USERS=%%appzones
# the sudo rule names this group
install -Dm644 contrib/arch/appzones.sysusers %{buildroot}%{_sysusersdir}/appzones.conf

%post
cat <<'MSG'
appzones: zones start on demand, or through the systemd user units:
  systemctl --user daemon-reload
Write ~/.config/appzones/zones.toml (see /usr/share/appzones/zones.minimal.toml),
then `appzones check`. For `appzones mount`, join the appzones group:
  sudo usermod -aG appzones "$USER"   (then log in again)
MSG

%files
%license LICENSE
%{_bindir}/appzones
%{_prefix}/lib/appzones/
%{_datadir}/appzones/
%{_userunitdir}/appzones@.service
%{_userunitdir}/appzones-autostart.service
%{_userunitdir}/appzones.slice
%{_sysusersdir}/appzones.conf
%config(noreplace) %attr(0440,root,root) %{_sysconfdir}/sudoers.d/appzones
