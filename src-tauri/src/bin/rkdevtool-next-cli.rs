use std::process::ExitCode;

fn main() -> ExitCode {
    match rkdevtool_next_lib::cli::run(std::env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rkdevtool-next-cli: {error}");
            ExitCode::FAILURE
        }
    }
}
