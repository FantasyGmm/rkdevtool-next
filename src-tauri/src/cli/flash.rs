use crate::{
    host::Host,
    jobs,
    model::{OperationKind, OperationRequest},
    state::{Backend, JobOrigin},
};
use std::{
    ffi::OsString,
    fmt,
    io::{self, Write},
    sync::Arc,
    time::Duration,
};

const HELP: &str = "Usage: rkdevtool-next-cli flash FILE --device ID --allow-flash
       [--start-sector LBA | --partition NAME] [--reboot]

Without a range selector, flash requires an RKFW/RKAF firmware package.
A RAW image requires an explicit LBA (in 512-byte sectors) or an exact GPT
partition name. Use scan to find the device ID. The device is not rebooted
unless --reboot is supplied. Ctrl+C requests cancellation at a safe boundary;
sectors already written are not rolled back.";

enum Target {
    Package,
    StartSector(u64),
    Partition(String),
}

pub(super) fn run(mut args: impl Iterator<Item = OsString>) -> Result<(), String> {
    let mut file = None;
    let mut device_id = None;
    let mut target = None;
    let mut allow_flash = false;
    let mut reboot = false;
    while let Some(arg) = args.next() {
        match arg.to_str().ok_or("Flash arguments must be valid UTF-8")? {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(());
            }
            "--device" if device_id.is_none() => {
                device_id = Some(value(&mut args, "--device", "ID")?);
            }
            "--allow-flash" if !allow_flash => allow_flash = true,
            "--reboot" if !reboot => reboot = true,
            "--start-sector" | "--partition" if target.is_some() => {
                return Err("Specify only one of --start-sector LBA or --partition NAME".into());
            }
            "--start-sector" => {
                let lba = value(&mut args, "--start-sector", "LBA")?;
                let lba = lba
                    .parse()
                    .map_err(|_| "--start-sector requires a non-negative decimal LBA")?;
                target = Some(Target::StartSector(lba));
            }
            "--partition" => {
                target = Some(Target::Partition(value(&mut args, "--partition", "NAME")?))
            }
            path if file.is_none() && !path.is_empty() && !path.starts_with('-') => {
                file = Some(path.to_owned());
            }
            _ => return Err(format!("Unexpected flash argument {arg:?}\n\n{HELP}")),
        }
    }

    let file = file.ok_or("flash requires FILE; see flash --help")?;
    let device_id =
        device_id.ok_or("flash requires an explicit --device ID; use scan to find it")?;
    if !allow_flash {
        return Err("flash requires --allow-flash to authorize writing to the device".into());
    }
    super::input_file(&file)?;
    super::run_async(flash(
        file,
        device_id,
        target.unwrap_or(Target::Package),
        reboot,
    ))
}

fn value(
    args: &mut impl Iterator<Item = OsString>,
    option: &str,
    label: &str,
) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{option} requires {label}"))?
        .into_string()
        .map_err(|_| format!("{label} must be valid UTF-8"))
        .and_then(|value| {
            if value.trim().is_empty() || value.starts_with('-') {
                Err(format!("{option} requires {label}"))
            } else {
                Ok(value)
            }
        })
}

