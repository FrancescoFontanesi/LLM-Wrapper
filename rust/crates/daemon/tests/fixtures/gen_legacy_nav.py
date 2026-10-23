#!/usr/bin/env python3
"""Generate ``legacy_nav.json``: how the *legacy* app tells an in-app link from an external one.

Nothing here re-implements the legacy rule.  ``ALLOWED_DOMAINS``, ``base_domain`` and
``is_internal`` are lifted out of ``app.py`` with ``ast`` and executed, so the fixture is produced by
the real code and cannot drift from it.  Only the standard library is needed (no GTK).

The legacy rule has one more step that cannot be executed here because it launches a browser
(``_handle_potential_external_link``): a link is handed to the browser when its scheme is http(s)
*and* ``is_internal`` says no; any other scheme is left to the web view.  That step is checked
against the source text below, so a change in ``app.py`` makes this script fail instead of silently
producing a stale fixture.

Every case carries ``expect``:

  "same"             the Rust verdict is what the legacy app does (allow in the window / open
                     externally)
  "deviation:<code>" the Rust verdict differs on purpose; ``rust`` says what it must be
      ip_provider    legacy ``base_domain`` of an IP address is its last two *numbers*, so any
                     address ending in the same two numbers counted as "internal"
      multi_part_tld legacy ``base_domain`` of ``x.example.co.uk`` is ``co.uk``, i.e. every .co.uk
      cleartext      legacy kept a plain-http link to an internal domain in the window
      credentials    legacy kept a link with user:password@ in the window
      scheme         legacy left ``file:``, ``mailto:`` ... to the web view
      odd_url        backslashes, ``%`` in the host, no host: legacy's parser and WebKit's disagree
                     or the URL is malformed; Rust refuses what it cannot read the same way twice
      trailing_dot   ``chatgpt.com.`` is the same host for WebKit; legacy compared it as different

Run:  python3 gen_legacy_nav.py > legacy_nav.json
"""
import ast
import json
import re
import sys
from pathlib import Path
from urllib.parse import urlparse

APP = Path(__file__).resolve().parents[5] / "app.py"
SRC = APP.read_text()

# the one step we cannot execute: make sure it still reads the way this script assumes
assert 'if parsed.scheme not in ("http", "https"):\n            return False' in SRC, "app.py changed"
assert "if not is_internal(uri, provider_url):" in SRC, "app.py changed"


def lift():
    tree = ast.parse(SRC)
    ns = {"re": re, "urlparse": urlparse}
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(getattr(t, "id", "") == "ALLOWED_DOMAINS" for t in node.targets):
            exec(compile(ast.Module([node], []), "app.py", "exec"), ns)
        if isinstance(node, ast.FunctionDef) and node.name in ("base_domain", "is_internal"):
            exec(compile(ast.Module([node], []), "app.py", "exec"), ns)
    return ns["ALLOWED_DOMAINS"], ns["is_internal"]


ALLOWED, IS_INTERNAL = lift()


def legacy_external(uri, provider):
    """Would the legacy app hand ``uri`` to the browser?"""
    if urlparse(uri).scheme not in ("http", "https"):
        return False
    return not IS_INTERNAL(uri, provider)


CG = "https://chatgpt.com"
GE = "https://gemini.google.com"
CL = "https://claude.ai"
MI = "https://chat.mistral.ai"
UK = "https://chat.example.co.uk"
LH = "http://localhost:3000"
IP = "http://127.0.0.1:8080"
IP2 = "https://203.0.113.5"

