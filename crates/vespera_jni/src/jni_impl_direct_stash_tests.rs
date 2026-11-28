use super::*;

/// [`stash_put`] on the process-global stash (the tests reopen it first).
fn put(token: Option<u64>, pending: PendingDirectResponse) -> u64 {
    stash_put(token, pending).expect("the stash is open and has room")
}

/// A materialised `400` (malformed wire, no app needed) that overflows
/// an empty buffer.
fn pending() -> PendingDirectResponse {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    match rt.block_on(vespera_inprocess::dispatch_into_async_borrowed_resumable(
        &[0, 0, 0, 1, b'{'],
        &mut [],
    )) {
        ResumableDirectWrite::Overflow(pending) => pending,
        ResumableDirectWrite::Complete(_) => panic!("an empty buffer always overflows"),
    }
}

/// Ceilings for a test stash: `entries` count, `bytes` budget.
const fn limits(entries: usize, bytes: usize) -> Limits {
    Limits { entries, bytes }
}

/// A stash that shares no state with the process-global one.
fn local() -> Stash {
    Stash {
        next: 1,
        entries: BTreeMap::new(),
        held: 0,
        checked_out: 0,
        checked_out_bytes: 0,
        closed: false,
    }
}

#[test]
fn exact_flag_only_for_an_unclamped_size() {
    let max = usize::try_from(jint::MAX).unwrap();
    assert_eq!(handle_flags(max, true, false), FLAG_EXACT);
    assert_eq!(
        handle_flags(max + 1, true, false),
        0,
        "clamped size is not exact"
    );
    assert_eq!(handle_flags(usize::MAX, true, true), FLAG_STREAM);
    assert_eq!(handle_flags(10, false, true), FLAG_STREAM);
}

