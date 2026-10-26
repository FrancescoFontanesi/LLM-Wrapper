# llm-wrapper

A lightweight desktop wrapper for web AI chats: ChatGPT by default, plus Gemini, Claude or any
`https://` address. Built to be summoned from a hotkey (Hyprland, Sway, a Quickshell bar, any
compositor or desktop): one small client command talks to a resident daemon that owns a GTK 4 +
WebKitGTK 6.0 window. Written in Rust.

You can use it in two ways: one daemon with an instant provider switcher, or one standalone
instance per provider.


---

## Highlights

- **Instant summon.** `llm-wrapper --toggle` is a ~2 ms static binary that talks to the running
  daemon over a local Unix socket; the window comes up in about 10 ms.
- **Persistent profile.** Cookies and login state survive restarts.
- **Many providers.** ChatGPT, Gemini, Claude or any URL; switch with `--provider`. Sign-in popups
  work.
- **Offline recovery.** When a page fails to load it retries every 30 s and reloads as soon as the
  network is back.
- **Light on memory.** A hidden window that has been idle for `--idle-timeout` minutes (default 15)
  goes back to the provider's home page in a fresh web process, so a long conversation's memory is
  returned to the system; summoning it again shows the home page at once. Nothing is lost: a page
  with an unsent message, an attached file, a download, a popup, sound or a load in progress is left
  alone. See [Memory](#memory).
- **Files through the desktop.** Uploads and downloads use the system file dialogs (the portal when
  there is one); a page never writes or reads a file you did not pick. Image paste (`Ctrl+V`)
  works.
- **External links** (sources, citations, docs) open in your default browser, on a click only.
- **Security first.** One page cannot open more than four windows, navigate you away, or ask the
  desktop for permissions (all denied); the control socket is private to your user and checked at
  both ends; data directories are `0700`. The reasoning and the proof per control are in
  [`docs/security.md`](docs/security.md).
- **Assistant keys:** `Esc` hides the window (the page never sees it), `Ctrl+N` new chat, `Ctrl+R` /
  `F5` reload (`Ctrl+Shift+R` bypasses the cache), `Ctrl++` / `Ctrl+-` / `Ctrl+0` zoom, `F12` Web
  Inspector. The chat box gets the focus whenever you summon the window.
- **60 fps on power-saver:** the daemon hides the system power profile from WebKit (otherwise it
  halves rendering to 30 fps). Not verified on real power-saver hardware.

---

## Install

### Requirements

At run time: **GTK 4**, **WebKitGTK 6.0**, `bubblewrap` and `xdg-dbus-proxy` (WebKit sandboxes its
web process with them). `xdg-desktop-portal` is recommended for native file dialogs.

To build: Rust **1.89** or newer, the GTK 4 and WebKitGTK 6.0 development packages, and `make`.

| Distribution | Build | Run |
|---|---|---|
| Arch / Garuda | `sudo pacman -S rust gtk4 webkitgtk-6.0 make` | `sudo pacman -S gtk4 webkitgtk-6.0 bubblewrap xdg-dbus-proxy xdg-desktop-portal` |
| Debian / Ubuntu 24.04 | Rust 1.89+ from [rustup](https://rustup.rs), `sudo apt install libgtk-4-dev libwebkitgtk-6.0-dev make` | `sudo apt install libgtk-4-1 libwebkitgtk-6.0-4 bubblewrap xdg-dbus-proxy xdg-desktop-portal` |

Developed and tested with GTK 4.14.5 and WebKitGTK 2.52.6; other versions are unverified.

### Build and install

```bash
make build                           # release build into rust/dist
sudo make install                    # /usr/local/bin, or:
make install PREFIX=$HOME/.local     # for one user, no root (needs ~/.local/bin on PATH)
```

`make install` puts `llm-wrapper` (the command) and `llm-wrapperd` (the daemon) side by side in
`$PREFIX/bin`, plus the desktop entry. **Keep the two together**: the client starts the daemon from
its own directory and from nowhere else (never through `PATH`). A symlink to the client works; the
real file is what counts.

`make uninstall` (same `PREFIX`) removes them. `make dist` makes a tarball of the installed layout.
Arch users can also try `packaging/arch/PKGBUILD` (never run with `makepkg` yet, see
[`packaging/README.md`](packaging/README.md)).

Check the installation: `llm-wrapper --version`, then `llm-wrapper --show`.

---

## Usage

```bash
llm-wrapper                 # toggle the window (starts the daemon the first time)
llm-wrapper [flags]
```

| Flag | Description |
| --- | --- |
| `--toggle` | Show the window if hidden, hide it if shown. |
| `--show` | Show and focus the window. |
| `--hide` | Hide the window (the daemon keeps running). |
| `--new-chat` | Open a clean chat with the active provider. Shows the window. |
| `--soft-clean` | Release the in-memory cache (disk cache and logins stay). |
| `--hard-clean` | Purge disk and DOM caches, keep login cookies. |
| `--background` | Start the daemon with the window hidden (for autostart). |
| `--idle-timeout MIN` | Minutes hidden before the page goes back to the provider's home page (default 15; at most 525 600). |
| `--provider NAME\|URL` | Switch to a provider from the list, or to any `https://` URL. Shows the window. |
| `--add-provider NAME URL` | Add or replace a provider. |
| `--remove-provider NAME` | Remove a provider (an already-open page stays until restart). |
| `--instance NAME` | Address a separate daemon: own socket, window class, data and state file. |
| `--user-agent STRING` | Override the User-Agent (printable ASCII, up to 512 bytes). |
| `--quit` | Shut the daemon down. |
| `-h`, `--help`, `-V`, `--version` | Help and version. |

Only `--new-chat` and `--provider` show the window; the other commands leave its visibility alone.
Unknown flags are an error (the Python version ignored them).

---

## Providers

Built in (used until you add or remove one):

| Name | URL |
| --- | --- |
| `chatgpt` (default) | https://chatgpt.com |
| `gemini` | https://gemini.google.com |
| `claude` | https://claude.ai |

The list is `providers.json` in `$XDG_CONFIG_HOME/llm-wrapper/` (`~/.config/llm-wrapper/`), created by
the first `--add-provider` or `--remove-provider`; it then holds the full list and overrides the
built-ins. It is shared by every instance.

```bash
llm-wrapper --provider gemini
llm-wrapper --provider https://chat.mistral.ai
llm-wrapper --add-provider mistral https://chat.mistral.ai
```

Names match `[A-Za-z0-9_-]{1,32}`. URLs must be `https://`; **plain `http://` is accepted only for
`localhost`, `127.0.0.0/8` and `::1`** (a login cookie must not cross the network in cleartext).
Addresses with credentials in them, non-ASCII or oddly written hosts are refused. Invalid input is
reported on stderr and does nothing.

---

## Two ways to run it

1. **Single daemon + switcher** (default): one window; `--provider` switches between providers.
2. **Standalone instances**: one daemon per provider, each started and addressed by name:
   ```bash
   llm-wrapper --instance gemini --provider gemini --background
   llm-wrapper --instance gemini --toggle
   ```
   Each instance has its own socket, window class `llm-wrapper-<name>` (for compositor rules), its own
   login data (log in once per instance) and its own state file.

---

## Memory

While the window is visible the page is left alone. When it has been hidden for `--idle-timeout`
minutes, the daemon looks at the page:

- **Quiet, nothing typed** → the window goes back to the provider's home page, in a new web process
  started in the background. The old process, with everything the conversation held, ends. The next
  time you summon the window it is already on the home page: nothing to load. The conversation
  itself is a click away in the provider's own history.
- **Busy or something unsent** (loading, playing sound, a popup or file dialog open, a download
  running, text typed in a box or a file attached and not sent) → the page stays as it is, only the
  in-memory caches are dropped, and the daemon looks again later (every minute while busy).

Measured on the same machine against the Python version, with a fake provider whose page holds
about 150 MiB (a long conversation): 340 MiB while in use, **195 MiB** once idle and home, against
334 MiB for the Python version. On a light page the Rust version is about 11 MiB *heavier* (GTK 4's
UI process is larger). Figures and method: [`docs/spec.md`](docs/spec.md) section 6. Real chat sites
could not be measured from the build sandbox.

A daemon started with `--background` counts its idle time from the start. To keep a page alive
indefinitely, give `--idle-timeout` a very large value.

---

## Where things live

| What | Where |
|---|---|
| Logins, cookies, web storage | `$XDG_DATA_HOME/llm-wrapper` (`~/.local/share/llm-wrapper`; `…/instances/NAME` for an instance) |
| Disk cache | `$XDG_CACHE_HOME/llm-wrapper` (`~/.cache/llm-wrapper`) |
| Provider list | `$XDG_CONFIG_HOME/llm-wrapper/providers.json` (`~/.config/llm-wrapper/`) |
| Control socket | `$XDG_RUNTIME_DIR/llm-wrapper[-NAME].v1.sock` |
| Single-instance lock, state file | `$XDG_RUNTIME_DIR/llm-wrapper[-NAME].lock`, `llm-wrapper[-NAME].json` |

Everything is private to your user (`0700` / `0600`) and checked at each start. An existing directory
with loose permissions is tightened and reported; a symlink or a foreign owner is refused.

---

## Hyprland integration

Add to `hyprland.conf`; `llm-wrapper` must be on the `PATH` Hyprland sees (or use the full path).

```ini
# autostart, hidden
exec-once = llm-wrapper --background

# floating window
windowrulev2 = float, class:^(llm-wrapper)$
windowrulev2 = size 794 1286, class:^(llm-wrapper)$
windowrulev2 = move 78 74, class:^(llm-wrapper)$
windowrulev2 = pin, class:^(llm-wrapper)$

# keys
bind = $mainMod, G, exec, llm-wrapper --toggle
bind = $mainMod CTRL,  G, exec, llm-wrapper --hard-clean
bind = $mainMod SHIFT, G, exec, llm-wrapper --new-chat
```

A standalone instance has its own class, e.g. `class:^(llm-wrapper-gemini)$`.

---

## Quickshell (or any UI)

The daemon writes its state to `$XDG_RUNTIME_DIR/llm-wrapper.json` (`llm-wrapper-NAME.json` for an
instance), rewritten atomically on every change, so a UI can watch it:

```json
{"instance": null, "active": "chatgpt", "url": "https://chatgpt.com", "visible": true,
 "providers": {"chatgpt": "https://chatgpt.com", "gemini": "https://gemini.google.com", "claude": "https://claude.ai"}}
```

For an ad-hoc URL, `active` and `url` are both the URL. A UI drives the daemon only through the CLI
commands. (The socket speaks a private binary protocol, [documented](docs/wire-protocol.md) for
reference but not meant to be spoken by other programs.)

---

## Desktop entry

`make install` installs `llm-wrapper.desktop` (it runs `llm-wrapper --show`). If the menu does not
pick it up: `update-desktop-database ~/.local/share/applications`.


## Project layout

```
rust/crates/protocol   wire format, validation, CLI grammar (no dependencies)
rust/crates/platform   runtime-directory checks, socket client, private files (no dependencies)
rust/crates/ctl        the `llm-wrapper` command
rust/crates/daemon     `llm-wrapperd`: window, navigation policy, memory policy
rust/crates/e2e        black-box test suite, fake chat site, benchmark
rust/scripts/          build-release.sh, e2e.sh
docs/                  spec.md (behaviour and deviations), security.md, wire-protocol.md, baselines
packaging/             desktop entry, Arch PKGBUILD
legacy/                the retired Python version (reference only)
Makefile               build / install / dist / test
```

Development: `make test` (format, clippy, unit tests); the end-to-end suite needs `xvfb-run`,
`dbus-daemon` and is described in [`rust/README.md`](rust/README.md).

---

## License

MIT. Free to use, copy, modify and distribute; see [`LICENSE`](LICENSE).
