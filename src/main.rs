use clap::Parser;
use svcnest::{cli::Cli, error::ServiceError};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    #[cfg(windows)]
    if let Err(error) = svcnest::platform::windows::prevent_stdio_inheritance() {
        eprintln!("IO_ERROR: {error:#}");
        std::process::exit(1);
    }
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
