//! [`shutdown_runtime`] takes the shared runtime down for good: later
//! dispatches find no runtime and answer `503`, and a second shutdown is a
//! no-op.  Runs in a CHILD process because the runtime is process-global.

use std::time::Duration;

use axum::routing::get;

use super::{
    RUNTIME_SHUT_DOWN_MSG, block_on_shared_runtime, panic_wire, runtime, runtime_unavailable_wire,
    shutdown_runtime,
};

const APP: &str = "shutdown_test";

fn status_and_body(wire: &[u8]) -> (String, Vec<u8>) {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    (
        String::from_utf8_lossy(&wire[4..4 + len]).into_owned(),
        wire[4 + len..].to_vec(),
    )
}

fn request() -> Vec<u8> {
    let header = format!(r#"{{"v":1,"method":"GET","path":"/ok","app":"{APP}"}}"#);
    let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(header.as_bytes());
    wire
}

#[test]
fn panic_wire_flags_the_metadata() {
    let (header, body) = status_and_body(&panic_wire());
    assert!(header.contains(r#""status":500"#), "{header}");
    assert!(header.contains(r#""panicked":"true""#), "{header}");
    assert_eq!(body, b"panic in Rust engine");
}

#[test]
fn shutdown_makes_later_dispatches_answer_503() {
    const CHILD_ENV: &str = "VESPERA_JNI_SHUTDOWN_TEST_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        let output = std::process::Command::new(
            std::env::current_exe().expect("current unit-test executable"),
        )
        .args([
            "--exact",
            "jni_impl::shutdown_tests::shutdown_makes_later_dispatches_answer_503",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("run isolated shutdown child test");
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "isolated shutdown test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    vespera_inprocess::register_app_named(APP, || {
        vespera_inprocess::Router::new().route("/ok", get(|| async { "ok" }))
    });
    let (header, _) = status_and_body(&runtime_unavailable_wire());
    assert!(
        header.contains(r#""status":500"#),
        "build failure stays a 500: {header}"
    );
    let (header, body) = status_and_body(
        &block_on_shared_runtime(vespera_inprocess::dispatch_from_bytes_async(request()))
            .expect("runtime available before shutdown"),
    );
    assert!(header.contains(r#""status":200"#), "{header}");
    assert_eq!(body, b"ok");

    assert!(shutdown_runtime(Duration::from_secs(1)));
    assert!(runtime().is_none());
    assert!(block_on_shared_runtime(async {}).is_none());
    let (header, body) = status_and_body(&runtime_unavailable_wire());
    assert!(header.contains(r#""status":503"#), "{header}");
    assert_eq!(body, RUNTIME_SHUT_DOWN_MSG.as_bytes());
    assert!(
        !shutdown_runtime(Duration::ZERO),
        "second shutdown is a no-op"
    );
}
