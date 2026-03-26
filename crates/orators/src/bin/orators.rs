use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "orators", about = "Orators terminal UI")]
struct Args {}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let _args = Args::parse();
    orators::tui::run().await
}
