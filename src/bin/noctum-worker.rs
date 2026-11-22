#[tokio::main]
async fn main() -> anyhow::Result<()> {
    noctum::worker::serve().await
}
