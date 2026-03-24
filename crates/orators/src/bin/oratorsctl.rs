use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = orators::cli::Cli::parse();
    orators::cli::run(cli).await
}
