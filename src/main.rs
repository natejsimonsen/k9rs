use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = k9rs::cli::Cli::parse();
    if let Err(e) = k9rs::cli::run(cli).await {
        eprintln!("Boom!! {e:#}");
        std::process::exit(1);
    }
}
