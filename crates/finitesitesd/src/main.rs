//! Thin binary wrapper; all logic lives in the `finitesitesd` library so
//! integration tests can drive a real in-process server.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match finitesitesd::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("finitesitesd: {message}");
            ExitCode::FAILURE
        }
    }
}
