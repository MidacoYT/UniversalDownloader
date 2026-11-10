#[path = "../upc.rs"]
mod upc;

use std::path::Path;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(installer_arg) = args.next() else {
        eprintln!("Usage: unpack_ubisoft.exe <Installer folder> [game output folder]");
        std::process::exit(2);
    };
    let installer = Path::new(&installer_arg);
    let output = args
        .next()
        .map(std::path::PathBuf::from)
        .or_else(|| installer.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| Path::new(".").to_path_buf());

    match upc::unpack_if_present(installer, &output, |percent| {
        eprint!("\rExtracting Ubisoft files: {percent}%");
    }) {
        Ok(true) => eprintln!("\nUbisoft game files extracted to {}.", output.display()),
        Ok(false) => {
            eprintln!("No uplay_install.manifest/disc pair found in the Installer folder.");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("\nExtraction failed: {error}");
            std::process::exit(1);
        }
    }
}
