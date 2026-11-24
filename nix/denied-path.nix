# The type of a deniedPaths entry (nix/module.nix, nix/darwin-module.nix): only what
# /etc/appzones/denied-paths gives back unchanged. appzones reads that file a line per path
# (any line break Python's splitlines knows), # starts a comment, and each line is stripped
# of whitespace at both ends (Unicode's too): an entry that would come back changed would
# protect another path, so it is refused here.
lib:
let
  u = c: builtins.fromJSON ''"\u${c}"'';
  # the line breaks beyond ASCII control characters, and the whitespace beyond ASCII's
  # (char::is_whitespace): never in an entry
  unicode = map u [
    "0085"
    "00a0"
    "1680"
    "2000"
    "2001"
    "2002"
    "2003"
    "2004"
    "2005"
    "2006"
    "2007"
    "2008"
    "2009"
    "200a"
    "2028"
    "2029"
    "202f"
    "205f"
    "3000"
  ];
in
lib.types.addCheck (lib.types.strMatching "(/|~/)([^#[:cntrl:]]*[^#[:space:][:cntrl:]])?") (
  s: !(builtins.any (c: lib.hasInfix c s) unicode)
)
// {
  description = "an absolute path or ~/..., without #, line breaks or whitespace at its end";
}
