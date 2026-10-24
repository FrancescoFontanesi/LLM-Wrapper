#!/usr/bin/env python3
"""
Optimized LLM web chat wrapper core (GTK3 + WebKitGTK).

- Single persistent instance with Unix socket IPC listener.
- Preserves disk cache and service workers for instant cold/warm loads.
- Proactive memory pressure caps and dynamic cache-model management.
- User style sheet containment (content-visibility: auto) for long chats.
- Wayland XDG Desktop Portal integration for file upload & download.
- Assistant keybindings: Escape (hide), Ctrl+N (new chat), Ctrl+R (reload), zoom, F12.
- External link handling (opened in default system browser).
"""

import argparse
import json
import os
import re
import socket
import subprocess
import sys
import threading
from pathlib import Path
from urllib.parse import urlparse

# Instance name selects a separate daemon: own socket, state file and data dirs
NAME_RE = re.compile(r"[A-Za-z0-9_-]{1,32}")


def _argv_value(argv, flag):
    for i, a in enumerate(argv):
        if a == flag and i + 1 < len(argv):
            return argv[i + 1]
        if a.startswith(flag + "="):
            return a.split("=", 1)[1]
    return None


INSTANCE = _argv_value(sys.argv[1:], "--instance")
if INSTANCE is not None and not NAME_RE.fullmatch(INSTANCE):
    sys.exit(f"[llm-wrapper] invalid --instance name: {INSTANCE!r}")
SUFFIX = f"-{INSTANCE}" if INSTANCE else ""
RUNTIME_DIR = Path(os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
SOCK_PATH = RUNTIME_DIR / f"llm-wrapper{SUFFIX}.sock"
STATE_PATH = RUNTIME_DIR / f"llm-wrapper{SUFFIX}.json"


def try_fast_client(args: list[str]) -> bool:
    """Attempts to forward CLI commands to an already running wrapper instance."""
    if not SOCK_PATH.exists():
        return False

    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(0.8)
            client.connect(str(SOCK_PATH))
            payload = json.dumps(args).encode("utf-8") + b"\n"
            client.sendall(payload)
            response = client.recv(1024)
            return response.strip() == b"OK"
    except (socket.error, OSError):
        return False


# If invoked directly via Python with an active instance, dispatch and exit
if try_fast_client(sys.argv[1:]):
    sys.exit(0)

# ==============================================================================
# HEAVY IMPORTS (Only executed by primary server)
# ==============================================================================

# WebKit caps rendering at 30 fps when power-profiles-daemon reports power-saver; hide the system bus so
# it can't see it (network monitoring falls back to netlink, everything else here uses the session bus).
# shortcut: drops all system-bus access for app + WebKit children, revisit if a feature needs it (e.g. geolocation).
os.environ["DBUS_SYSTEM_BUS_ADDRESS"] = "unix:path=/nonexistent"

import gi
from gi import Repository

repo = Repository.get_default()
webkit_versions = repo.enumerate_versions("WebKit2")
if "4.1" in webkit_versions:
    WEBKIT_VERSION = "4.1"
    GTK_VERSION = "3.0"
elif "4.0" in webkit_versions:
    WEBKIT_VERSION = "4.0"
    GTK_VERSION = "3.0"
else:  # pragma: no cover
    raise ImportError("WebKitGTK introspection data (4.1/4.0) not available")

gi.require_version("Gtk", GTK_VERSION)
gi.require_version("WebKit2", WEBKIT_VERSION)

from gi.repository import Gio, GLib, GObject, Gtk, WebKit2

try:
    gi.require_version("Gdk", GTK_VERSION)
    from gi.repository import Gdk
except (ValueError, ImportError):
    Gdk = None

try:
    gi.require_version("GdkWayland", GTK_VERSION)
except ValueError:
    try:
        gi.require_version("GdkWayland", "1.0")
    except ValueError:
        pass

try:
    from gi.repository import GdkWayland
except ImportError:
    GdkWayland = None

APP_ID = "com.llm.wrapper" + (f".i_{INSTANCE.replace('-', '_')}" if INSTANCE else "")
WAYLAND_APP_ID = f"llm-wrapper{SUFFIX}"
GLib.set_prgname(WAYLAND_APP_ID)
GLib.set_application_name("LLM")
BASE_DIR = Path(__file__).resolve().parent
DATA_DIR = BASE_DIR / "instances" / INSTANCE / "session" if INSTANCE else BASE_DIR / "session"
CACHE_DIR = BASE_DIR / "instances" / INSTANCE / "cache" if INSTANCE else BASE_DIR / "cache"
COOKIE_STORE = DATA_DIR / "cookies.sqlite"
PROVIDERS_FILE = BASE_DIR / "providers.json"
DEFAULT_PROVIDERS = {"chatgpt": "https://chatgpt.com", "gemini": "https://gemini.google.com", "claude": "https://claude.ai"}
DEFAULT_PROVIDER = "chatgpt"
USER_AGENT = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
)

