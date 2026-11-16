use aun::daemon::{self, Args};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: arch-update-notifier [OPTIONS]

Background update checker for Arch Linux (repo, AUR, Flatpak, Snap).
Normally started as the systemd user service arch-update-notifier.service.

Options:
  --schema-dir DIR   Load the GSettings schema from DIR (must contain
                     gschemas.compiled). Default: the extension's schemas/
                     directory under $XDG_DATA_HOME, then $XDG_DATA_DIRS.
  --once             Run one check now, print the summary and exit
                     (no D-Bus service, no notification).
  --stderr           Log to stderr instead of the journal.
  -V, --version      Print version and exit.
  -h, --help         Print this help and exit.
";

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args::default();
    let mut it = std::env::args_os().skip(1);
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--schema-dir") => {
                let v = it.next().ok_or("--schema-dir needs a directory")?;
                args.schema_dir = Some(PathBuf::from(v));
            }
            Some(s) if s.starts_with("--schema-dir=") => {
                args.schema_dir = Some(PathBuf::from(&s["--schema-dir=".len()..]));
            }
            Some("--once") => args.once = true,
            Some("--stderr") => args.stderr = true,
            Some("-V" | "--version") => {
                println!("arch-update-notifier {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            Some("-h" | "--help") => {
                print!("{USAGE}");
                return Ok(None);
            }
            _ => return Err(format!("unknown argument: {}", a.to_string_lossy())),
        }
    }
    Ok(Some(args))
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("arch-update-notifier: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let log = aun::logging::init(args.stderr || args.once);

    // current_thread is enough: the work is subprocess and network I/O.
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("arch-update-notifier: cannot start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(daemon::run(args, log)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            eprintln!("arch-update-notifier: {e:#}");
            ExitCode::FAILURE
        }
    }
}
