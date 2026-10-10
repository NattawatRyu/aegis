//! Feature `noise`: a C client getting its keys from a relay that is its own
//! backend (`aegis_protocol::noise`), knowing only the relay's public key.
//! The keys it ends with go to `aegis_client_new`, as a backend's would.
//!
//! ```text
//! aegis_noise_hello(pub, 0)        -> send; answer: AEGIS_NOISE_CHALLENGE, cookie
//! aegis_noise_hello(pub, cookie)   -> send; answer: AEGIS_NOISE_WELCOME, keys
//! aegis_client_new(name, keys)     -> the ordinary client
//! ```

use aegis_protocol::noise::{hello, is_hello, Answer, Hello, NoiseError, HELLO_LEN, WELCOME_LEN};
use aegis_protocol::CLIENT_KEYS_LEN;

use crate::client::{AEGIS_ERR_BAD_SEAL, AEGIS_ERR_BUFFER, AEGIS_ERR_MALFORMED};
use crate::{boundary, AEGIS_ERR_ARG, AEGIS_ERR_NULL};

pub const AEGIS_NOISE_PUBLIC_LEN: usize = 32;
pub const AEGIS_NOISE_HELLO_LEN: usize = HELLO_LEN;
/// `aegis_noise_answer`: a cookie; free the handle, hello again with it.
pub const AEGIS_NOISE_CHALLENGE: i32 = 1;
/// `aegis_noise_answer`: the keys, for `aegis_client_new`.
pub const AEGIS_NOISE_WELCOME: i32 = 2;

/// Opaque to C. `None` once spent: welcomed, or a welcome that did not open.
pub struct AegisNoise {
    hello: Option<Hello>,
}

/// A fresh handshake to the relay whose public key is `relay_public`
/// (AEGIS_NOISE_PUBLIC_LEN bytes), carrying `cookie` (0: none yet). The
/// hello (AEGIS_NOISE_HELLO_LEN bytes) goes into `buf`; returns its length.
/// `*out` is written only on success.
#[no_mangle]
pub extern "C" fn aegis_noise_hello(
    relay_public: *const u8,
    cookie: u64,
    out: *mut *mut AegisNoise,
    buf: *mut u8,
    cap: usize,
) -> i32 {
    boundary(|| {
        if relay_public.is_null() || out.is_null() || buf.is_null() {
            return AEGIS_ERR_NULL;
        }
        if cap < HELLO_LEN {
            return AEGIS_ERR_BUFFER;
        }
        // SAFETY: the caller passes AEGIS_NOISE_PUBLIC_LEN bytes (checked
        // non-null), copied out at once.
        let public = unsafe { *relay_public.cast::<[u8; AEGIS_NOISE_PUBLIC_LEN]>() };
        let (h, wire) = hello(&public, (cookie != 0).then_some(cookie));
        // SAFETY: `buf` holds `cap` >= HELLO_LEN bytes; `out` checked non-null.
        unsafe {
            std::ptr::copy_nonoverlapping(wire.as_ptr(), buf, wire.len());
            *out = Box::into_raw(Box::new(AegisNoise { hello: Some(h) }));
        }
        wire.len() as i32
    })
}

/// Read a datagram from the relay. AEGIS_NOISE_CHALLENGE: `*cookie`
/// written. AEGIS_NOISE_WELCOME: AEGIS_CLIENT_KEYS_LEN bytes written to
/// `keys`; the handle is spent. AEGIS_ERR_MALFORMED: not an answer to a
/// hello, nothing changed — read the next. AEGIS_ERR_BAD_SEAL: a welcome
/// that did not open (not this relay, or altered); the handle is spent,
/// start again. A spent handle: AEGIS_ERR_ARG.
#[no_mangle]
pub extern "C" fn aegis_noise_answer(
    n: *mut AegisNoise,
    data: *const u8,
    len: usize,
    cookie: *mut u64,
    keys: *mut u8,
) -> i32 {
    boundary(|| {
        if data.is_null() || cookie.is_null() || keys.is_null() {
            return AEGIS_ERR_NULL;
        }
        // SAFETY: from `aegis_noise_hello`, not freed, one thread at a time
        // (aegis.h); null checked.
        let Some(n) = (unsafe { n.as_mut() }) else { return AEGIS_ERR_NULL };
        let Some(h) = n.hello.as_mut() else { return AEGIS_ERR_ARG };
        // SAFETY: the caller says `data` holds `len` bytes (checked non-null).
        let reply = unsafe { std::slice::from_raw_parts(data, len) };
        match h.answer(reply) {
            Ok(Answer::Challenge(c)) => {
                // SAFETY: null checked above.
                unsafe { *cookie = c };
                AEGIS_NOISE_CHALLENGE
            }
            Ok(Answer::Welcome(k)) => {
                n.hello = None;
                // SAFETY: `keys` holds AEGIS_CLIENT_KEYS_LEN bytes (aegis.h),
                // checked non-null.
                unsafe { std::ptr::copy_nonoverlapping(k.to_bytes().as_ptr(), keys, CLIENT_KEYS_LEN) };
                AEGIS_NOISE_WELCOME
            }
            // Only a welcome-sized answer reaches the handshake state.
            Err(NoiseError::BadHandshake) if len == WELCOME_LEN && is_hello(reply) => {
                n.hello = None;
                AEGIS_ERR_BAD_SEAL
            }
            Err(_) => AEGIS_ERR_MALFORMED,
        }
    })
}

