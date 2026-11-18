# Generic Linux binaries (`nix_ld`)

A binary built for other distributions (a release download, an agent
CLI) asks for the loader `/lib64/ld-linux-x86-64.so.2`. On a NixOS host
with nix-ld that path is nix-ld's shim, which runs the binary with the loader and libraries
from `NIX_LD` and `NIX_LD_LIBRARY_PATH`. A zone has no `/lib64` and starts
with a clean environment, so the same binary fails there with "cannot
execute: required file not found". With `nix_ld = true` the zone gets
`/lib64/ld-linux-x86-64.so.2` (a link to the host shim in `/nix/store`) and
the two variables, pointing at `/run/current-system/sw/share/nix-ld/lib`.
The libraries are the host's `programs.nix-ld.libraries`. Nothing else is
shared: no `LD_LIBRARY_PATH`, which stays on the env never-list. The
example configuration turns it on in `work` and keeps it off in
`untrusted`.
