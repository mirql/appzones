# Golden files: what the Python implementation answered

appzones was written in Python until 0.11; the Rust helper replaced it (design/rust.md,
"Everything in Rust"). Before the Python was removed, its answers were captured here,
so the Rust code is still held to them (`tests/golden.rs`, run in the helper's Nix build):

| file | what | cases |
|---|---|---|
| `config.json` | every configuration the Python unit tests loaded, and `examples/*.toml`: errors, warnings, every zone's settings and origins, `settings_hash` | 937 (23 file trees) |
| `plan.json` | `make_plan` for three zones that use most settings, on both layouts: `plan.json`, every file written, what it printed | 6 |
| `policy.json` | the never-list for runtime mounts, and its answer for 169 paths | 169 |

`capture.py` made them, with `capture.nix` (a Nix build, so the file system the answers
depend on is the one the test runs in), from the last tree with the Python sources
(rust/stage4-one-binary, before "remove the Python"). Each case keeps the file tree it
saw; `{T}` stands for its root. To change an answer on purpose, change the JSON here, in
the same commit as the code, and say why.
