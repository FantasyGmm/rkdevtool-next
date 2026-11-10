use std::process::ExitCode;

fn main() -> ExitCode {
    match rkdevtool_next_lib::maintenance::run(std::env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rkdevtool-maintenance: {error}");
            ExitCode::FAILURE
        }
    }
}
