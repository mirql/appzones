# What holds the repository together, beside the code: contrib/hyprland/zones.lua (the
# pre-0.6 name) is appzones.lua's code past each file's header comment; docs/_data/llms.yml
# (llms.txt, llms-full.txt) names every guide page, and only pages that exist; the shell
# scripts parse.
{ pkgs }:
pkgs.runCommand "appzones-repo"
  {
    src = pkgs.lib.cleanSource ../.;
    nativeBuildInputs = [ pkgs.diffutils ];
  }
  ''
    cd $src
    code() { awk 'body || !/^--/ { body = 1; print }' "$1"; }
    diff <(code contrib/hyprland/zones.lua) <(code contrib/hyprland/appzones.lua) \
      || { echo "zones.lua is not appzones.lua's code"; exit 1; }
    if [ -f docs/index.md ]; then
      (cd docs && find . -name '*.md' | sed 's|^\./||' | grep -vx -e index.md -e guide.md | sort) > $TMPDIR/pages
      sed -n 's/^    - file:[[:space:]]*\([^#]*[^#[:space:]]\)[[:space:]]*\(#.*\)\{0,1\}$/\1/p' docs/_data/llms.yml \
        | tr -d "\"'" | sort > $TMPDIR/listed
      diff $TMPDIR/pages $TMPDIR/listed \
        || { echo "docs/_data/llms.yml and the docs pages differ (<: not listed, >: no such page)"; exit 1; }
    fi
    for f in contrib/sway/appzones-sway contrib/debian/build-deb.sh contrib/debian/postinst contrib/debian/postrm \
             contrib/debian/apparmor-profile tests/distro-test.sh tests/cloud-test.sh; do
      [ ! -f "$f" ] || sh -n "$f" || { echo "$f does not parse"; exit 1; }
    done
    touch $out
  ''
