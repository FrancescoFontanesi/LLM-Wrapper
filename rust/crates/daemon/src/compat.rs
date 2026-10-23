//! How the web view presents itself to the chat sites.
//!
//! The sites' bot checks (Cloudflare Turnstile in particular) loop forever on a `WebKit` that says
//! what it is, so the app, like the legacy one, introduces itself as Chrome on Linux: the
//! `User-Agent`, a user script that makes the JavaScript environment agree with it, and a style
//! sheet that keeps long chats cheap to lay out. One profile for every provider: the legacy app
//! had no per-provider differences and nothing known today calls for any. The Chrome version lives
//! in one place so that keeping it current is a one-line change.

use webkit6::{
    UserContentInjectedFrames, UserContentManager, UserScript, UserScriptInjectionTime,
    UserStyleLevel, UserStyleSheet,
};

/// The Chrome build the app claims to be (stable channel, Linux, July 2026). The legacy app said 131.
pub const CHROME_FULL_VERSION: &str = "151.0.7922.71";

/// Chrome's reduced `User-Agent` carries only the major version.
fn major() -> &'static str {
    CHROME_FULL_VERSION.split('.').next().unwrap_or("0")
}

/// The default `User-Agent`.
pub fn user_agent() -> String {
    format!(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{}.0.0.0 Safari/537.36",
        major()
    )
}

/// Style sheet for every frame: skip layout of messages that are off screen, and do not let code
/// highlighting force a relayout of the page.
pub const STYLESHEET: &str = "\
/* Optimize chat history rendering via offscreen skipping */
div[data-message-author-role] {
    content-visibility: auto !important;
    contain-intrinsic-size: auto 320px !important;
}

/* Prevent expensive layout recalculations in syntax-highlighted code */
pre code {
    contain: layout style !important;
}
";

/// Makes the page see a Chrome-on-Linux environment. `@MAJOR@` and `@FULL@` are filled in by
/// [`script`].
const SCRIPT: &str = r"
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
                        { brand: 'Chromium', version: '@MAJOR@' },
                        { brand: 'Not_A Brand', version: '24' },
                        { brand: 'Google Chrome', version: '@MAJOR@' }
                    ],
                    mobile: false,
                    platform: 'Linux',
                    getHighEntropyValues: function() {
                        return Promise.resolve({
                            architecture: 'x86',
                            bitness: '64',
                            brands: [
                                { brand: 'Chromium', version: '@MAJOR@' },
                                { brand: 'Not_A Brand', version: '24' },
                                { brand: 'Google Chrome', version: '@MAJOR@' }
                            ],
                            fullVersionList: [
                                { brand: 'Chromium', version: '@FULL@' },
                                { brand: 'Not_A Brand', version: '24.0.0.0' },
                                { brand: 'Google Chrome', version: '@FULL@' }
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
";

/// The user script, with the Chrome version filled in.
pub fn script() -> String {
    SCRIPT
        .replace("@MAJOR@", major())
        .replace("@FULL@", CHROME_FULL_VERSION)
}

/// The content manager every view of the app uses: the style sheet and the script, in all frames
/// of every page (the bot checks run in frames of their own).
pub fn content_manager() -> UserContentManager {
    let manager = UserContentManager::new();
    manager.add_style_sheet(&UserStyleSheet::new(
        STYLESHEET,
        UserContentInjectedFrames::AllFrames,
        UserStyleLevel::User,
        &[],
        &[],
    ));
    manager.add_script(&UserScript::new(
        &script(),
        UserContentInjectedFrames::AllFrames,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    ));
    manager
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_is_chrome_on_linux_with_the_reduced_version() {
        let ua = user_agent();
        assert_eq!(
            ua,
            format!("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{}.0.0.0 Safari/537.36", major())
        );
        assert!(ua.is_ascii() && ua.len() < 200);
        assert!(
            major().parse::<u32>().unwrap() >= 131,
            "never older than the legacy app's"
        );
    }

    #[test]
    fn the_script_agrees_with_the_user_agent() {
        let js = script();
        assert!(!js.contains('@'), "an unfilled placeholder");
        assert!(js.contains(&format!(
            "{{ brand: 'Google Chrome', version: '{}' }}",
            major()
        )));
        assert!(js.contains(&format!("version: '{CHROME_FULL_VERSION}'")));
        assert_eq!(
            js.matches('{').count(),
            js.matches('}').count(),
            "unbalanced braces"
        );
        assert_eq!(
            js.matches('(').count(),
            js.matches(')').count(),
            "unbalanced parentheses"
        );
        for needle in [
            "'webdriver'",
            "'vendor'",
            "window.chrome",
            "WEBGL_debug_renderer_info",
            "'userAgentData'",
            "'plugins'",
        ] {
            assert!(js.contains(needle), "{needle}");
        }
    }

    #[test]
    fn the_style_sheet_only_does_layout_containment() {
        assert!(STYLESHEET.contains("content-visibility: auto"));
        assert!(STYLESHEET.contains("contain: layout style"));
        // nothing that could load anything or hide content
        for forbidden in ["url(", "@import", "display: none", "visibility: hidden"] {
            assert!(!STYLESHEET.contains(forbidden), "{forbidden}");
        }
    }
}