/// One test: the stash is process-global, so the scenarios must not
/// interleave with each other.
#[test]
fn stash_handle_protocol_and_eviction() {
    // Handle written, redeemable exactly once.
    let mut out = vec![0u8; 64];
    let p = pending();
    let required = p.required_bytes();
    let code = stash_overflow(&mut out, p);
    assert_eq!(code, -jint::try_from(required).unwrap());
    let token = u64::from_be_bytes(out[..8].try_into().unwrap());
    assert_ne!(token, 0);
    assert_eq!(out[8] & FLAG_EXACT, FLAG_EXACT);
    assert_eq!(out[8] & FLAG_STREAM, 0);
    let token = jlong::try_from(token).unwrap();
    assert!(take_stashed(token).is_some());
    assert!(take_stashed(token).is_none(), "redeemed once");
    assert!(take_stashed(-1).is_none());

    // No room for a handle: nothing is written or kept.
    let mut tiny = vec![0xAAu8; 4];
    assert!(stash_overflow(&mut tiny, pending()) < 0);
    assert_eq!(tiny, [0xAA; 4]);
    assert!(lock_stash().entries.is_empty());
    reset_stash();

    // Live entries are never evicted early.  At the hard ceiling a fresh
    // overflow is refused with a retryable 503 wire, and every entry still
    // awaiting redemption stays redeemable (R41-J2-MED1).
    let live: Vec<u64> = (0..MAX_STASHED).map(|_| put(None, pending())).collect();
    assert_eq!(stashed_count(), MAX_STASHED);
    let mut out = vec![0u8; 1024];
    let code = stash_overflow(&mut out, pending());
    let len = usize::try_from(code).expect("a complete 503 wire");
    assert!(wire_status(&out[..len]).contains(r#""status":503"#));
    assert_eq!(stashed_count(), MAX_STASHED, "nothing was evicted");
    for token in live {
        assert!(
            stash_take(token).is_some(),
            "a live entry was evicted under pressure"
        );
    }
    assert_eq!(stashed_count(), 0);
    reset_stash();

    // Expired entries are swept by any access, not only by a put.
    let stale = Instant::now()
        .checked_sub(STASH_TTL + Duration::from_secs(1))
        .expect("monotonic clock far enough from its origin");
    let expired = put(None, pending());
    let live = put(None, pending());
    lock_stash().entries.get_mut(&expired).expect("stashed").0 = stale;
    assert!(stash_take(live).is_some());
    assert!(
        lock_stash().entries.is_empty(),
        "expired entry swept by a take"
    );
    reset_stash();

    // A re-stash over a live token displaces that entry: it leaves the
    // stash and its bytes are released (it is dropped after the lock).
    let each = pending().held_bytes();
    assert!(each > 0, "a materialised wire holds its bytes");
    let live = put(None, pending());
    let _ = put(Some(live), pending());
    assert_eq!(stashed_count(), 1, "the displaced entry left the stash");
    assert_eq!(stashed_bytes(), each, "its bytes were released");
    assert!(stash_take(live).is_some());
    assert_eq!(stashed_bytes(), 0, "removals release their bytes");
    reset_stash();

    // Closed by the shutdown's cleanup: nothing is stored (a new entry or a
    // re-stash), and an overflow answers the retryable 503 wire instead.
    clear_stash();
    assert_eq!(
        stash_put(None, pending()).err(),
        Some(Refusal::Closed),
        "closed refuses a new entry"
    );
    assert_eq!(stash_put(Some(7), pending()).err(), Some(Refusal::Closed));
    let mut out = vec![0u8; 1024];
    let code = stash_overflow(&mut out, pending());
    let len = usize::try_from(code).expect("a complete unavailable wire");
    assert_eq!(
        &out[..len],
        runtime_unavailable_wire().as_slice(),
        "the refused overflow answers the unavailable wire"
    );
    assert_eq!(stashed_count(), 0, "nothing stored after the close");
    reset_stash();
}

/// A re-put over a live token always stores (it replaces its own entry,
/// so the count does not grow), even at the ceiling; nothing else leaves.
/// Uses a local `Stash`, so it does not race the process-global one.
#[test]
fn re_put_at_the_ceiling_keeps_every_other_entry() {
    let mut stash = local();
    let now = Instant::now();
    let caps = limits(3, usize::MAX);
    let mut evicted = Vec::new();
    let tokens: Vec<u64> = (0..3)
        .map(|_| {
            stash
                .put(None, now, pending(), caps, &mut evicted)
                .expect("room")
        })
        .collect();
    let live = *tokens.last().expect("the ceiling is not empty");
    let token = stash
        .put(Some(live), now, pending(), caps, &mut evicted)
        .expect("a re-put is never refused");
    assert_eq!(token, live, "the re-put keeps its token");
    assert_eq!(evicted.len(), 1, "only the displaced live entry leaves");
    assert_eq!(stash.entries.len(), 3, "no entry was lost");
    for token in &tokens {
        assert!(
            stash.entries.contains_key(token),
            "an unrelated entry was evicted by the re-put"
        );
    }
}

/// A fetch that has taken its entry out keeps that entry counted: a fresh
/// stash that the ceiling admits only without the checkout is refused
/// while the fetch runs, and the fetch's re-put under its token stores
/// anyway (its space was never given away).  Releasing the checkout then
/// leaves exactly the re-stashed entries counted.  Local `Stash`, so it
/// does not race the process-global one.
#[test]
fn checked_out_entry_counts_toward_the_ceilings_until_released() {
    let mut stash = local();
    let now = Instant::now();
    let caps = limits(2, usize::MAX);
    let mut evicted = Vec::new();
    let first = stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect("room");
    stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect("room");
    let (taken, bytes) = stash.check_out(first).expect("stashed");
    assert_eq!(stash.entries.len(), 1);
    assert_eq!(stash.checked_out, 1);

    let (refusal, refused) = stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect_err("the checked-out entry counts toward the ceiling");
    assert_eq!(refusal, Refusal::Full);
    drop((refused, taken));

    let restashed = stash
        .put(Some(first), now, pending(), caps, &mut evicted)
        .expect("a re-put is never refused");
    assert_eq!(restashed, first);
    stash.release_checkout(bytes);
    assert_eq!(stash.checked_out, 0);
    assert_eq!(stash.checked_out_bytes, 0);
    assert_eq!(stash.entries.len(), 2);
    assert!(evicted.is_empty(), "nothing live was evicted");
}

/// The count ceiling refuses a fresh stash (`Full`) and hands the response
/// back unstored; a live entry is never displaced to make room.
#[test]
fn full_stash_refuses_a_fresh_entry_and_keeps_the_live_ones() {
    let mut stash = local();
    let now = Instant::now();
    let caps = limits(2, usize::MAX);
    let mut evicted = Vec::new();
    let first = stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect("room");
    let second = stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect("room");
    let (refusal, refused) = stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect_err("the ceiling is reached");
    assert_eq!(refusal, Refusal::Full);
    drop(refused);
    assert!(evicted.is_empty(), "nothing live was evicted");
    assert!(stash.entries.contains_key(&first));
    assert!(stash.entries.contains_key(&second));
}

/// The byte ceiling refuses a fresh stash that would exceed it, while a
/// single response is kept alone when the stash is empty.
#[test]
fn byte_ceiling_refuses_a_fresh_entry_and_keeps_one_alone() {
    let mut stash = local();
    let now = Instant::now();
    let mut evicted = Vec::new();
    let each = pending().held_bytes();
    assert!(each > 0, "a materialised wire holds its bytes");
    let caps = limits(usize::MAX, each * 2);
    stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect("room");
    stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect("room");
    assert_eq!(stash.held, each * 2);
    let (refusal, _refused) = stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect_err("over the byte ceiling");
    assert_eq!(refusal, Refusal::Full);
    assert_eq!(stash.entries.len(), 2, "live entries kept");
    assert_eq!(stash.held, each * 2, "a refused entry counts nothing");

    let mut alone = local();
    let zero = limits(usize::MAX, 0);
    alone
        .put(None, now, pending(), zero, &mut evicted)
        .expect("kept alone in an empty stash");
    assert!(
        alone.put(None, now, pending(), zero, &mut evicted).is_err(),
        "not alone any more"
    );
    assert!(evicted.is_empty());
}

/// Expired entries are swept by a put, so they make room under the count
/// ceiling; they are evicted, live ones never are.
#[test]
fn expired_entries_make_room_under_the_ceiling() {
    let mut stash = local();
    let now = Instant::now();
    let caps = limits(1, usize::MAX);
    let mut evicted = Vec::new();
    let stale = now
        .checked_sub(STASH_TTL + Duration::from_secs(1))
        .expect("monotonic clock far enough from its origin");
    let old = stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect("room");
    stash.entries.get_mut(&old).expect("stashed").0 = stale;
    let fresh = stash
        .put(None, now, pending(), caps, &mut evicted)
        .expect("the expired entry was swept, so there is room");
    assert_eq!(evicted.len(), 1, "the expired entry left");
    assert!(!stash.entries.contains_key(&old));
    assert!(stash.entries.contains_key(&fresh));
}

/// `Stash::insert` hands the displaced entry back (the caller drops it
/// outside the lock) and keeps the held-byte count exact.  Uses a local
/// `Stash`, so it does not race the process-global one.
#[test]
fn insert_returns_the_displaced_entry() {
    let mut stash = local();
    let now = Instant::now();
    let each = pending().held_bytes();
    assert!(stash.insert(5, now, pending()).is_none(), "fresh token");
    let displaced = stash.insert(5, now, pending());
    assert!(displaced.is_some(), "the entry at token 5 is returned");
    assert_eq!(stash.held, each, "the displaced bytes are released");
    assert_eq!(stash.entries.len(), 1);
}

fn wire_status(wire: &[u8]) -> String {
    let len = usize::try_from(u32::from_be_bytes(wire[..4].try_into().unwrap())).unwrap();
    String::from_utf8_lossy(&wire[4..4 + len]).into_owned()
}

/// Round 13.  Runs in a CHILD process: it builds the process-global
/// shared runtime (which fixes the runtime config other tests set) and
/// uses the global stash.
#[test]
fn resumable_dispatch_never_reruns_a_handler() {
    const CHILD_ENV: &str = "VESPERA_JNI_STASH_RESUMABLE_CHILD";
    const APP: &str = "stash_resumable_test";
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if std::env::var_os(CHILD_ENV).is_none() {
        let output = std::process::Command::new(
            std::env::current_exe().expect("current unit-test executable"),
        )
        .args([
            "--exact",
            "jni_impl::direct::stash::tests::resumable_dispatch_never_reruns_a_handler",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("run isolated resumable child test");
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "isolated resumable test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    vespera_inprocess::register_app_named(APP, || {
        vespera_inprocess::Router::new()
            .route(
                "/panic",
                axum::routing::get(|| async {
                    CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    panic!("handler panic under test");
                }),
            )
            .route("/big", axum::routing::get(|| async { "x".repeat(4096) }))
    });
    let request_for = |path: &str| {
        let header = format!(r#"{{"v":1,"method":"GET","path":"{path}","app":"{APP}"}}"#);
        let mut request = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
        request.extend_from_slice(header.as_bytes());
        request
    };
    let request = request_for("/panic");

    // An `out` below the 1 KiB minimum asks for it before the handler runs.
    let mut tiny = vec![0xAAu8; vespera_inprocess::MIN_DIRECT_OUT_BYTES - 1];
    let code = dispatch_resumable(&request, &mut tiny).expect("runtime");
    assert_eq!(
        code,
        direct_overflow_code(vespera_inprocess::MIN_DIRECT_OUT_BYTES)
    );
    assert_eq!(CALLS.load(std::sync::atomic::Ordering::SeqCst), 0);
    // The handle area is zeroed (no stale token from an earlier overflow).
    assert_eq!(tiny[..STASH_HANDLE_LEN], [0; STASH_HANDLE_LEN]);
    assert_eq!(
        tiny[STASH_HANDLE_LEN..],
        [0xAA; vespera_inprocess::MIN_DIRECT_OUT_BYTES - 1 - STASH_HANDLE_LEN]
    );
    // A buffer shorter than the handle area is zeroed in full.
    let mut shorter = [0xAAu8; 4];
    assert!(dispatch_resumable(&request, &mut shorter).is_some());
    assert_eq!(shorter, [0; 4]);

    // A panicking handler's `500` wire fits the minimum: complete, and the
    // handler ran exactly once.
    let mut minimum = vec![0u8; vespera_inprocess::MIN_DIRECT_OUT_BYTES];
    let code = dispatch_resumable(&request, &mut minimum).expect("runtime");
    assert!(code >= 0, "the panic wire fits the minimum: {code}");
    assert_eq!(CALLS.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(wire_status(&minimum).contains(r#""panicked":"true""#));

    // A response overflowing the minimum is stashed: a handle, redeemable
    // without a re-run of its handler.
    let mut small = vec![0u8; vespera_inprocess::MIN_DIRECT_OUT_BYTES];
    let code = dispatch_resumable(&request_for("/big"), &mut small).expect("runtime");
    let required = usize::try_from(-code).expect("an overflow");
    assert!(required > small.len(), "{code}");
    let token = u64::from_be_bytes(small[..8].try_into().unwrap());
    assert_ne!(token, 0, "the overflow left a stash handle");
    let mut big = vec![0u8; required];
    let written = fetch_into(jlong::try_from(token).unwrap(), &mut big);
    assert_eq!(usize::try_from(written).unwrap(), required);
    let header = wire_status(&big);
    assert!(header.contains(r#""status":200"#), "{header}");
    assert_eq!(CALLS.load(std::sync::atomic::Ordering::SeqCst), 1);
    reset_stash();

    // An error wire that overflows `out` without a handle: `-(len)`
    // and a zeroed handle area, never `Integer.MIN_VALUE`.
    let wire = runtime_unavailable_wire();
    let mut out = [0xAAu8; 16];
    assert_eq!(
        write_error_wire(&mut out, &wire),
        direct_overflow_code(wire.len())
    );
    assert_eq!(out[..STASH_HANDLE_LEN], [0; STASH_HANDLE_LEN]);
    assert_eq!(out[STASH_HANDLE_LEN..], [0xAA; 16 - STASH_HANDLE_LEN]);
    let mut fits = vec![0u8; wire.len()];
    assert_eq!(
        write_error_wire(&mut fits, &wire),
        direct_complete_code(wire.len())
    );
    assert_eq!(fits, wire);
}
