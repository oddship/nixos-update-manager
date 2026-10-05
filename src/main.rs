use anyhow::Result;
use clap::{Parser, Subcommand};
use nixos_update_manager::backend::{InputPolicy, Store};
use nixos_update_manager::repository::Repository;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    /// Send a desktop result notification (used by background GUI jobs).
    #[arg(long, global = true)]
    notify: bool,
    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Validate a local flake Git repository without changing it.
    Inspect { path: PathBuf },
    /// Make a Git-filtered candidate source snapshot without changing the checkout.
    Snapshot { path: PathBuf, destination: PathBuf },
    /// Check allowed inputs in an isolated snapshot; never build or activate.
    Check {
        path: PathBuf,
        #[arg(long)]
        host: String,
        #[arg(long)]
        update_input: Vec<String>,
        #[arg(long)]
        exclude_input: Vec<String>,
    },
    /// Build the exact recorded candidate; never activate.
    Prepare { id: String },
    /// Refresh the retained output's runtime review without rebuilding.
    Review { id: String },
    /// Read candidate history, including active operations, newest last.
    History,
    /// Preview and validate an exact candidate application.
    ApplyPlan { id: String },
    /// Authenticate and apply, optionally committing the reviewed lock.
    Apply {
        id: String,
        #[arg(long)]
        commit: bool,
        #[arg(long)]
        message: Option<String>,
    },
    /// Retry a requested commit without reapplying the system.
    Commit { id: String },
    /// Request cancellation without waiting for the operation lock.
    Cancel { id: String },
    /// Read a persisted operation record.
    Show { id: String },
    /// Verify source, HEAD, branch, lock and index still match the candidate.
    Fresh { id: String },
}

fn main() {
    if let Err(error) = run() {
        eprintln!(
            "{}",
            serde_json::json!({"schema_version":1,"error":error.to_string()})
        );
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let state_dir = match cli.state_dir {
        Some(path) => {
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()?.join(path)
            }
        }
        None => std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .ok_or_else(|| {
                anyhow::anyhow!("set --state-dir when no XDG state directory is available")
            })?
            // Keep the original state location across the public rename.
            .join("nixos-updates"),
    };
    let Some(command) = cli.command else {
        nixos_update_manager::gui::run(state_dir);
        return Ok(());
    };
    match command {
        Action::Inspect { path } => {
            let repo = Repository::open(&path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"schema_version":1,"repository":repo,"fingerprint":repo.fingerprint()?})
                )?
            );
        }
        Action::Snapshot { path, destination } => {
            let repo = Repository::open(&path)?;
            let snapshot = repo.snapshot(&destination, &repo.fingerprint()?)?;
            println!(
                "{}",
                serde_json::json!({"schema_version":1,"snapshot":snapshot})
            );
        }
        Action::Check {
            path,
            host,
            update_input,
            exclude_input,
        } => {
            let store = Store::open(&state_dir)?;
            let c = store.check(
                &path,
                &host,
                InputPolicy {
                    update: update_input,
                    exclude: exclude_input,
                },
            )?;
            if cli.notify {
                nixos_update_manager::gui::notify_background(&state_dir, &c);
            }
            println!("{}", serde_json::to_string_pretty(&c)?);
            if c.error.is_some() {
                std::process::exit(1);
            }
        }
        Action::Prepare { id } => {
            let c = Store::open(&state_dir)?.prepare(&id)?;
            if cli.notify {
                nixos_update_manager::gui::notify_background(&state_dir, &c);
            }
            println!("{}", serde_json::to_string_pretty(&c)?);
            if c.error.is_some() {
                std::process::exit(1);
            }
        }
        Action::Show { id } => println!(
            "{}",
            serde_json::to_string_pretty(&Store::read(&state_dir, &id)?)?
        ),
        Action::History => println!(
            "{}",
            serde_json::to_string_pretty(&Store::records(&state_dir)?)?
        ),
        Action::Review { id } => {
            let c = Store::open(&state_dir)?.review(&id)?;
            println!("{}", serde_json::to_string_pretty(&c)?);
            if c.error.is_some() {
                std::process::exit(1);
            }
        }
        Action::ApplyPlan { id } => println!(
            "{}",
            serde_json::to_string_pretty(&Store::open(&state_dir)?.apply_plan(&id)?)?
        ),
        Action::Apply {
            id,
            commit,
            message,
        } => {
            let c = Store::open(&state_dir)?.apply(&id, commit, message)?;
            println!("{}", serde_json::to_string_pretty(&c)?);
            if c.error.is_some() {
                std::process::exit(1);
            }
        }
        Action::Commit { id } => {
            let c = Store::open(&state_dir)?.commit(&id)?;
            println!("{}", serde_json::to_string_pretty(&c)?);
            if c.error.is_some() {
                std::process::exit(1);
            }
        }
        Action::Cancel { id } => println!(
            "{}",
            serde_json::json!({"schema_version":1,"requested":Store::request_cancel(&state_dir, &id)?})
        ),
        Action::Fresh { id } => {
            let store = Store::open(&state_dir)?;
            store.require_fresh(&store.load(&id)?)?;
            println!("{}", serde_json::json!({"schema_version":1,"fresh":true}));
        }
    }
    Ok(())
}
