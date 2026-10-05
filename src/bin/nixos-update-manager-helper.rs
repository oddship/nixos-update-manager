use anyhow::{Result, ensure};
use clap::Parser;
use nixos_update_manager::activation::{
    activate_exact, activation_lease, system_status, validate_system,
};
use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    system: PathBuf,
    #[arg(long)]
    expected_running: PathBuf,
}

fn run() -> Result<()> {
    let args = Args::parse();
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "run this helper through pkexec"
    );
    let _activation = activation_lease()?;
    validate_system(&args.system)?;
    let initial = system_status()?;
    ensure!(
        initial.running == args.expected_running && initial.profile == args.expected_running,
        "review baseline changed before authentication completed"
    );
    println!(
        "{}",
        serde_json::json!({"schema_version":1,"phase":"authenticated"})
    );
    io::stdout().flush()?;
    // The unprivileged parent checks its checkout after graphical authentication.
    // EOF, any other command, or no parent means no profile/system mutation.
    let mut line = String::new();
    // This timeout expires before mutation; authentication cannot leave an idle
    // privileged helper waiting indefinitely for a vanished parent.
    unsafe {
        libc::alarm(30);
    }
    io::stdin().lock().take(16).read_line(&mut line)?;
    ensure!(
        line == "activate\n",
        "activation was not confirmed by the updater"
    );
    unsafe {
        libc::alarm(0);
    }
    let status = activate_exact(&args.system, &args.expected_running)?;
    println!(
        "{}",
        serde_json::json!({"schema_version":1,"phase":"applied","status":status})
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!(
            "{}",
            serde_json::json!({"schema_version":1,"error":format!("{error:#}"),"actual":system_status().ok()})
        );
        std::process::exit(1);
    }
}
