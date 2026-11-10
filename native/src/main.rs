#[tokio::main]
async fn main() {
    std::process::exit(gridstator::cli::run().await);
}
