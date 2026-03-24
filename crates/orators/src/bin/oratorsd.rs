use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "orators=info,orators_linux=info".into()),
        )
        .init();

    let args = orators::daemon::DaemonArgs::parse();
    orators::daemon::run(args).await
}