async fn flash(
    file: String,
    device_id: String,
    target: Target,
    reboot: bool,
) -> Result<(), String> {
    let mut temporary = tempfile::Builder::new();
    temporary.prefix("rkdevtool-flash-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let directory = temporary
        .tempdir()
        .map_err(|e| format!("Cannot create temporary flash state directory: {e}"))?;
    let backend = Arc::new(Backend::load(directory.path().to_owned())?);

    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let (prepared, gate, target_description) = {
        let preflight = async {
            stderr_line(format_args!(
                "Preflight: checking {file:?} for device {device_id:?}"
            ));
            let (start_sector, sectors, partition_name, description) = match &target {
                Target::Package => (0, None, None, "full RKFW/RKAF package".into()),
                Target::StartSector(lba) => (*lba, None, None, format!("RAW image at LBA {lba}")),
                Target::Partition(name) => {
                    let mut matches = crate::partitions(&backend, &device_id)
                        .await?
                        .into_iter()
                        .filter(|partition| partition.name == name.as_str());
                    let partition = matches
                        .next()
                        .ok_or_else(|| format!("No GPT partition named {name:?}"))?;
                    if matches.next().is_some() {
                        return Err(format!("Ambiguous GPT partition name {name:?}"));
                    }
                    (
                        partition.start_sector,
                        Some(partition.sectors),
                        Some(partition.name),
                        format!(
                            "RAW image in partition {name:?} (LBA {}, {} sectors)",
                            partition.start_sector, partition.sectors
                        ),
                    )
                }
            };
            let request = OperationRequest {
                device_id: device_id.clone(),
                kind: OperationKind::Flash,
                firmware_path: Some(file),
                output_path: None,
                start_sector,
                sectors,
                _legacy_verify: None,
                reboot,
                confirmed: true,
                writes: None,
                archive_member: None,
                partition_name,
            };
            let (prepared, gate) = crate::prepare_operation(&backend, request).await?;
            match (&target, prepared.is_firmware_package()) {
                (Target::Package, false) => {
                    return Err(
                        "A RAW image requires --start-sector LBA or --partition NAME".into(),
                    );
                }
                (Target::StartSector(_) | Target::Partition(_), true) => {
                    return Err(
                        "An RKFW/RKAF package cannot use --start-sector or --partition".into(),
                    );
                }
                _ => {}
            }
            Ok::<_, String>((prepared, gate, description))
        };
        tokio::pin!(preflight);
        tokio::select! {
            biased;
            signal = &mut interrupt => return Err(preflight_interrupt(signal)),
            result = &mut preflight => result.map_err(|e| format!("Flash preflight failed: {e}"))?,
        }
    };

    let reboot_label = if reboot { "yes" } else { "no" };
    stderr_line(format_args!(
        "Flash target: device {device_id:?}, {target_description}; reboot: {reboot_label}"
    ));
    // Preparation can finish in the same poll that an interrupt arrives.
    tokio::select! {
        biased;
        signal = &mut interrupt => return Err(preflight_interrupt(signal)),
        () = std::future::ready(()) => {}
    }

    let (id, mut task) = jobs::launch_task(
        Host::default(),
        backend.clone(),
        prepared,
        gate,
        JobOrigin::Cli,
    )?;
    let mut interrupted = false;
    let mut progress = tokio::time::interval(Duration::from_millis(500));
    progress.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_progress = String::new();
    let result = loop {
        tokio::select! {
            biased;
            result = &mut task => break result,
            signal = &mut interrupt, if !interrupted => {
                interrupted = true;
                let cancellation = backend.cancel(&id);
                match signal {
                    Ok(()) => stderr_line(format_args!("Ctrl+C received; waiting for the flash task to stop safely.")),
                    Err(e) => stderr_line(format_args!("Ctrl+C handler failed ({e}); requesting safe cancellation.")),
                }
                if let Err(e) = cancellation {
                    stderr_line(format_args!("Could not request cancellation ({e}); waiting for the flash result."));
                }
            }
            _ = progress.tick() => report_progress(&backend, &id, &mut last_progress),
        }
    }
    .map_err(|e| format!("Flash task stopped unexpectedly: {e}"))??;
    stderr_line(format_args!("Flash complete: {result}"));
    Ok(())
}

fn preflight_interrupt(signal: io::Result<()>) -> String {
    match signal {
        Ok(()) => "Flash cancelled before writing".into(),
        Err(e) => format!("Cannot listen for Ctrl+C; flash did not start: {e}"),
    }
}

fn report_progress(backend: &Backend, id: &str, last: &mut String) {
    let Ok(events) = backend.operation_events(Some(id), 1) else {
        return;
    };
    let Some(event) = events.first().filter(|event| event.status == "running") else {
        return;
    };
    let line = match event.progress {
        Some(percent) => format!("[{} {percent:.1}%] {}", event.stage, event.message),
        None => format!("[{}] {}", event.stage, event.message),
    };
    if *last != line {
        stderr_line(format_args!("{line}"));
        *last = line;
    }
}

fn stderr_line(message: fmt::Arguments<'_>) {
    // A closed progress stream must not unwind and tear down an active job.
    let _ = writeln!(io::stderr(), "{message}");
}
