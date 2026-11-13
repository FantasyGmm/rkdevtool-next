mod flash;

use crate::{archive, firmware, maintenance, mcp, usb};
use std::{
    ffi::OsString,
    fs::File,
    future::Future,
    io::{self, Read, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const HELP: &str = "rkdevcli [--help | --version]
  scan                          List RockUSB devices as JSON
  inspect FILE                  Inspect a firmware image or archive as JSON
  unpack FILE OUTPUT_DIR        Extract RKFW/RKAF into a new child directory
  flash FILE --device ID --allow-flash [--start-sector LBA | --partition NAME] [--reboot]
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
        Some("scan") => {
            if positional::<0>(args, "scan", "List RockUSB devices as JSON.")?.is_some() {
                run_async(async {
                    print_json(&crate::probe_targets(usb::targets().await?).await?)
                })?;
            }
            Ok(())
        }
        Some("inspect") => {
            if let Some([path]) = positional(
                args,
                "inspect FILE",
                "Inspect RKFW, RKAF, RAW, MiniLoader or a supported image archive as JSON, without USB.",
            )? {
                inspect(&path)?;
            }
            Ok(())
        }
        Some("unpack") => {
            if let Some([path, directory]) = positional(
                args,
                "unpack FILE OUTPUT_DIR",
                "Extract RKFW/RKAF into a new child of an existing directory and print its path.",
            )? {
                unpack(path, directory)?;
            }
            Ok(())
        }
        Some("flash") => flash::run(args),
        Some("maintenance") => maintenance::run_as("rkdevcli maintenance", args),
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
            println!("rkdevcli {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") | None if args.next().is_none() => {
            print_help();
            Ok(())
        }
        _ => Err(format!("Invalid command or arguments\n\n{HELP}")),
    }
}

fn positional<const N: usize>(
    args: impl Iterator<Item = OsString>,
    usage: &str,
    description: &str,
) -> Result<Option<[String; N]>, String> {
    let args = args
        .map(|arg| arg.into_string().map_err(|_| "Arguments must be UTF-8"))
        .collect::<Result<Vec<_>, _>>()?;
    if matches!(args.as_slice(), [arg] if arg == "--help" || arg == "-h") {
        println!("rkdevcli {usage}\n\n{description}");
        return Ok(None);
    }
    if args
        .iter()
        .any(|arg| arg.is_empty() || arg.starts_with('-'))
    {
        return Err(format!("Usage: rkdevcli {usage}"));
    }
    args.try_into()
        .map(Some)
        .map_err(|_| format!("Usage: rkdevcli {usage}"))
}

fn run_async<T>(future: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Cannot initialize the runtime: {e}"))?
        .block_on(future)
}

fn input_file(path: &str) -> Result<(), String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("Cannot inspect {path:?}: {e}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("Choose a nonempty regular file: {path:?}"));
    }
    Ok(())
}

fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    let mut out = io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value).map_err(|e| e.to_string())?;
    writeln!(out).map_err(|e| e.to_string())
}

fn inspect(path: &str) -> Result<(), String> {
    input_file(path)?;
    let mut prefix = Vec::with_capacity(8);
    File::open(path)
        .and_then(|file| file.take(8).read_to_end(&mut prefix))
        .map_err(|e| format!("Cannot read {path:?}: {e}"))?;
    if archive::detect_format(&prefix).is_some() {
        print_json(&archive::inspect_archive(path)?)
    } else {
        print_json(&firmware::inspect(path)?.info)
    }
}

fn unpack(path: String, directory: String) -> Result<(), String> {
    input_file(&path)?;
    run_async(async move {
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        let mut task = tokio::task::spawn_blocking(move || {
            firmware::unpack(&path, &directory, |_| {
                if stop.load(Ordering::Acquire) {
                    Err("Unpack cancelled".into())
                } else {
                    Ok(())
                }
            })
        });
        let result = tokio::select! {
            result = &mut task => result,
            interrupt = tokio::signal::ctrl_c() => {
                if interrupt.is_ok() {
                    cancelled.store(true, Ordering::Release);
                }
                task.await
            }
        };
        let output = result.map_err(|e| format!("Unpack task failed: {e}"))??;
        println!("{output}");
        Ok(())
    })
}
