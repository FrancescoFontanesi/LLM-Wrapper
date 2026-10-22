//! Property and fuzz-style tests (deterministic, no external crates).
//!
//! Properties:
//!  1. Arbitrary bytes never panic any decoder.
//!  2. Canonical form: if `decode(b)` succeeds then `encode(decode(b)) == b` — a request has exactly
//!     one byte representation, so there is nothing to smuggle past a filter.
//!  3. Every valid request round-trips.
//!  4. Every strict prefix of a valid frame is an error, never a shorter valid message.
//!  5. Mutated valid frames never panic, and any that still decode are canonical.

mod common;

use common::Rng;
use llm_wrapper_protocol::frame::{self, Kind};
use llm_wrapper_protocol::{
    Flags, ProviderName, ProviderRef, ProviderUrl, Request, Response, UserAgent,
};

fn random_request(rng: &mut Rng) -> Request {
    let names = ["a", "chatgpt", "gemini", "my_provider-1", "x", "claude"];
    let urls = [
        "https://chatgpt.com",
        "https://gemini.google.com/app",
        "https://x.example:8443/p?q=1#f",
        "http://localhost:3000/",
        "https://[2001:db8::1]/",
        "http://127.0.0.1:1/",
    ];
    let mut req = Request {
        flags: Flags::from_bits(u16::try_from(rng.below(256)).unwrap()).unwrap(),
        ..Request::default()
    };
    if rng.chance(50) {
        req.idle_timeout_minutes = Some(u32::try_from(1 + rng.below(525_600)).unwrap());
    }
    if rng.chance(50) {
        let ua = rng.string_from(
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 ./;()_-",
            1,
            80,
        );
        req.user_agent = Some(UserAgent::parse(&ua).unwrap());
    }
    if rng.chance(50) {
        req.provider = Some(if rng.chance(50) {
            ProviderRef::Name(ProviderName::parse(rng.pick(&names)).unwrap())
        } else {
            ProviderRef::Url(ProviderUrl::parse(rng.pick(&urls)).unwrap())
        });
    }
    if rng.chance(40) {
        req.add_provider = Some((
            ProviderName::parse(rng.pick(&names)).unwrap(),
            ProviderUrl::parse(rng.pick(&urls)).unwrap(),
        ));
    }
    if rng.chance(40) {
        req.remove_provider = Some(ProviderName::parse(rng.pick(&names)).unwrap());
    }
    req
}

fn assert_canonical_if_ok(body: &[u8]) {
    if let Ok(req) = Request::decode(body) {
        assert_eq!(
            req.encode_body().unwrap(),
            body,
            "non-canonical bytes decoded: {body:02x?}"
        );
    }
}

#[test]
fn arbitrary_bytes_never_panic_and_stay_canonical() {
    let mut rng = Rng::new(0xf00d_0001);
    for _ in 0..200_000 {
        let bytes = rng.bytes(48);
        assert_canonical_if_ok(&bytes);
        let _ = Response::decode(&bytes);
        let _ = frame::read(&mut std::io::Cursor::new(&bytes), Kind::Request);
        let _ = frame::read(&mut std::io::Cursor::new(&bytes), Kind::Response);
    }
    // biased: start from a valid flags field so the field parsers get exercised
    for _ in 0..200_000 {
        let mut bytes = vec![1, 0, 2, 0, 0];
        bytes.extend(rng.bytes(40));
        assert_canonical_if_ok(&bytes);
    }
}

#[test]
fn valid_requests_round_trip_through_frames() {
    let mut rng = Rng::new(0xc0de_0002);
    for _ in 0..20_000 {
        let req = random_request(&mut rng);
        let frame = req.encode().unwrap();
        let body = frame::read(&mut std::io::Cursor::new(&frame), Kind::Request).unwrap();
        assert_eq!(Request::decode(&body).unwrap(), req);
    }
}

#[test]
fn every_strict_prefix_of_a_valid_frame_is_an_error() {
    let mut rng = Rng::new(0xbeef_0003);
    for _ in 0..300 {
        let frame = random_request(&mut rng).encode().unwrap();
        for cut in 0..frame.len() {
            let r = frame::read(&mut std::io::Cursor::new(&frame[..cut]), Kind::Request);
            assert!(r.is_err(), "prefix {cut}/{} parsed as a frame", frame.len());
        }
        // and a body cut short inside a field is never accepted either
        let body = frame[6..].to_vec();
        for cut in 0..body.len() {
            if let Ok(r) = Request::decode(&body[..cut]) {
                // a prefix may end exactly on a field boundary: then it must itself be canonical
                assert_eq!(r.encode_body().unwrap(), &body[..cut]);
            }
        }
    }
}

#[test]
fn mutated_frames_never_panic_and_stay_canonical() {
    let mut rng = Rng::new(0xabad_0004);
    let mut still_valid = 0_u32;
    for _ in 0..40_000 {
        let mut body = random_request(&mut rng).encode_body().unwrap();
        for _ in 0..=rng.below(3) {
            match rng.below(4) {
                0 if !body.is_empty() => {
                    let i = rng.below(body.len());
                    body[i] ^= 1 << rng.below(8);
                }
                1 if !body.is_empty() => {
                    let i = rng.below(body.len());
                    body.remove(i);
                }
                2 => {
                    let i = rng.below(body.len() + 1);
                    body.insert(i, u8::try_from(rng.below(256)).unwrap());
                }
                _ => body.truncate(rng.below(body.len() + 1)),
            }
        }
        if Request::decode(&body).is_ok() {
            still_valid += 1;
        }
        assert_canonical_if_ok(&body);
    }
    assert!(
        still_valid > 0,
        "mutations never produced a valid message; the test is vacuous"
    );
}

/// A peer that never stops sending.
struct Endless(u64);

impl std::io::Read for Endless {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0 += buf.len() as u64;
        buf.fill(b'A');
        Ok(buf.len())
    }
}

#[test]
fn oversized_declarations_do_not_allocate_or_read_unboundedly() {
    // A header claiming a 64 KiB body must be refused before any body is read.
    let mut stream = std::io::Cursor::new([b'L', b'W', 1, 1, 0xff, 0xff].to_vec());
    assert!(frame::read(&mut stream, Kind::Request).is_err());
    assert_eq!(stream.position(), 6);

    // A reader that would yield forever is never read past the declared body.
    let mut r = Endless(0);
    assert!(frame::read(&mut r, Kind::Request).is_err());
    assert!(r.0 <= 64, "read {} bytes from a hostile peer", r.0);
}
