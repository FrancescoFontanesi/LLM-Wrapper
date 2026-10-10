# llm-wrapper (GTK + WebKitGTK)

A lightweight desktop wrapper for web AI chats: ChatGPT by default, plus Gemini, Claude or any URL. Designed as a quick-summon desktop assistant via hotkeys/macros (such as Hyprland, Sway, or Quickshell HUD), featuring instant IPC, proactive RAM management, full file upload/download portal support, and layout containment for long chat histories. You can use it in two ways: one daemon with an instant provider switcher, or one standalone instance per provider.

---

## Highlights
- Wraps web AI chats in a GTK WebKit2 window with a persistent profile.
- **Multiple providers**: switch instantly between ChatGPT, Gemini, Claude or any URL; each provider keeps its own page alive, and sign-in popups are supported.
- Maintains cookies and login state under `./session/`; transient data lives in `./cache/`.
- Offline recovery: when a page load fails, it retries every 30 s and reloads as soon as the network comes back.
- Supports custom user agents, background startup, and soft/hard cleanup modes.
- Ships with a `.desktop` file and ready-to-use Hyprland bindings.
- **Instant IPC Summon (<10ms)**: Fast-path CLI dispatcher forwards `--toggle`, `--show`, `--hide`, and other commands over a local Unix domain socket directly to the running backend without loading Python/WebKit libraries.
- **Fast Startup & Session Persistence**: Caches and service workers remain intact on disk under `./cache/` and `./session/`, eliminating slow cold-starts.
- **Proactive Memory Management**: While the window is visible, the full `WEB_BROWSER` cache model is kept. After the window has been hidden for `--idle-timeout` minutes (default 15), the wrapper switches to `DOCUMENT_VIEWER` and releases the in-memory cache once. The on-disk cache is never wiped automatically.
- **60 fps on power-saver**: WebKit halves rendering to 30 fps when the system power profile is `power-saver`; the wrapper hides the profile from WebKit (by not exposing the D-Bus system bus) so scrolling and streaming stay smooth.
- **Long Chat History Optimization**: Injects CSS rules with `content-visibility: auto` so offscreen messages skip layout and painting.
- **Full File System Integration**:
  - **Uploads**: Natively handles `run-file-chooser` using `Gtk.FileChooserNative` via XDG Desktop Portal (Hyprland / GTK portal).
  - **Downloads**: Natively handles `download-started` and `decide-destination` with portal save dialogs and desktop completion notifications.
  - **Clipboard**: `javascript_can_access_clipboard` enabled for seamless image paste (`Ctrl+V`).
- **External Links**: Automatically routes external URLs (sources, citations, documentation) to your default desktop browser without leaving the chat.
- **Assistant Keybindings**:
  - `Escape`: Instantly dismiss/hide the assistant (the key is not forwarded to the page, so it won't close the chat's own menus).
  - `Ctrl+N`: Start a clean new chat.
  - `Ctrl+R` / `F5`: Reload page (`Ctrl+Shift+R` for hard reload).
  - `Ctrl++` / `Ctrl+-` / `Ctrl+0`: Zoom in, zoom out, and reset zoom.
  - `F12`: Open Web Inspector for debugging.
  - Automatic focus on the chat prompt textarea whenever summoned.

---

## Requirements

### System packages
Install GObject introspection, GTK 3, and WebKitGTK from your distribution package manager:

- **Arch / Garuda Linux**:
  ```bash
  sudo pacman -S python-gobject gtk3 webkit2gtk-4.1 xdg-desktop-portal-hyprland
  ```
- **Debian / Ubuntu**:
  ```bash
  sudo apt install python3-gi gir1.2-gtk-3.0 gir1.2-webkit2-4.1
  ```

Optional but recommended: OpenBSD netcat (`openbsd-netcat` on Arch, `netcat-openbsd` on Debian/Ubuntu). The launcher
uses `nc -U` for the instant IPC path; without it every command goes through the slower Python fallback.

On other distributions, install the equivalents for GTK 3 and WebKitGTK 4.0/4.1. The script checks
for WebKit 4.1 first and transparently falls back to 4.0.

### Python environment

The launcher uses `.venv/` if it exists and otherwise falls back to the system `python3`, so a virtual environment is optional. If you want one, create it with system site packages enabled (access to the system-wide GI bindings):

```bash
python3 -m venv .venv --system-site-packages
source .venv/bin/activate
```

---

## Usage

Run or toggle the application from anywhere:

```bash
./llm-wrapper
./llm-wrapper [flags]
```

The script understands several optional flags (they can be combined):

