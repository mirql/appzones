# What holds the repository together, beside the code: docs/_data/llms.yml
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
    for f in docs/index.md docs/guide.md docs/_data/llms.yml; do
      [ -f "$f" ] || { echo "$f is missing"; exit 1; }
    done
    (cd docs && find . -name '*.md' | sed 's|^\./||' | grep -vx -e index.md -e guide.md | sort) > $TMPDIR/pages
    sed -n 's/^    - file:[[:space:]]*\([^#]*[^#[:space:]]\)[[:space:]]*\(#.*\)\{0,1\}$/\1/p' docs/_data/llms.yml \
      | tr -d "\"'" | sort > $TMPDIR/listed
    diff $TMPDIR/pages $TMPDIR/listed \
      || { echo "docs/_data/llms.yml and the docs pages differ (<: not listed, >: no such page)"; exit 1; }
    # every one of them is needed (by a package or a test): missing is a failure, too
    for f in contrib/sway/appzones-sway contrib/debian/build-deb.sh contrib/debian/postinst contrib/debian/postrm \
             contrib/debian/apparmor-profile tests/distro-test.sh tests/cloud-test.sh; do
      sh -n "$f" || { echo "$f is missing or does not parse"; exit 1; }
    done
    touch $out
  ''
