use axum_example::create_app;
use vespera::Serve;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    println!("🚀 Server running on http://localhost:3000");

    create_app().await.serve("0.0.0.0:3000").await
}
