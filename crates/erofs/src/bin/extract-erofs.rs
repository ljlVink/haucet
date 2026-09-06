use clap::Parser;
use erofs::{CliArgs, run_cli_args};
use std::process::ExitCode;

fn main() -> ExitCode {
    let result = CliArgs::try_parse()
        .map_err(|error| error.to_string())
        .and_then(|args| run_cli_args(args).map_err(|error| error.to_string()));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("extract-erofs: {error}");
            ExitCode::FAILURE
        }
    }
}
