//! Standalone server mode — `cargo run -p rust-jni-demo`.
//!
//! Reuses the exact same `create_app()` and routes as JNI mode.
//! The only difference is the transport: TCP socket instead of
//! `tower::ServiceExt::oneshot`.
//!
//! ```text
//! cargo run -p rust-jni-demo
//! # → http://localhost:3000/health
//! # → http://localhost:3000/documents/validate
//! ```

use rust_jni_demo::create_app;
use vespera::axum;

#[tokio::main]
async fn main() {
    let app = create_app();

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    println!("Server running on http://localhost:3000");
    println!("  GET  /health");
    println!("  POST /documents/validate");

    axum::serve(listener, app).await.unwrap();
}