ALLOWED_DOMAINS = {
    "chatgpt.com",
    "openai.com",
    "auth0.openai.com",
    "auth.openai.com",
    "oaistatic.com",
    "oaiusercontent.com",
    "cloudflare.com",
    "challenges.cloudflare.com",
    "turnstile.com",
    "cloudflareinsights.com",
    "cf-assets.com",
    "arkoselabs.com",
    "livekit.cloud",
    "statsig.com",
    "statsigapi.net",
    "featuregates.org",
    "identrust.com",
    "sentry.io",
    "accounts.google.com",
    "appleid.apple.com",
    "login.microsoftonline.com",
    "login.live.com",
}

DATA_DIR.mkdir(parents=True, exist_ok=True)
CACHE_DIR.mkdir(parents=True, exist_ok=True)
COOKIE_STORE.touch(exist_ok=True)


def is_valid_url(url) -> bool:
    """Only http(s) URLs with a host are accepted as providers."""
    try:
        parsed = urlparse(url)
        return parsed.scheme in ("http", "https") and bool(parsed.hostname)
    except (TypeError, ValueError):
        return False


def load_providers() -> dict[str, str]:
    try:
        data = json.loads(PROVIDERS_FILE.read_text())
        return {k: v for k, v in data.items() if NAME_RE.fullmatch(k) and is_valid_url(v)}
    except (OSError, ValueError, AttributeError):
        return dict(DEFAULT_PROVIDERS)


def base_domain(host: str) -> str:
    # shortcut: last two labels, wrong for multi-part TLDs like co.uk; switch to a public-suffix list if a provider needs it.
    return ".".join(host.split(".")[-2:])


def is_internal(uri: str, provider_url: str) -> bool:
    host = urlparse(uri).hostname or ""
    provider_host = urlparse(provider_url).hostname or ""
    allowed = set(ALLOWED_DOMAINS)
    if provider_host:
        allowed.add(base_domain(provider_host))
    return any(host == d or host.endswith("." + d) for d in allowed)