/// NULL is a no-op.
#[no_mangle]
pub extern "C" fn aegis_noise_free(n: *mut AegisNoise) {
    if !n.is_null() {
        // SAFETY: from `aegis_noise_hello`, freed once (aegis.h).
        drop(unsafe { Box::from_raw(n) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::AEGIS_CLIENT_KEYS_LEN;
    use aegis_protocol::noise::{challenge, welcome, RelayStatic};
    use aegis_protocol::{open_up, seal_up, ClientKeys, EdgeKey};
    use std::ptr::null_mut;

    fn start(public: &[u8; 32], cookie: u64) -> (*mut AegisNoise, Vec<u8>) {
        let mut n = null_mut();
        let mut buf = [0u8; AEGIS_NOISE_HELLO_LEN];
        let len = aegis_noise_hello(public.as_ptr(), cookie, &mut n, buf.as_mut_ptr(), buf.len());
        assert_eq!(len, AEGIS_NOISE_HELLO_LEN as i32);
        assert!(!n.is_null());
        (n, buf.to_vec())
    }

    fn answer(n: *mut AegisNoise, reply: &[u8]) -> (i32, u64, [u8; AEGIS_CLIENT_KEYS_LEN]) {
        let (mut cookie, mut keys) = (0, [0u8; AEGIS_CLIENT_KEYS_LEN]);
        let s = aegis_noise_answer(n, reply.as_ptr(), reply.len(), &mut cookie, keys.as_mut_ptr());
        (s, cookie, keys)
    }

    /// Challenged, then welcomed, through the ABI. Strays in between change
    /// nothing; the keys open at the relay's edge; the handle is then spent.
    #[test]
    fn a_c_handshake_is_challenged_then_welcomed() {
        let relay = RelayStatic::generate();
        let edge = EdgeKey::with_epoch([3; 32], 2);
        let (n, _) = start(&relay.public(), 0);
        assert_eq!(answer(n, &[1, 2, 3]).0, AEGIS_ERR_MALFORMED);
        let (s, cookie, _) = answer(n, &challenge(41));
        assert_eq!((s, cookie), (AEGIS_NOISE_CHALLENGE, 41));
        aegis_noise_free(n);

        let (n, wire) = start(&relay.public(), cookie);
        assert_eq!(aegis_protocol::noise::hello_cookie(&wire), Some(41), "the cookie went out");
        let reply = welcome(&relay, &edge, 99, &wire).unwrap();
        assert_eq!(answer(n, &[0; WELCOME_LEN]).0, AEGIS_ERR_MALFORMED, "a session's datagram");
        let (s, _, keys) = answer(n, &reply);
        assert_eq!(s, AEGIS_NOISE_WELCOME);
        let keys = ClientKeys::from_bytes(&keys);
        assert_eq!((keys.sid.epoch(), keys.sid.expires()), (2, 99));
        let mut up = seal_up(&keys, b"join");
        assert_eq!(open_up(&edge, &mut up).map(|(s, f)| (s, f.to_vec())), Ok((keys.sid, b"join".to_vec())));
        assert_eq!(answer(n, &reply).0, AEGIS_ERR_ARG, "spent");
        aegis_noise_free(n);
    }

    /// A welcome altered on the way spends the handle; one welcome-sized
    /// but not an answer to a hello does not.
    #[test]
    fn an_altered_welcome_spends_the_handshake() {
        let relay = RelayStatic::generate();
        let (n, wire) = start(&relay.public(), 5);
        let mut reply = welcome(&relay, &EdgeKey::new([1; 32]), 1, &wire).unwrap();
        reply[40] ^= 1;
        assert_eq!(answer(n, &reply).0, AEGIS_ERR_BAD_SEAL);
        assert_eq!(answer(n, &challenge(3)).0, AEGIS_ERR_ARG, "spent: start again");
        aegis_noise_free(n);
    }

    #[test]
    fn nulls_and_a_short_buffer_are_refused_and_nothing_is_allocated() {
        let public = RelayStatic::generate().public();
        let mut n = null_mut();
        let mut buf = [0u8; AEGIS_NOISE_HELLO_LEN];
        let short = aegis_noise_hello(public.as_ptr(), 0, &mut n, buf.as_mut_ptr(), AEGIS_NOISE_HELLO_LEN - 1);
        assert_eq!(short, AEGIS_ERR_BUFFER);
        assert!(n.is_null(), "*out written on a failure");
        assert_eq!(aegis_noise_hello(std::ptr::null(), 0, &mut n, buf.as_mut_ptr(), buf.len()), AEGIS_ERR_NULL);
        assert_eq!(aegis_noise_hello(public.as_ptr(), 0, null_mut(), buf.as_mut_ptr(), buf.len()), AEGIS_ERR_NULL);
        assert_eq!(answer(null_mut(), &challenge(1)).0, AEGIS_ERR_NULL);
        let (n, _) = start(&public, 0);
        let mut keys = [0u8; AEGIS_CLIENT_KEYS_LEN];
        assert_eq!(aegis_noise_answer(n, buf.as_ptr(), 1, null_mut(), keys.as_mut_ptr()), AEGIS_ERR_NULL);
        aegis_noise_free(n);
        aegis_noise_free(null_mut());
    }
}
