//! Golden vectors. Any other implementation of the protocol (the daemon, a test double, a
//! third-party client) must produce and accept exactly these bytes. See `docs/wire-protocol.md`.

use llm_wrapper_protocol::frame::{self, Kind};
use llm_wrapper_protocol::{parse_args, Parsed, ProtocolError, Request, Response, Status};

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn request(args: &[&str]) -> Request {
    let v: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    match parse_args(&v).unwrap() {
        Parsed::Run(i) => i.request,
        other => panic!("{other:?}"),
    }
}

/// (command line, frame as hex). Spaces are only for the reader: header | flags | other fields.
const REQUESTS: &[(&[&str], &str)] = &[
    (&[], "4c57 0101 0005 0100020000"),
    (&["--toggle"], "4c57 0101 0005 0100020001"),
    (&["--show", "--quit"], "4c57 0101 0005 0100020082"),
    (
        &["--provider", "gemini", "--idle-timeout", "30"],
        "4c57 0101 0016 0100020000 0200040000001e 0400070067656d696e69",
    ),
    (
        &["--user-agent", "UA/1", "--show"],
        "4c57 0101 000c 0100020002 030004 55412f31",
    ),
    (
        &["--add-provider", "m", "https://x.y"],
        "4c57 0101 0017 0100020000 050001 6d 06000b 68747470733a2f2f782e79",
    ),
    (
        &["--remove-provider", "claude"],
        "4c57 0101 000e 0100020000 070006 636c61756465",
    ),
    (
        &["--provider", "https://chat.mistral.ai/"],
        "4c57 0101 0021 0100020000 040019 01 68747470733a2f2f636861742e6d69737472616c2e61692f",
    ),
];

#[test]
fn request_vectors() {
    for (args, expected) in REQUESTS {
        let req = request(args);
        let frame = req.encode().unwrap();
        assert_eq!(frame, unhex(expected), "{args:?}: got {}", hex(&frame));
        // and back
        let mut cur = std::io::Cursor::new(&frame);
        let body = frame::read(&mut cur, Kind::Request).unwrap();
        assert_eq!(Request::decode(&body).unwrap(), req, "{args:?}");
    }
}

#[test]
fn response_vectors() {
    let ok = unhex("4c57 0102 0001 00");
    assert_eq!(Response::ok().encode().unwrap(), ok);
    let rej = unhex("4c57 0102 0003 01 6e6f");
    assert_eq!(
        Response::with_message(Status::Rejected, "no")
            .encode()
            .unwrap(),
        rej
    );
    let mut cur = std::io::Cursor::new(&rej);
    let r = Response::decode(&frame::read(&mut cur, Kind::Response).unwrap()).unwrap();
    assert_eq!((r.status, r.message.as_str()), (Status::Rejected, "no"));
}

#[test]
fn malformed_frames_and_the_error_each_must_produce() {
    let read_req = |h: &str| {
        let bytes = unhex(h);
        frame::read(&mut std::io::Cursor::new(bytes), Kind::Request)
    };
    let read_resp = |h: &str| {
        let bytes = unhex(h);
        frame::read(&mut std::io::Cursor::new(bytes), Kind::Response)
    };
    // an old text-protocol client (`--toggle\n`) talking to the new daemon
    assert_eq!(read_req("2d2d746f67676c650a"), Err(ProtocolError::BadMagic));
    assert_eq!(
        read_req("4c57 0201 0000"),
        Err(ProtocolError::UnsupportedVersion(2))
    );
    assert_eq!(
        read_req("4c57 0102 0000"),
        Err(ProtocolError::UnexpectedKind(2))
    );
    assert_eq!(
        read_resp("4c57 0101 0000"),
        Err(ProtocolError::UnexpectedKind(1))
    );
    assert_eq!(
        read_req("4c57 0101 2001"),
        Err(ProtocolError::FrameTooLarge {
            len: 8193,
            max: 8192
        })
    );
    assert_eq!(
        read_resp("4c57 0102 0401"),
        Err(ProtocolError::FrameTooLarge {
            len: 1025,
            max: 1024
        })
    );
    assert_eq!(
        read_req("4c57 0101 0005 0100"),
        Err(ProtocolError::Truncated)
    );
    assert_eq!(read_req(""), Err(ProtocolError::Truncated));
}

#[test]
fn worst_case_request_fits_the_limit() {
    let url = format!("https://{}.example/{}", "a".repeat(60), "p".repeat(1900));
    assert!(url.len() <= 2048);
    let name = "n".repeat(32);
    let ua = "u".repeat(512);
    let r = request(&[
        "--user-agent",
        &ua,
        "--provider",
        &url,
        "--add-provider",
        &name,
        &url,
        "--remove-provider",
        &name,
        "--idle-timeout",
        "525600",
        "--toggle",
        "--quit",
    ]);
    let frame = r.encode().unwrap();
    assert!(frame.len() <= 6 + 8192, "{}", frame.len());
}
