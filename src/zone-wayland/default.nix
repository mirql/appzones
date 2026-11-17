# zone-wayland: a per-zone wp_security_context_v1 listener (zone-wayland.c).
{
  stdenv,
  wayland,
  wayland-scanner,
  wayland-protocols,
  pkg-config,
}:

stdenv.mkDerivation {
  pname = "zone-wayland";
  version = "1.0.0";
  src = ./.;
  nativeBuildInputs = [
    wayland-scanner
    pkg-config
  ];
  buildInputs = [ wayland ];
  buildPhase = ''
    xml=${wayland-protocols}/share/wayland-protocols/staging/security-context/security-context-v1.xml
    wayland-scanner client-header $xml security-context-v1-client-protocol.h
    wayland-scanner private-code $xml security-context-v1-protocol.c
    $CC -O2 -Wall -Werror -o zone-wayland zone-wayland.c security-context-v1-protocol.c \
      $(pkg-config --cflags --libs wayland-client)
  '';
  installPhase = "install -Dm755 zone-wayland $out/bin/zone-wayland";
  meta.mainProgram = "zone-wayland";
}