| Flag | Description |
| --- | --- |
| `--toggle` | Toggle visibility of the assistant (summons or hides window instantly). |
| `--show` | Ensure the assistant window is visible and focused. |
| `--hide` | Hide the assistant window into the background. |
| `--new-chat` | Navigate back to a clean new chat. |
| `--soft-clean` | Release the in-memory cache (disk cache and session are kept). |
| `--hard-clean` | Purge disk and DOM caches while preserving login cookies. |
| `--background` | Launch the backend hidden right away (ideal for autostart). |
| `--idle-timeout <min>` | Minutes hidden before deep memory trim triggers (default: 15). |
| `--provider NAME\|URL` | Switch the window to a provider from the list, or to any http(s) URL. Shows the window. |
| `--add-provider NAME URL` | Add or replace a provider in `providers.json`. |
| `--remove-provider NAME` | Remove a provider from the list (an already-open view stays until restart). |
| `--instance NAME` | Run or address a separate daemon: own socket, window class, data and state file. |
| `--quit` | Gracefully shut down the wrapper daemon. |
| `--user-agent "UA"` | Override the default Chromium-based user agent string. |

---

## Providers

Built-in providers (used until a `providers.json` exists):

| Name | URL |
| --- | --- |
| `chatgpt` (default) | https://chatgpt.com |
| `gemini` | https://gemini.google.com |
| `claude` | https://claude.ai |

`providers.json` is created on the first `--add-provider` or `--remove-provider`, holds the full list and overrides the built-ins. It lives in the repository directory and is gitignored.

```bash
./llm-wrapper --provider gemini            # switch the running window to Gemini
./llm-wrapper --provider https://chat.mistral.ai
./llm-wrapper --add-provider mistral https://chat.mistral.ai
```

Provider names must match `[A-Za-z0-9_-]{1,32}` and URLs must be http or https. Invalid input is reported on stderr and ignored.

---

## Two ways to run it

1. **Single daemon + switcher** (default): one window; `--provider` switches between providers instantly.
2. **Standalone instances**: one daemon per provider, each started and addressed by name:
   ```bash
   ./llm-wrapper --instance gemini --provider gemini --background
   ./llm-wrapper --instance gemini --toggle
   ```
   Each instance has its own socket, window class `llm-wrapper-<name>` (for Hyprland rules), its own data under `instances/<name>/` (log in once per instance), and its own state file.

---

## Hyprland integration

For autostart and window behaviour, add the following to `hyprland.conf`.

### Autostart
Launch the wrapper hidden on login:
```ini
exec-once = /path/to/LLM-Wrapper/llm-wrapper --background
```

### Floating Window Rules
```ini
windowrulev2 = float, class:^(llm-wrapper)$
windowrulev2 = size 794 1286, class:^(llm-wrapper)$
windowrulev2 = move 78 74, class:^(llm-wrapper)$
windowrulev2 = pin, class:^(llm-wrapper)$
```

For a standalone instance, match its window class, e.g.:
```ini
windowrulev2 = float, class:^(llm-wrapper-gemini)$
```

### Keybindings
```ini
bind = $mainMod, G, exec, /path/to/LLM-Wrapper/llm-wrapper --toggle
bind = $mainMod CTRL,  G, exec, /path/to/LLM-Wrapper/llm-wrapper --hard-clean
bind = $mainMod SHIFT, G, exec, /path/to/LLM-Wrapper/llm-wrapper --new-chat
```

---

## Integrating with Quickshell (or any UI)

The running daemon writes its state to `$XDG_RUNTIME_DIR/llm-wrapper.json` (or `llm-wrapper-<name>.json` for an instance):

```json
{"instance": null, "active": "chatgpt", "url": "https://chatgpt.com", "visible": true, "providers": {"chatgpt": "https://chatgpt.com", "gemini": "https://gemini.google.com", "claude": "https://claude.ai"}}
```

The file is rewritten atomically on every change (startup, switch, add, remove, visibility), so a UI can watch it. For an ad-hoc URL provider, `active` and `url` are both the URL.

The UI drives the daemon only through CLI commands (`--provider`, `--add-provider`, `--remove-provider`, `--toggle`, ...).

---

## Desktop entry

Edit [`llm-wrapper.desktop`](llm-wrapper.desktop) to point `Exec=` to the repository location,
copy it to `~/.local/share/applications/`, then run:

```bash
update-desktop-database ~/.local/share/applications
```

---

## Project structure

```
llm-wrapper           # Ultra-fast CLI & IPC launcher (~2-5ms)
app.py                # GTK3 + WebKitGTK application server
llm-wrapper.desktop
cache/                # WebKit on-disk cache (persisted for fast cold-starts)
session/              # Persistent session, cookies, and local storage
providers.json        # your provider list (optional, gitignored)
instances/            # data of standalone instances
```

Feel free to tailor it further: the launcher is a tiny bash script and the whole app lives in `app.py`.
