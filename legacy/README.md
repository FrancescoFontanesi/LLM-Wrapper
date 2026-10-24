# Legacy Python implementation (reference only)

`app.py` and the `llm-wrapper` bash launcher are the original GTK 3 / WebKit2GTK 4.1 wrapper. They
are **retired**: nothing installs them and nothing new is built on them.

They stay in the repository because the Rust implementation was specified against them and is still
checked against them:

* the end-to-end suite can run against this app (`rust/scripts/e2e.sh`, no `--impl`; needs
  `python3-gi`, `gir1.2-gtk-3.0`, `gir1.2-webkit2-4.1`) — that is how the "known defects" of
  `docs/spec.md` section 4 are kept honest;
* `rust/crates/*/tests/fixtures/gen_legacy_*.py` lift the navigation rule and the command-line
  grammar out of `app.py` to produce the committed JSON fixtures the Rust unit tests compare against.

To use the app, see the top-level `README.md`. These files can be deleted the day nobody needs to
regenerate the fixtures or re-run the comparison.
