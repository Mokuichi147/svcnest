use clap::Parser;
use svcnest::{cli::Cli, error::ServiceError};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let cli = Cli::parse();
    match svcnest::cli::execute(cli).await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            let code = ServiceError::code(&error);
            eprintln!("{code}: {error:#}");
            std::process::exit(1);
        }
    }
}
