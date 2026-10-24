# Packaging

| What | Where | Status |
|---|---|---|
| Build / install / uninstall / tarball | `Makefile` (repository root) | `make build`, `make install DESTDIR=… PREFIX=…`, `make uninstall`, `make dist` exercised; the end-to-end suite (without the slow scenarios) was run against the installed files, and a daemon was started and stopped through a symlink to the installed client |
| Desktop entry | `packaging/llm-wrapper.desktop` | installed by `make install`; **not** checked with `desktop-file-validate` (not available here) |
| Arch Linux | `packaging/arch/PKGBUILD` (`llm-wrapper-git`) | syntax checked only: **never run with `makepkg`** (this was built on Ubuntu) |

Runtime dependencies: GTK 4, WebKitGTK 6.0, `bubblewrap` and `xdg-dbus-proxy` (WebKit sandboxes its web
process with them), and — recommended — `xdg-desktop-portal` for the desktop's own file dialogs.

| Distribution | Build dependencies | Runtime |
|---|---|---|
| Arch | `rust` `gtk4` `webkitgtk-6.0` `make` | `gtk4` `webkitgtk-6.0` `bubblewrap` `xdg-dbus-proxy` |
| Debian / Ubuntu (24.04) | `rustc`/`cargo` 1.89 or newer (rustup), `libgtk-4-dev` `libwebkitgtk-6.0-dev` `make` | `libgtk-4-1` `libwebkitgtk-6.0-4` `bubblewrap` `xdg-dbus-proxy` |

Not provided, on purpose: a Flatpak (the hotkey client has to reach the daemon's socket in the host's
runtime directory and start it from the host), `.deb`/`.rpm` recipes (no way to test them here), and CI.