def send_notification(summary: str, body: str = "") -> None:
    try:
        subprocess.Popen(
            ["notify-send", "-a", "llm-wrapper", summary, body],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except Exception:
        pass


class LLMApplication(Gtk.Application):
    """Optimized GTK application embedding WebKit with persistent storage."""

    def __init__(self) -> None:
        super().__init__(
            application_id=APP_ID,
            flags=Gio.ApplicationFlags.HANDLES_COMMAND_LINE,
        )
        self.window: Gtk.ApplicationWindow | None = None
        self.context: WebKit2.WebContext | None = None
        self.datamanager: WebKit2.WebsiteDataManager | None = None
        self.cookie_manager: WebKit2.CookieManager | None = None
        self.webview: WebKit2.WebView | None = None
        self.network_monitor: Gio.NetworkMonitor | None = None

        self._providers: dict[str, str] = load_providers()
        self._views: dict[str, WebKit2.WebView] = {}
        self._active: str | None = None
        self._settings: WebKit2.Settings | None = None
        self._ucm: WebKit2.UserContentManager | None = None
        self._failed_views: set = set()
        self._error_page_views: set = set()
        self._user_agent_override: str | None = None
        self._idle_timeout_id: int = 0
        self._reload_timer_id: int = 0
        self._idle_timeout_minutes: int = 15

        self._server_sock: socket.socket | None = None
        self._server_thread: threading.Thread | None = None
        self._server_running = False

    # Gtk lifecycle -----------------------------------------------------
    def do_startup(self) -> None:  # type: ignore[override]
        Gtk.Application.do_startup(self)

        # Hold application so it persists when window is hidden
        self.hold()

        self.datamanager = WebKit2.WebsiteDataManager(
            base_data_directory=str(DATA_DIR),
            base_cache_directory=str(CACHE_DIR),
        )

        if hasattr(self.datamanager, "set_itp_enabled"):
            try:
                self.datamanager.set_itp_enabled(False)
            except GLib.Error:
                pass

        if hasattr(self.datamanager, "set_persistent_credential_storage_enabled"):
            self.datamanager.set_persistent_credential_storage_enabled(True)

        # Proactive memory management: cap the WebKit network process RAM (set_memory_pressure_settings applies there) and trigger automatic GC
        if hasattr(self.datamanager, "set_memory_pressure_settings"):
            mps = WebKit2.MemoryPressureSettings.new()
            mps.set_strict_threshold(0.75)       # 384 MB: strict GC and aggressive drop
            mps.set_conservative_threshold(0.5)  # 256 MB: start gentle GC and cache purge
            mps.set_memory_limit(512)            # 512 MB memory limit
            mps.set_poll_interval(5.0)           # Check memory every 5 seconds
            self.datamanager.set_memory_pressure_settings(mps)

        self.context = WebKit2.WebContext.new_with_website_data_manager(self.datamanager)
        self.context.set_cache_model(WebKit2.CacheModel.WEB_BROWSER)

        # Download handling
        self.context.connect("download-started", self._on_download_started)

        # Cookies
        self.cookie_manager = self.context.get_cookie_manager()
        if self.cookie_manager is not None:
            self.cookie_manager.set_accept_policy(WebKit2.CookieAcceptPolicy.ALWAYS)
            self.cookie_manager.set_persistent_storage(
                str(COOKIE_STORE), WebKit2.CookiePersistentStorage.SQLITE
            )

        # Network monitor for offline recovery
        self.network_monitor = Gio.NetworkMonitor.get_default()
        if self.network_monitor is not None:
            self.network_monitor.connect("network-changed", self._on_network_changed)

        # Start Unix domain socket listener for instant IPC (<20ms)
        self._start_socket_server()

    def do_shutdown(self) -> None:  # type: ignore[override]
        self._stop_socket_server()

        if self._idle_timeout_id:
            GLib.source_remove(self._idle_timeout_id)
            self._idle_timeout_id = 0

        if self._reload_timer_id:
            GLib.source_remove(self._reload_timer_id)
            self._reload_timer_id = 0

        try:
            STATE_PATH.unlink(missing_ok=True)
        except OSError:
            pass

        Gtk.Application.do_shutdown(self)

    def do_activate(self) -> None:  # type: ignore[override]
        self._ensure_window()
        self._switch_provider(self._active or DEFAULT_PROVIDER)

        if self.window is not None:
            self.window.present()

    def do_command_line(self, command_line: Gio.ApplicationCommandLine) -> int:  # type: ignore[override]
        raw_args = command_line.get_arguments()[1:]
        self._execute_command_list(raw_args)
        return 0

    # Fast IPC Server ---------------------------------------------------
    def _start_socket_server(self) -> None:
        try:
            if SOCK_PATH.exists():
                SOCK_PATH.unlink(missing_ok=True)

            self._server_sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self._server_sock.bind(str(SOCK_PATH))
            self._server_sock.listen(10)
            self._server_running = True

            self._server_thread = threading.Thread(
                target=self._socket_listener_worker,
                daemon=True,
                name="llm-ipc-listener",
            )
            self._server_thread.start()
        except Exception as err:
            print(f"[llm-wrapper] Error starting IPC socket server: {err}", file=sys.stderr)

    def _stop_socket_server(self) -> None:
        self._server_running = False
        if self._server_sock:
            try:
                self._server_sock.close()
            except Exception:
                pass
            self._server_sock = None
        try:
            SOCK_PATH.unlink(missing_ok=True)
        except OSError:
            pass

    def _socket_listener_worker(self) -> None:
        while self._server_running and self._server_sock:
            try:
                conn, _ = self._server_sock.accept()
                with conn:
                    conn.settimeout(1.0)
                    data = conn.recv(4096)
                    if not data:
                        continue
                    try:
                        args = json.loads(data.decode("utf-8").strip())
                    except Exception:
                        args = [a for a in data.decode("utf-8").splitlines() if a]

                    GLib.idle_add(self._execute_command_list, args)
                    conn.sendall(b"OK\n")
            except (socket.timeout, OSError):
                if not self._server_running:
                    break
            except Exception as err:
                if self._server_running:
                    print(f"[llm-wrapper] IPC listener error: {err}", file=sys.stderr)

    def _execute_command_list(self, raw_args: list[str]) -> None:
        parser = argparse.ArgumentParser(add_help=False)
        parser.add_argument("--toggle", action="store_true")
        parser.add_argument("--show", action="store_true")
        parser.add_argument("--hide", action="store_true")
        parser.add_argument("--new-chat", action="store_true")
        parser.add_argument("--soft-clean", action="store_true")
        parser.add_argument("--hard-clean", action="store_true")
        parser.add_argument("--user-agent", type=str)
        parser.add_argument("--background", action="store_true")
        parser.add_argument("--idle-timeout", type=int)
        parser.add_argument("--quit", action="store_true")
        parser.add_argument("--provider", type=str)
        parser.add_argument("--add-provider", nargs=2, metavar=("NAME", "URL"))
        parser.add_argument("--remove-provider", type=str)
        parser.add_argument("--instance", type=str)

        try:
            args, _ = parser.parse_known_args(raw_args)
        except (Exception, SystemExit):
            args = parser.parse_args([])

        if args.quit:
            self.quit()
            return

        if args.idle_timeout is not None:
            self._idle_timeout_minutes = max(1, args.idle_timeout)

        if args.user_agent:
            self._user_agent_override = args.user_agent
            self._apply_user_agent(self._user_agent_override)

        if args.add_provider:
            name, url = args.add_provider
            if NAME_RE.fullmatch(name) and is_valid_url(url):
                self._providers[name] = url
                self._save_providers()
            else:
                print(f"[llm-wrapper] invalid provider: {name!r} {url!r}", file=sys.stderr)

        if args.remove_provider:
            if self._providers.pop(args.remove_provider, None) is not None:
                self._save_providers()
            else:
                print(f"[llm-wrapper] unknown provider: {args.remove_provider!r}", file=sys.stderr)

        self._ensure_window()
        provider = self._resolve_provider(args.provider) if args.provider else None
        self._switch_provider(provider or self._active or DEFAULT_PROVIDER)

        if args.new_chat:
            self.new_chat()

        if args.soft_clean:
            self.soft_clean()

        if args.hard_clean:
            self.hard_clean()

        if args.toggle:
            if self.window is not None:
                if self.window.get_visible():
                    self.window.hide()
                else:
                    self.window.present()
        elif args.show:
            if self.window is not None:
                self.window.present()
        elif args.hide:
            if self.window is not None:
                self.window.hide()
        elif args.background:
            if self.window is not None:
                self.window.hide()
        else:
            if self.window is not None:
                self.window.present()

    # Public actions ----------------------------------------------------
    def new_chat(self) -> None:
        """Navigates to the active provider root URL."""
        if self.webview is not None:
            self.webview.load_uri(self._url_for(self._active))

    def soft_clean(self) -> None:
        """Trims volatile RAM without wiping persistent disk caches or service workers."""
        if self.context is None or self.datamanager is None:
            return

        # Clean only in-memory cache to release decoded images, bitmaps, and buffers
        self.datamanager.clear(WebKit2.WebsiteDataTypes.MEMORY_CACHE, 0, None, None, None)
        self._evaluate_js("if (window.gc) { window.gc(); }")

    def hard_clean(self) -> None:
        """Purges transient disk cache and DOM caches while preserving login cookies."""
        if self.datamanager is None:
            return

        cleanup_types = (
            WebKit2.WebsiteDataTypes.DISK_CACHE
            | WebKit2.WebsiteDataTypes.MEMORY_CACHE
            | WebKit2.WebsiteDataTypes.DOM_CACHE
            | WebKit2.WebsiteDataTypes.OFFLINE_APPLICATION_CACHE
        )
        self.datamanager.clear(cleanup_types, 0, None, None, None)

    # Window & WebKit Setup ---------------------------------------------
    def _ensure_window(self) -> None:
        if self.window is not None:
            return

        self.window = Gtk.ApplicationWindow(application=self)
        self.window.set_title("LLM")
        self.window.set_default_size(794, 1286)
        self.window.set_role(WAYLAND_APP_ID)
        try:
            self.window.set_wmclass(WAYLAND_APP_ID, WAYLAND_APP_ID)
        except Exception:
            pass

        self.window.connect("realize", self._on_window_realize)
        self.window.connect("delete-event", self._on_window_delete)
        self.window.connect("key-press-event", self._on_key_press_event)
        self.window.connect("notify::visible", self._on_visible_changed)

        # WebKit Settings
        settings = WebKit2.Settings()
        settings.set_enable_smooth_scrolling(True)
        settings.set_enable_javascript(True)
        settings.set_javascript_can_access_clipboard(True)
        settings.set_javascript_can_open_windows_automatically(True)
        settings.set_allow_modal_dialogs(True)
        settings.set_enable_page_cache(True)
        settings.set_enable_developer_extras(True)
        settings.set_hardware_acceleration_policy(WebKit2.HardwareAccelerationPolicy.ALWAYS)
        settings.set_enable_webgl(True)
        settings.set_enable_webaudio(True)
        settings.set_enable_mediasource(True)
        settings.set_enable_media_stream(True)
        settings.set_enable_encrypted_media(True)
        settings.set_enable_site_specific_quirks(True)

        # Inject CSS for containment and stealth JS for Turnstile verification
        self._settings = settings
        self._ucm = self._create_user_content_manager()
        self._apply_user_agent(self._user_agent_override)

    # Provider views ----------------------------------------------------
    def _url_for(self, name: str) -> str:
        return self._providers.get(name, name)

    def _view_url(self, view: WebKit2.WebView) -> str:
        name = next((n for n, v in self._views.items() if v is view), self._active)
        return self._url_for(name)

    def _create_view(self, name: str) -> WebKit2.WebView:
        view = WebKit2.WebView(web_context=self.context, user_content_manager=self._ucm)
        view.set_settings(self._settings)

        view.connect("load-changed", self._on_webview_load_changed)
        view.connect("load-failed", self._on_webview_load_failed)
        view.connect("run-file-chooser", self._on_run_file_chooser)
        view.connect("decide-policy", self._on_decide_policy)
        view.connect("create", self._on_webview_create)

        view.load_uri(self._url_for(name))
        return view

    def _switch_provider(self, name: str) -> None:
        view = self._views.get(name)
        if view is None:
            view = self._create_view(name)
            self._views[name] = view

        if view is not self.webview:
            if self.webview is not None:
                self.window.remove(self.webview)
            self.window.add(view)
            view.show()
            self.webview = view

        self._active = name
        self.window.set_title(name if name in self._providers else (urlparse(name).hostname or name))
        self._write_state()
        if self.window.get_visible():
            view.grab_focus()

    def _resolve_provider(self, value: str) -> str | None:
        if value in self._providers or is_valid_url(value):
            return value
        print(f"[llm-wrapper] unknown provider or invalid URL: {value!r}", file=sys.stderr)
        return None

    def _save_providers(self) -> None:
        try:
            PROVIDERS_FILE.write_text(json.dumps(self._providers, indent=2) + "\n")
        except OSError as err:
            print(f"[llm-wrapper] cannot write {PROVIDERS_FILE}: {err}", file=sys.stderr)
        self._write_state()

    def _write_state(self) -> None:
        state = {
            "instance": INSTANCE,
            "active": self._active,
            "url": self._url_for(self._active),
            "visible": self.window is not None and self.window.get_visible(),
            "providers": self._providers,
        }
        tmp = STATE_PATH.with_suffix(".tmp")
        try:
            tmp.write_text(json.dumps(state, indent=2))
            os.replace(tmp, STATE_PATH)
        except OSError as err:
            print(f"[llm-wrapper] cannot write state file: {err}", file=sys.stderr)

    def _create_user_content_manager(self) -> WebKit2.UserContentManager:
        ucm = WebKit2.UserContentManager()
        css = """
        /* Optimize chat history rendering via offscreen skipping */
        div[data-message-author-role] {
            content-visibility: auto !important;
            contain-intrinsic-size: auto 320px !important;
        }

        /* Prevent expensive layout recalculations in syntax-highlighted code */
        pre code {
            contain: layout style !important;
        }
        """
        stylesheet = WebKit2.UserStyleSheet(
            source=css,
            injected_frames=WebKit2.UserContentInjectedFrames.ALL_FRAMES,
            level=WebKit2.UserStyleLevel.USER,
            allow_list=[],
            block_list=[],
        )
        ucm.add_style_sheet(stylesheet)

        # Stealth UserScript to align WebKit JS environment with Chrome on Linux
        # Resolves Cloudflare Turnstile infinite challenge loop
        stealth_js = """
        (function() {
            try {
                Object.defineProperty(navigator, 'webdriver', {
                    get: () => false,
                    configurable: true
                });
            } catch(e) {}

            try {
                Object.defineProperty(navigator, 'vendor', {
                    get: () => 'Google Inc.',
                    configurable: true
                });
            } catch(e) {}

            if (!window.chrome) {
                window.chrome = {
                    app: {
                        isInstalled: false,
                        InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' },
                        RunningState: { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' }
                    },
                    runtime: {
                        OnInstalledReason: { CHROME_UPDATE: 'chrome_update', INSTALL: 'install', SHARED_MODULE_UPDATE: 'shared_module_update', UPDATE: 'update' },
                        OnRestartRequiredReason: { APP_UPDATE: 'app_update', OS_UPDATE: 'os_update', PERIODIC: 'periodic' },
                        PlatformArch: { ARM: 'arm', ARM64: 'arm64', MIPS: 'mips', MIPS64: 'mips64', X86_32: 'x86-32', X86_64: 'x86-64' },
                        PlatformNaclArch: { ARM: 'arm', MIPS: 'mips', MIPS64: 'mips64', X86_32: 'x86-32', X86_64: 'x86-64' },
                        PlatformOs: { ANDROID: 'android', CROS: 'cros', LINUX: 'linux', MAC: 'mac', OPENBSD: 'openbsd', WIN: 'win' },
                        RequestUpdateCheckStatus: { NO_UPDATE: 'no_update', THROTTLED: 'throttled', UPDATE_AVAILABLE: 'update_available' }
                    },
                    loadTimes: function() {},
                    csi: function() {}
                };
            }

            try {
                function patchWebGL(proto) {
                    if (!proto || !proto.getParameter) return;
                    const origGetParameter = proto.getParameter;
                    proto.getParameter = function(param) {
                        const ext = this.getExtension('WEBGL_debug_renderer_info');
                        if (ext) {
                            if (param === ext.UNMASKED_VENDOR_WEBGL) return 'Intel Open Source Technology Center';
                            if (param === ext.UNMASKED_RENDERER_WEBGL) return 'Mesa Intel(R) Xe Graphics (TGL GT2)';
                        }
                        return origGetParameter.apply(this, arguments);
                    };
                }
                if (window.WebGLRenderingContext) patchWebGL(window.WebGLRenderingContext.prototype);
                if (window.WebGL2RenderingContext) patchWebGL(window.WebGL2RenderingContext.prototype);
            } catch(e) {}

            try {
                if (!navigator.userAgentData) {
                    Object.defineProperty(navigator, 'userAgentData', {
                        get: () => ({
                            brands: [
                                { brand: 'Chromium', version: '131' },
                                { brand: 'Not_A Brand', version: '24' },
                                { brand: 'Google Chrome', version: '131' }
                            ],
                            mobile: false,
                            platform: 'Linux',
                            getHighEntropyValues: function() {
                                return Promise.resolve({
                                    architecture: 'x86',
                                    bitness: '64',
                                    brands: [
                                        { brand: 'Chromium', version: '131' },
                                        { brand: 'Not_A Brand', version: '24' },
                                        { brand: 'Google Chrome', version: '131' }
                                    ],
                                    fullVersionList: [
                                        { brand: 'Chromium', version: '131.0.6778.85' },
                                        { brand: 'Not_A Brand', version: '24.0.0.0' },
                                        { brand: 'Google Chrome', version: '131.0.6778.85' }
                                    ],
                                    mobile: false,
                                    model: '',
                                    platform: 'Linux',
                                    platformVersion: '6.12.0'
                                });
                            }
                        }),
                        configurable: true
                    });
                }
            } catch(e) {}

            try {
                if (!navigator.plugins || navigator.plugins.length === 0) {
                    const fakePlugins = [
                        { name: 'PDF Viewer', description: 'Portable Document Format', filename: 'internal-pdf-viewer' },
                        { name: 'Chrome PDF Viewer', description: 'Portable Document Format', filename: 'internal-pdf-viewer' },
                        { name: 'Chromium PDF Viewer', description: 'Portable Document Format', filename: 'internal-pdf-viewer' }
                    ];
                    fakePlugins.item = function(i) { return this[i] || null; };
                    fakePlugins.namedItem = function(name) { return this.find(p => p.name === name) || null; };
                    fakePlugins.refresh = function() {};
                    Object.defineProperty(navigator, 'plugins', {
                        get: () => fakePlugins,
                        configurable: true
                    });
                }
            } catch(e) {}
        })();
        """
        stealth_script = WebKit2.UserScript(
            source=stealth_js,
            injected_frames=WebKit2.UserContentInjectedFrames.ALL_FRAMES,
            injection_time=WebKit2.UserScriptInjectionTime.START,
            allow_list=[],
            block_list=[],
        )
        ucm.add_script(stealth_script)
        return ucm

    def _apply_user_agent(self, override: str | None) -> None:
        user_agent = override or USER_AGENT
        if self._settings is not None and hasattr(self._settings, "set_user_agent"):
            self._settings.set_user_agent(user_agent)
        else:
            for view in self._views.values():
                if hasattr(view, "set_custom_user_agent"):
                    view.set_custom_user_agent(user_agent)

    def _evaluate_js(self, script: str) -> None:
        if self.webview is None:
            return
        try:
            if hasattr(self.webview, "evaluate_javascript"):
                self.webview.evaluate_javascript(script, len(script), None, None, None, None, None)
            elif hasattr(self.webview, "run_javascript"):
                self.webview.run_javascript(script, None, None, None)
        except Exception:
            pass

    # Window events -----------------------------------------------------
    def _on_window_delete(self, window: Gtk.Window, _event: Gdk.Event) -> bool:
        window.hide()
        return True

    def _on_window_realize(self, window: Gtk.ApplicationWindow, *_) -> None:
        if Gdk is None:
            return

        get_surface = getattr(window, "get_surface", None) or getattr(window, "get_window", None)
        if get_surface is None:
            return

        surface = get_surface()
        if surface is None:
            return

        try:
            surface.set_app_id(WAYLAND_APP_ID)
        except AttributeError:
            if GdkWayland is not None and hasattr(GdkWayland, "window_set_app_id"):
                try:
                    GdkWayland.window_set_app_id(surface, WAYLAND_APP_ID)
                except Exception:
                    pass

    def _on_key_press_event(self, window: Gtk.Window, event: Gdk.EventKey) -> bool:
        if Gdk is None:
            return False

        state = event.state & Gtk.accelerator_get_default_mod_mask()
        keyval = event.keyval

        # Escape -> dismiss/hide assistant window
        if keyval == Gdk.KEY_Escape and state == 0:
            window.hide()
            return True

        # Ctrl+N -> New chat
        if (keyval in (Gdk.KEY_n, Gdk.KEY_N)) and (state == Gdk.ModifierType.CONTROL_MASK):
            self.new_chat()
            return True

        # Ctrl+R or F5 -> Reload
        if ((keyval in (Gdk.KEY_r, Gdk.KEY_R)) and (state & Gdk.ModifierType.CONTROL_MASK)) or (keyval == Gdk.KEY_F5):
            if self.webview is not None:
                if state & Gdk.ModifierType.SHIFT_MASK:
                    self.webview.reload_bypass_cache()
                else:
                    self.webview.reload()
            return True

        # Zoom controls: Ctrl + Plus / Ctrl + Minus / Ctrl + 0 (Shift allowed: '+' needs it on US layouts)
        ctrl_only = (state & ~Gdk.ModifierType.SHIFT_MASK) == Gdk.ModifierType.CONTROL_MASK
        if ctrl_only:
            if keyval in (Gdk.KEY_plus, Gdk.KEY_equal, Gdk.KEY_KP_Add):
                if self.webview is not None:
                    self.webview.set_zoom_level(self.webview.get_zoom_level() + 0.1)
                return True
            elif keyval in (Gdk.KEY_minus, Gdk.KEY_underscore, Gdk.KEY_KP_Subtract):
                if self.webview is not None:
                    self.webview.set_zoom_level(max(0.5, self.webview.get_zoom_level() - 0.1))
                return True
            elif keyval in (Gdk.KEY_0, Gdk.KEY_KP_0):
                if self.webview is not None:
                    self.webview.set_zoom_level(1.0)
                return True

        # F12 -> Toggle Web Inspector
        if keyval == Gdk.KEY_F12:
            if self.webview is not None:
                inspector = self.webview.get_inspector()
                if inspector is not None:
                    if inspector.is_attached():
                        inspector.close()
                    else:
                        inspector.show()
                    return True

        return False

    def _on_visible_changed(self, widget: Gtk.Widget, _pspec: GObject.ParamSpec) -> None:
        self._write_state()
        if widget.get_visible():
            self._on_unhidden()
        else:
            self._on_hidden()

    def _on_hidden(self) -> None:
        # Memory is released only after the idle timeout, not on every hide
        self._schedule_idle_cleanup()

    def _on_unhidden(self) -> None:
        self._cancel_idle_cleanup()

        # Restore full browser cache model for instant interaction
        if self.context is not None:
            self.context.set_cache_model(WebKit2.CacheModel.WEB_BROWSER)

        if self.webview is not None:
            self.webview.grab_focus()

            # Autofocus prompt textarea so user can immediately type
            self._evaluate_js("""
            (function() {
                try {
                    const el = document.querySelector('#prompt-textarea') ||
                               document.querySelector('textarea, [contenteditable="true"]');
                    if (el) {
                        el.focus();
                    }
                } catch(e) {}
            })();
            """)

    def _schedule_idle_cleanup(self) -> None:
        if self._idle_timeout_id:
            return

        def _on_idle_tick() -> bool:
            if self.window is not None and not self.window.get_visible():
                if self.context is not None:
                    self.context.set_cache_model(WebKit2.CacheModel.DOCUMENT_VIEWER)
                self.soft_clean()
            self._idle_timeout_id = 0
            return False

        timeout_seconds = self._idle_timeout_minutes * 60
        self._idle_timeout_id = GLib.timeout_add_seconds(timeout_seconds, _on_idle_tick)

    def _cancel_idle_cleanup(self) -> None:
        if self._idle_timeout_id:
            GLib.source_remove(self._idle_timeout_id)
            self._idle_timeout_id = 0

    # File System: Upload ------------------------------------------------
    def _on_run_file_chooser(
        self, _webview: WebKit2.WebView, request: WebKit2.FileChooserRequest
    ) -> bool:
        """Handles file upload dialogs using the Wayland XDG Desktop Portal via Gtk.FileChooserNative."""
        dialog = Gtk.FileChooserNative.new(
            "Select File to Upload",
            self.window,
            Gtk.FileChooserAction.OPEN,
            "_Open",
            "_Cancel",
        )
        dialog.set_select_multiple(request.get_select_multiple())

        mime_filter = request.get_mime_types_filter()
        if mime_filter is not None:
            dialog.add_filter(mime_filter)
            dialog.set_filter(mime_filter)

        res = dialog.run()
        if res == Gtk.ResponseType.ACCEPT:
            filenames = dialog.get_filenames()
            if filenames:
                request.select_files(filenames)
            else:
                request.cancel()
        else:
            request.cancel()

        dialog.destroy()
        return True

    # File System: Download ----------------------------------------------
    def _on_download_started(
        self, _context: WebKit2.WebContext, download: WebKit2.Download
    ) -> None:
        download.connect("decide-destination", self._on_decide_destination)
        download.connect("finished", self._on_download_finished)
        download.connect("failed", self._on_download_failed)

    def _on_decide_destination(
        self, download: WebKit2.Download, suggested_filename: str
    ) -> bool:
        """Prompts user for download destination using native portal file save dialog."""
        dialog = Gtk.FileChooserNative.new(
            "Save File",
            self.window,
            Gtk.FileChooserAction.SAVE,
            "_Save",
            "_Cancel",
        )
        dialog.set_current_name(suggested_filename or "download")

        downloads_dir = GLib.get_user_special_dir(GLib.UserDirectory.DIRECTORY_DOWNLOAD)
        if downloads_dir and os.path.isdir(downloads_dir):
            dialog.set_current_folder(downloads_dir)

        res = dialog.run()
        if res == Gtk.ResponseType.ACCEPT:
            filename = dialog.get_filename()
            if filename:
                dest_uri = Path(filename).as_uri()
                download.set_destination(dest_uri)
                download.set_allow_overwrite(True)
                dialog.destroy()
                return True

        download.cancel()
        dialog.destroy()
        return True

    def _on_download_finished(self, download: WebKit2.Download) -> None:
        dest = download.get_destination() or ""
        name = Path(urlparse(dest).path).name if dest else "File"
        send_notification("Download complete", f"Saved {name}")

    def _on_download_failed(
        self, download: WebKit2.Download, error: GLib.Error
    ) -> None:
        send_notification("Download failed", str(error.message))

    # Navigation & External Links ---------------------------------------
    def _on_decide_policy(
        self,
        webview: WebKit2.WebView,
        decision: WebKit2.PolicyDecision,
        decision_type: WebKit2.PolicyDecisionType,
    ) -> bool:
        if decision_type == WebKit2.PolicyDecisionType.RESPONSE:
            if hasattr(decision, "is_mime_type_supported") and not decision.is_mime_type_supported():
                decision.download()
                return True
        elif decision_type == WebKit2.PolicyDecisionType.NEW_WINDOW_ACTION:
            nav_func = getattr(decision, "get_navigation_action", None)
            if nav_func is not None:
                nav_action = nav_func()
                req = nav_action.get_request()
                uri = req.get_uri() if req else ""
                if uri and self._handle_potential_external_link(uri, self._view_url(webview)):
                    decision.ignore()
                    return True
        elif decision_type == WebKit2.PolicyDecisionType.NAVIGATION_ACTION:
            nav_func = getattr(decision, "get_navigation_action", None)
            if nav_func is not None:
                nav_action = nav_func()
                if (
                    nav_action.get_navigation_type() == WebKit2.NavigationType.LINK_CLICKED
                    and nav_action.is_user_gesture()
                ):
                    req = nav_action.get_request()
                    uri = req.get_uri() if req else ""
                    if uri and self._handle_potential_external_link(uri, self._view_url(webview)):
                        decision.ignore()
                        return True
        return False

    def _on_webview_create(
        self, webview: WebKit2.WebView, navigation_action: WebKit2.NavigationAction
    ) -> Gtk.Widget | None:
        req = navigation_action.get_request()
        uri = req.get_uri() if req is not None else ""
        if uri and self._handle_potential_external_link(uri, self._view_url(webview)):
            return None

        # Internal popups (e.g. OAuth sign-in) need a real window, otherwise the login silently fails
        popup = WebKit2.WebView(related_view=webview)
        popup.set_settings(webview.get_settings())
        win = Gtk.Window(transient_for=self.window)
        win.set_default_size(600, 720)
        win.add(popup)
        popup.connect("ready-to-show", lambda _v: win.show_all())
        popup.connect("close", lambda _v: win.destroy())
        return popup

    def _handle_potential_external_link(self, uri: str, provider_url: str) -> bool:
        parsed = urlparse(uri)
        if parsed.scheme not in ("http", "https"):
            return False

        if not is_internal(uri, provider_url):
            try:
                Gio.AppInfo.launch_default_for_uri(uri, None)
            except Exception:
                if Gtk is not None and self.window is not None:
                    Gtk.show_uri_on_window(self.window, uri, Gdk.CURRENT_TIME if Gdk else 0)
            return True
        return False

    # Network recovery --------------------------------------------------
    def _on_webview_load_changed(
        self, webview: WebKit2.WebView, load_event: WebKit2.LoadEvent
    ) -> None:
        if load_event == WebKit2.LoadEvent.STARTED:
            # WebKit's error page emits STARTED right after load-failed: that one must not clear the failure
            if webview in self._error_page_views:
                self._error_page_views.discard(webview)
            else:
                self._failed_views.discard(webview)

    def _on_webview_load_failed(
        self,
        webview: WebKit2.WebView,
        _load_event: WebKit2.LoadEvent,
        _failing_uri: str,
        error: GLib.Error,
    ) -> bool:
        if error.matches(WebKit2.network_error_quark(), WebKit2.NetworkError.CANCELLED) or error.matches(
            WebKit2.policy_error_quark(), WebKit2.PolicyError.FRAME_LOAD_INTERRUPTED_BY_POLICY_CHANGE
        ):
            return False

        self._failed_views.add(webview)
        self._error_page_views.add(webview)
        self._schedule_reload_retry()
        return False

    def _on_network_changed(
        self, _monitor: Gio.NetworkMonitor, available: bool
    ) -> None:
        """Recover as soon as network connectivity is restored."""
        if available:
            self._reload_failed_views()

    def _reload_failed_views(self) -> None:
        for view in list(self._failed_views):
            view.load_uri(self._view_url(view))

    def _schedule_reload_retry(self) -> None:
        if self._reload_timer_id:
            return

        def _retry() -> bool:
            self._reload_timer_id = 0
            self._reload_failed_views()
            return False

        self._reload_timer_id = GLib.timeout_add_seconds(30, _retry)

    def _cancel_reload_retry(self) -> None:
        if not self._reload_timer_id:
            return

        GLib.source_remove(self._reload_timer_id)
        self._reload_timer_id = 0


def main(argv: list[str]) -> int:
    app = LLMApplication()
    return app.run(argv)


if __name__ == "__main__":
    sys.exit(main(sys.argv))