S = ("same", None)
# (uri, provider, expect, rust) -- rust is only given for deviations: "allow" | "external" | "block"
CASES = [
    # --- the provider's own site and the domains the app always trusts ---------------------------
    (CG + "/c/abc-123", CG, *S), (CG, CG, *S), ("https://cdn.chatgpt.com/x.js", CG, *S),
    ("https://CHATGPT.COM/", CG, *S), ("https://chatgpt.com:8443/", CG, *S),
    ("https://openai.com/", CG, *S), ("https://auth.openai.com/authorize", CG, *S),
    ("https://accounts.google.com/o/oauth2/v2/auth", CG, *S),
    ("https://appleid.apple.com/auth", CG, *S), ("https://login.microsoftonline.com/common", CG, *S),
    ("https://challenges.cloudflare.com/turnstile", CG, *S), ("https://oaistatic.com/a.js", CG, *S),
    ("https://sentry.io/", CG, *S),
    # --- everything else goes to the browser -------------------------------------------------------
    ("https://example.org/", CG, *S), ("https://notchatgpt.com/", CG, *S),
    ("https://chatgpt.com.evil.example/", CG, *S), ("https://evil.example/chatgpt.com", CG, *S),
    ("https://evil.example/?u=https://chatgpt.com", CG, *S),
    ("https://chatgpt.com@evil.example/", CG, *S), ("https://user:pw@evil.example/", CG, *S),
    ("https://[2001:db8::1]/", CG, *S), ("https://xn--80ak6aa92e.com/", CG, *S),
    # --- the base domain of other providers --------------------------------------------------------
    (GE, GE, *S), ("https://www.google.com/search?q=x", GE, *S), ("https://accounts.google.com/", GE, *S),
    ("https://claude.ai/chat/1", CL, *S), ("https://anthropic.com/", CL, *S), ("https://www.claude.ai/", CL, *S),
    ("https://auth.mistral.ai/login", MI, *S), ("https://mistral.ai/", MI, *S), ("https://example.com/", MI, *S),
    ("https://login.example.co.uk/", UK, *S),
    # --- local providers -----------------------------------------------------------------------------
    ("http://localhost:3000/x", LH, *S), ("http://localhost:9999/", LH, *S), ("https://example.org/", LH, *S),
    ("http://127.0.0.1:8080/x", IP, *S), ("http://127.0.0.1:9/", IP, *S), ("http://localhost:3000/", IP, *S), ("http://127.0.0.2/", IP, *S),
    # --- deliberate deviations ---------------------------------------------------------------------
    ("http://10.9.0.1/", IP, "deviation:ip_provider", "external"),
    ("https://9.9.113.5/", IP2, "deviation:ip_provider", "external"),
    ("https://attacker.co.uk/", UK, "deviation:multi_part_tld", "external"),
    ("https://other.org.uk/", "https://chat.example.org.uk", "deviation:multi_part_tld", "external"),
    ("http://chatgpt.com/x", CG, "deviation:cleartext", "external"),
    ("http://auth.openai.com/", CG, "deviation:cleartext", "external"),
    ("https://user:pw@chatgpt.com/", CG, "deviation:credentials", "external"),
    ("https://chatgpt.com@chatgpt.com/", CG, "deviation:credentials", "external"),
    ("file:///etc/passwd", CG, "deviation:scheme", "block"),
    ("mailto:someone@example.org", CG, "deviation:scheme", "block"),
    ("tel:+123456", CG, "deviation:scheme", "block"),
    ("ftp://example.org/file", CG, "deviation:scheme", "block"),
    ("javascript:alert(1)", CG, "deviation:scheme", "block"),
    ("intent://scan/#Intent;scheme=zxing;end", CG, "deviation:scheme", "block"),
    ("https://evil.example\\@chatgpt.com/", CG, "deviation:odd_url", "block"),
    ("https://chatgpt%2ecom/", CG, "deviation:odd_url", "block"),
    ("https:///x", CG, "deviation:odd_url", "block"),
    ("https://chatgpt.com/ x", CG, "deviation:odd_url", "block"),
    ("https://chatgpt.com./", CG, "deviation:trailing_dot", "allow"),
]


def main():
    out = {"python": sys.version.split()[0], "allowed_domains": sorted(ALLOWED), "cases": []}
    for uri, provider, expect, rust in CASES:
        ext = legacy_external(uri, provider)
        case = {"uri": uri, "provider": provider, "legacy_external": ext, "expect": expect}
        if rust is not None:
            case["rust"] = rust
        # the table must say what the real code does
        if expect == "same":
            assert urlparse(uri).scheme in ("http", "https"), (uri, "same-cases are web links")
        out["cases"].append(case)
    json.dump(out, sys.stdout, indent=1, ensure_ascii=True, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
