#!/usr/bin/env python3
"""Generate ``legacy_cli.json``: how the *legacy* Python app interprets its command line.

Nothing here re-implements the legacy parser.  The ``argparse`` definition and the two
validation helpers (``NAME_RE`` / ``is_valid_url``) are lifted out of ``app.py`` with ``ast``
and executed, so the fixture is produced by the real code and cannot drift from it.  It needs
only the Python standard library (no GTK).

Every case also carries ``expect``: what the Rust implementation is required to do.

  "same"             parses to exactly the legacy effective request
  "rejected"         legacy argparse/process rejects it too; Rust must reject it
  "help"/"version"   new in Rust (legacy silently ignores the flag)
  "deviation:<code>" legacy accepts (or ignores) it, Rust deliberately rejects it:
      unknown_option   legacy ``parse_known_args`` silently ignores unknown flags
      abbreviation     legacy accepts unambiguous prefixes (``--tog``)
      positional       legacy silently ignores stray positionals
      invalid_value    legacy parses it and the daemon later refuses/ignores it; Rust says so up front
      url_policy       http to a non-loopback host, userinfo, ... (security hardening)
      strict_int       ``int()`` leniency (underscores, whitespace, non-ASCII digits)
      range            absurd values (e.g. idle timeout of 10^11 minutes)

Run:  python3 gen_legacy_cli.py > legacy_cli.json
"""
import ast
import contextlib
import io
import json
import re
import sys
from pathlib import Path
from urllib.parse import urlparse

APP = Path(__file__).resolve().parents[5] / "legacy" / "app.py"


def lift():
    tree = ast.parse(APP.read_text())
    ns = {"re": re, "urlparse": urlparse}

    # NAME_RE and is_valid_url, exactly as written in app.py
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(getattr(t, "id", "") == "NAME_RE" for t in node.targets):
            exec(compile(ast.Module([node], []), "app.py", "exec"), ns)
        if isinstance(node, ast.FunctionDef) and node.name == "is_valid_url":
            exec(compile(ast.Module([node], []), "app.py", "exec"), ns)

    # the parser: statements of _execute_command_list from `parser = ...` to the last add_argument
    cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "LLMApplication")
    fn = next(n for n in cls.body if isinstance(n, ast.FunctionDef) and n.name == "_execute_command_list")
    body = fn.body
    start = next(i for i, s in enumerate(body) if isinstance(s, ast.Assign) and getattr(s.targets[0], "id", "") == "parser")
    last = max(i for i, s in enumerate(body)
               if isinstance(s, ast.Expr) and "parser.add_argument" in ast.unparse(s))
    import argparse
    ns["argparse"] = argparse
    exec(compile(ast.Module(body[start:last + 1], []), "app.py", "exec"), ns)
    return ns["parser"], ns["NAME_RE"], ns["is_valid_url"]


PARSER, NAME_RE, IS_VALID_URL = lift()
FLAGS = ["toggle", "show", "hide", "new_chat", "soft_clean", "hard_clean", "background", "quit"]


def legacy(args):
    """Return (outcome, effective) the way app.py would act on ``args``."""
    err = io.StringIO()
    try:
        with contextlib.redirect_stderr(err):
            ns, _extra = PARSER.parse_known_args(args)
    except (Exception, SystemExit):  # app.py swallows this and falls back to defaults
        return "rejected", None
    # process-level --instance check (module import time in app.py)
    if ns.instance is not None and not NAME_RE.fullmatch(ns.instance):
        return "rejected", None
    eff = {
        "flags": sorted(f for f in FLAGS if getattr(ns, f)),
        "idle_timeout": max(1, ns.idle_timeout) if ns.idle_timeout is not None else None,
        "user_agent": ns.user_agent if ns.user_agent else None,  # `if args.user_agent:`
        "provider": ns.provider or None,
        "add_provider": list(ns.add_provider) if ns.add_provider else None,
        "remove_provider": ns.remove_provider or None,
        "instance": ns.instance,
    }
    return "parsed", eff


def daemon_would_accept(eff):
    """Would the legacy *daemon* act on the values (it validates after parsing)?"""
    ok = True
    if eff["add_provider"]:
        n, u = eff["add_provider"]
        ok &= bool(NAME_RE.fullmatch(n)) and IS_VALID_URL(u)
    if eff["provider"]:
        ok &= bool(NAME_RE.fullmatch(eff["provider"]) or IS_VALID_URL(eff["provider"]))
    return bool(ok)


