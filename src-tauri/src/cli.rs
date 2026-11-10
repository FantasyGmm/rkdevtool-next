use crate::{maintenance, mcp};
use std::{ffi::OsString, path::PathBuf};

const HELP: &str = "rkdevtool-next-cli [--help | --version]
  serve --state-dir DIRECTORY [--allow-flash]
  maintenance [--allow-device-write] COMMAND

serve starts a loopback-only MCP server and prints the generated client
configuration path. Use a private state directory separate from the GUI.
The service is read-only unless started with --allow-flash.
Ctrl-C stops accepting requests and waits for any active flash to finish.
Use maintenance --help for standalone RockUSB maintenance commands.";

fn print_help() {
    println!("{HELP}\n\nWarning: {}", mcp::UNATTENDED_FLASH_WARNING);
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let mut args = args.into_iter();
    let command = args.next();
    let name = command
        .as_deref()
        .map(|arg| arg.to_str().ok_or("Invalid command"))
        .transpose()?;
    match name {
        Some("maintenance") => maintenance::run(args),
        Some("serve") => {
            let mut directory = None;
            let mut allow_flash = false;
            while let Some(arg) = args.next() {
                match arg.to_str() {
                    Some("--state-dir") if directory.is_none() => {
                        let path = args
                            .next()
                            .filter(|p| !p.is_empty() && !p.to_string_lossy().starts_with("--"))
                            .ok_or("--state-dir requires a directory")?;
                        directory = Some(PathBuf::from(path));
                    }
                    Some("--allow-flash") if !allow_flash => allow_flash = true,
                    Some("--help" | "-h") => {
                        print_help();
                        return Ok(());
                    }
                    _ => return Err(format!("Invalid serve option {arg:?}\n\n{HELP}")),
                }
            }
            let directory =
                std::path::absolute(directory.ok_or("serve requires --state-dir DIRECTORY")?)
                    .map_err(|e| format!("Invalid state directory: {e}"))?;
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Cannot initialize the MCP runtime: {e}"))?
                .block_on(mcp::serve_headless(directory, allow_flash))
        }
        Some("--version" | "-V") if args.next().is_none() => {
            println!("rkdevtool-next-cli {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") | None if args.next().is_none() => {
            print_help();
            Ok(())
        }
        _ => Err(format!("Invalid command or arguments\n\n{HELP}")),
    }
}
