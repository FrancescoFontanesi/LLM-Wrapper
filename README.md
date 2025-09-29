# ChatGPT Wrapper (GTK + WebKitGTK)

A lightweight desktop wrapper for [chatgpt.com](https://chatgpt.com) built with PyGObject. The app
runs as a single-file script, keeps your session on disk, exposes toggle-friendly CLI flags for
window managers such as Hyprland, and performs automatic light cleanup while hidden to keep things
snappy.

---

## Highlights
- Embeds ChatGPT in a GTK WebKit2 window with a persistent profile.
- Maintains cookies and login state under `./session/`; transient data lives in `./cache/`.
- Optional auto-clean runs every 5 minutes while the window stays hidden (`--toggle` friendly).
- Supports custom user agents, background startup, and soft/hard cleanup modes.
- Ships with a `.desktop` file and ready-to-use Hyprland bindings.

## Requirements

### System packages
Install the GObject introspection stack and WebKitGTK from your distro. Example for Debian / Ubuntu:

```bash
sudo apt install python3-gi gir1.2-gtk-3.0 gir1.2-webkit2-4.1
```

On other distributions, install the equivalents for GTK 3 and WebKitGTK 4.0/4.1. The script checks
for WebKit 4.1 first and transparently falls back to 4.0.

### Python environment

1. Create a virtual environment with access to system-wide GI bindings:
   ```bash
   python3 -m venv .venv --system-site-packages
   source .venv/bin/activate
   ```
2. Install the Python wheels listed in [`requirements.txt`](requirements.txt):
   ```bash
   pip install -r requirements.txt
   ```

> ℹ️ The GI modules (`gi`, `Gtk`, `WebKit2`, …) are provided by the system packages above. The
> Python dependency file only contains helper wheels that complement them.

## Usage

From the repository root:

```bash
./chatgpt-wrapper
```

The script understands several optional flags (they can be combined):

| Flag | Description |
| --- | --- |
| `--toggle` | Show or hide the existing instance window (creates a scratchpad-like behaviour). |
| `--background` | Launch the application hidden right away. |
| `--soft-clean` | Clear cache and service worker data while keeping cookies/session data. |
| `--hard-clean` | Remove every persisted artifact except cookies. |
| `--user-agent "UA"` | Override the default Chromium-based desktop user agent string. |

While hidden, a soft clean runs every 5 minutes. Use the manual flags above if you need an
immediate reset.

## Hyprland integration

For a dedicated special workspace:

```ini
windowrulev2 = workspace special:chatgpt, appid:^(chatgpt-wrapper)$
windowrulev2 = float, appid:^(chatgpt-wrapper)$
bind = $mainMod, G, togglespecialworkspace, chatgpt
exec-once = /absolute/path/to/chatgpt-wrapper --background
```

If you just want toggle bindings:

```ini
bind = $mainMod, G, exec, /absolute/path/to/chatgpt-wrapper --toggle
bind = $mainMod SHIFT, G, exec, /absolute/path/to/chatgpt-wrapper --soft-clean
bind = $mainMod CTRL,  G, exec, /absolute/path/to/chatgpt-wrapper --hard-clean
```

## Desktop entry

Edit [`chatgpt-wrapper.desktop`](chatgpt-wrapper.desktop) to point `Exec=` to the repository location
and copy it to `~/.local/share/applications/`. Afterwards run:

```bash
update-desktop-database ~/.local/share/applications
```

## Project structure

```
chatgpt-wrapper       # GTK/WebKit wrapper script
chatgpt-wrapper.desktop
cache/                # WebKit cache (safe to delete)
session/              # Persistent cookies & profile
```

Feel free to tailor the script further—everything lives in a single file for quick hacking.
