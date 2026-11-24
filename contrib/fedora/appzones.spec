# The RPM of appzones for Fedora (`just test-fedora` builds it from the checkout and tests
# it; `just release-rpm VERSION` attaches it to a release). One Rust binary and the mount
# helper, built with Fedora's cargo (the crates come from crates.io, pinned by Cargo.lock).
Name:           appzones
Version:        0.14.0
Release:        1%{?dist}
Summary:        Long-lived sandboxes for desktop apps, dev tools and AI agents
License:        MIT
URL:            https://github.com/mirql/appzones
Source0:        https://github.com/mirql/appzones/archive/refs/tags/v%{version}/%{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust >= 1.85
BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  systemd-rpm-macros
# what a server needs; the desktop's parts are optional (a zone starts without them)
Requires:       bubblewrap
Requires:       passt
Requires:       nftables
Requires:       iproute
Recommends:     sudo
Recommends:     xdg-dbus-proxy
Recommends:     dbus-daemon

# the binaries are stripped at build (helper/Cargo.toml): no debuginfo package
%global debug_package %{nil}

%description
An app zone is a place apps live in, with its own home, its own network rules and its
own window border colour, declared in one TOML file. Built on bubblewrap, pasta (passt)
and nftables. Members of the group appzones may share folders with running zones
(appzones mount, through a sudo rule for exactly its helper).

%prep
%autosetup

%build
make build
test -x build/appzones-helper && test -x build/appzones-mount-helper

%install
# in %{_libexecdir} (bin_t): pasta, in its SELinux domain, may execute only such files
make install DESTDIR=%{buildroot} PREFIX=/usr LIBDIR=%{_libexecdir}/appzones SYSTEMD=yes APPARMOR=no SUDOERS=yes \
  MOUNT_USERS=%%appzones
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
%{_libexecdir}/appzones/
%{_datadir}/appzones/
%{_userunitdir}/appzones@.service
%{_userunitdir}/appzones-autostart.service
%{_userunitdir}/appzones.slice
%{_sysusersdir}/appzones.conf
%config(noreplace) %attr(0440,root,root) %{_sysconfdir}/sudoers.d/appzones