S = "same"
CASES = [
    # --- plain flags -------------------------------------------------------------------------
    ([], S), (["--toggle"], S), (["--show"], S), (["--hide"], S), (["--new-chat"], S),
    (["--soft-clean"], S), (["--hard-clean"], S), (["--background"], S), (["--quit"], S),
    (["--toggle", "--show"], S), (["--show", "--hide", "--background"], S), (["--toggle", "--toggle"], S),
    (["--new-chat", "--soft-clean", "--hard-clean", "--show"], S),
    # --- idle timeout ------------------------------------------------------------------------
    (["--idle-timeout", "5"], S), (["--idle-timeout=5"], S), (["--idle-timeout", "0"], S),
    (["--idle-timeout", "-5"], S), (["--idle-timeout", "+7"], S), (["--idle-timeout", "525600"], S),
    (["--idle-timeout", "abc"], "rejected"), (["--idle-timeout"], "rejected"), (["--idle-timeout", "5.5"], "rejected"),
    (["--idle-timeout="], "rejected"), (["--idle-timeout", "--show"], "rejected"),
    (["--idle-timeout", " 5 "], "deviation:strict_int"), (["--idle-timeout", "1_0"], "deviation:strict_int"),
    (["--idle-timeout", "٥"], "deviation:strict_int"),
    (["--idle-timeout", "525601"], "deviation:range"), (["--idle-timeout", "99999999999"], "deviation:range"),
    # --- user agent --------------------------------------------------------------------------
    (["--user-agent", "Mozilla/5.0 (X11; Linux x86_64) Test/1.0"], S), (["--user-agent=Foo/1.0"], S),
    (["--user-agent=Foo=Bar"], S), (["--user-agent", "-foo bar"], S),
    (["--user-agent"], "rejected"), (["--user-agent", "-foo"], "rejected"),
    (["--user-agent", ""], "deviation:invalid_value"),
    (["--user-agent", "a" * 512], S), (["--user-agent", "a" * 513], "deviation:invalid_value"),
    (["--user-agent", "tab\there"], "deviation:invalid_value"),
    (["--user-agent", "naïve"], "deviation:invalid_value"),
    (["--user-agent", "line1\r\nX-Injected: 1"], "deviation:invalid_value"),
    # --- provider ----------------------------------------------------------------------------
    (["--provider", "gemini"], S), (["--provider=claude"], S), (["--provider", "https://chat.mistral.ai"], S),
    (["--provider", "http://localhost:3000"], S), (["--provider", "http://127.0.0.1:8080/"], S),
    (["--provider", "http://[::1]:8080/"], S), (["--provider=https://x.example/?a=b"], S),
    (["--provider", "-5"], S), (["--provider", "a", "--provider", "b"], S),
    (["--provider"], "rejected"), (["--provider", "-x"], "rejected"),
    (["--provider", "http://example.com"], "deviation:url_policy"),
    (["--provider", "https://user:pw@example.com"], "deviation:url_policy"),
    (["--provider", "https://2130706433/"], "deviation:url_policy"),
    (["--provider", "not a url"], "deviation:invalid_value"), (["--provider", "ftp://x.example"], "deviation:invalid_value"),
    (["--provider", "javascript:alert(1)"], "deviation:invalid_value"),
    (["--provider", "file:///etc/passwd"], "deviation:invalid_value"),
    # --- add / remove provider ---------------------------------------------------------------
    (["--add-provider", "mistral", "https://chat.mistral.ai"], S), (["--add-provider", "n" * 32, "https://x.example"], S),
    (["--add-provider", "a"], "rejected"), (["--add-provider"], "rejected"),
    (["--add-provider=a", "https://x.example"], "rejected"),
    (["--add-provider", "a", "b", "c"], "deviation:positional"),
    (["--add-provider", "bad name", "https://x.example"], "deviation:invalid_value"),
    (["--add-provider", "n" * 33, "https://x.example"], "deviation:invalid_value"),
    (["--add-provider", "ok", "javascript:alert(1)"], "deviation:invalid_value"),
    (["--add-provider", "ok", "http://example.com"], "deviation:url_policy"),
    (["--add-provider", "ok", "https://user:pw@example.com"], "deviation:url_policy"),
    (["--remove-provider", "chatgpt"], S), (["--remove-provider"], "rejected"),
    (["--remove-provider", "bad name"], "deviation:invalid_value"),
    # --- instance ----------------------------------------------------------------------------
    (["--instance", "gemini"], S), (["--instance=gemini"], S),
    (["--instance", "gemini", "--provider", "gemini", "--background"], S),
    (["--instance"], "rejected"), (["--instance", ""], "rejected"), (["--instance", "bad name!"], "rejected"),
    (["--instance", "x" * 33], "rejected"),
    # --- unknown / abbreviated / positional ---------------------------------------------------
    (["--toogle"], "deviation:unknown_option"), (["--TOGGLE"], "deviation:unknown_option"),
    ([" --toggle"], "deviation:positional"), (["-t"], "deviation:unknown_option"),
    (["--tog"], "deviation:abbreviation"), (["--prov", "gemini"], "deviation:abbreviation"),
    (["--h"], "rejected"),
    (["foo"], "deviation:positional"), (["https://chatgpt.com"], "deviation:positional"),
    ([""], "deviation:positional"), (["--"], "deviation:unknown_option"),
    (["--toggle", "extra"], "deviation:positional"),
    (["--help"], "help"), (["-h"], "help"), (["--version"], "version"), (["-V"], "version"),
    # --- a few realistic combinations ----------------------------------------------------------
    (["--provider", "gemini", "--add-provider", "x", "https://x.example", "--toggle"], S),
    (["--instance", "work", "--user-agent", "UA/1", "--idle-timeout", "30", "--show"], S),
]


def main():
    out = {"python": sys.version.split()[0], "app_py": "app.py (lifted via ast)", "cases": []}
    for args, expect in CASES:
        outcome, eff = legacy(args)
        case = {"args": args, "legacy": outcome, "expect": expect}
        if eff is not None:
            case["effective"] = eff
            case["legacy_daemon_accepts_values"] = daemon_would_accept(eff)
        # self-consistency of the table: a case claimed "same" must be accepted by legacy end to end
        if expect == "same":
            assert outcome == "parsed" and case["legacy_daemon_accepts_values"], (args, case)
        if expect == "rejected":
            assert outcome == "rejected", (args, case)
        if expect.startswith("deviation:"):
            assert outcome == "parsed", (args, case)
        out["cases"].append(case)
    json.dump(out, sys.stdout, indent=1, ensure_ascii=True, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
