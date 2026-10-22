# Wire protocol, version 1

Between the `llm-wrapper` client and the `llm-wrapperd` daemon, over a unix stream socket named
`$XDG_RUNTIME_DIR/llm-wrapper[-INSTANCE].v1.sock` (mode `0600`, in a `0700` directory). One request
frame, one response frame, then the connection closes. Implemented once, in
`rust/crates/protocol`, and shared by both ends; this page is the contract for any other
implementation. Nothing here is negotiated: the version is in the file name *and* in every header.

All integers are big endian. Text is UTF-8.

## Frame

```
 0      2      3      4          6
 +------+------+------+----------+----------------+
 | 'L''W'| ver=1| kind | body_len |      body      |
 +------+------+------+----------+----------------+
```

| Field | Size | Rule |
|---|---|---|
| magic | 2 | `4c 57`. Anything else → `BadMagic` (an old text client, a stray program). |
| version | 1 | `1`. |
| kind | 1 | `1` request (client → daemon), `2` response (daemon → client). A peer expecting one kind rejects the other. |
| body_len | 2 | At most **8192** for a request, **1024** for a response. Checked *before* any body byte is read. |

Checks run in that order, so an error names the first thing that is wrong. A reader never consumes
more than `6 + limit` bytes whatever the peer sends (test: an endless stream is read for ≤ 64 bytes).

## Request body

A sequence of fields `tag(1) | len(2) | value(len)`, **tags strictly ascending**. Ascending order
makes the encoding *canonical*: for any byte string at most one request decodes from it, and
`encode(decode(b)) == b` (property-tested over 400 000 random and 40 000 mutated inputs). Unknown
tags, repeated tags, out-of-order tags, truncation and trailing bytes are errors; nothing is skipped.

| Tag | Name | Value | Notes |
|---|---|---|---|
| 1 | flags | u16 | **Required.** Bits below; any undefined bit is an error. |
| 2 | idle_timeout | u32 | minutes, 1 ..= 525 600. |
| 3 | user_agent | 1–512 bytes | printable ASCII (0x20–0x7e). |
| 4 | provider | kind(1) + text | kind `0` = provider name, `1` = URL. |
| 5 | add_name | provider name | tags 5 and 6 appear together or not at all. |
| 6 | add_url | URL | |
| 7 | remove | provider name | |

Flag bits: `0x01` toggle, `0x02` show, `0x04` hide, `0x08` new-chat, `0x10` soft-clean,
`0x20` hard-clean, `0x40` background, `0x80` quit.

Values are validated again by the receiver; a client's own checks are a courtesy, never trusted.

* **name** — `[A-Za-z0-9_-]{1,32}`.
* **URL** — ≤ 2048 bytes, printable ASCII without `" < > \ ^ ` { | }`; `https://host[:port][/…]`, or
  `http://` when the host is `localhost`, an IPv4 in `127.0.0.0/8` or `::1`; no `user:pass@`;
  host is a DNS name (labels of `[A-Za-z0-9-]`, ≤ 63 bytes, ≤ 253 total) or a canonical IPv4/IPv6
  literal — a host whose last label is a number must be a canonical dotted quad; port 1–65535; every
  `%` followed by two hex digits.

## Response body

`status(1) | message(0..=512 bytes, UTF-8)`.

| Status | Meaning | Client exit |
|---|---|---|
| 0 | Ok | 0 |
| 1 | Rejected (well-formed, refused; see message) | 1 |
| 2 | Unsupported (version/feature) | 3 |
| 3 | Busy | 1 |
| 4 | Internal | 1 |

The daemon answers **after** it has acted, so the status is the truth (the legacy daemon answered
before acting, which is defect D01). Clients print the message after replacing control characters
and bidirectional overrides with `?`.

## Golden vectors

Pinned in `rust/crates/protocol/tests/wire_vectors.rs`; spaces are only for the reader.

| Command line | Frame |
|---|---|
| *(none)* | `4c57 0101 0005 0100020000` |
| `--toggle` | `4c57 0101 0005 0100020001` |
| `--show --quit` | `4c57 0101 0005 0100020082` |
| `--provider gemini --idle-timeout 30` | `4c57 0101 0016 0100020000 0200040000001e 0400070067656d696e69` |
| `--user-agent UA/1 --show` | `4c57 0101 000c 0100020002 030004 55412f31` |
| `--add-provider m https://x.y` | `4c57 0101 0017 0100020000 050001 6d 06000b 68747470733a2f2f782e79` |
| `--remove-provider claude` | `4c57 0101 000e 0100020000 070006 636c61756465` |

Responses: `Ok` = `4c57 0102 0001 00`; `Rejected "no"` = `4c57 0102 0003 01 6e6f`.

## Client behaviour that is part of the contract

* Connect within 0.8 s; the **whole** reply within 0.8 s (a peer sending one byte at a time does not
  extend it).
* A stale socket (nobody listening) means "no daemon"; a socket that is not a socket, not ours, or
  open to others is a security error, not "no daemon".
* On "no daemon" the client starts the sibling `llm-wrapperd` with the original arguments, except for
  `--quit`, which then has nothing to do.
