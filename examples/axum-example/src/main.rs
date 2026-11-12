use axum_example::create_app;
use vespera::Serve;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    println!("🚀 Server running on http://localhost:3000");

    // `serve_with_graceful_shutdown` stops accepting connections on Ctrl-C and
    // returns once in-flight requests finish (plain `.serve(addr)` runs forever).
    create_app()
        .await
        .serve_with_graceful_shutdown("0.0.0.0:3000", async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
