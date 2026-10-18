//! The tegata command line

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tegata::config::Loaded;
use tegata::engine::{Approval, Engine};

#[derive(Parser)]
#[command(name = "tegata", version, about = "Create, store, rotate, and fill in secrets")]
struct Cli {
    /// The configuration file (YAML)
    #[arg(
        short = 'f',
        long = "file",
        global = true,
        default_value = "tegata.yaml",
        env = "TEGATA_FILE"
    )]
    file: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the configuration, providers, and the names used in templates
    Check,
    /// Show each resource and secret: generations, enabled versions, readers
    Status,
    /// Create secret containers and read access, create missing values, and re-read data resources
    Apply {
        #[command(flatten)]
        approval: ApprovalArgs,
    },
    /// Create a new generation (copying the current value to `previous` first, if set)
    Rotate {
        #[arg(required = true)]
        resources: Vec<String>,
        #[command(flatten)]
        approval: ApprovalArgs,
    },
    /// Put a value by hand (asked with hidden input on a terminal, otherwise read from stdin)
    Set { resource: String },
    /// Make `previous` equal to the current value (the end of a rotation)
    Settle {
        #[arg(required = true)]
        resources: Vec<String>,
    },
    /// Clean up generations older than the latest: delete what providers created and disable the versions. All resources if none are given
    Prune { resources: Vec<String> },
    /// Fill secrets that have no value from files filled in by hand (for moving to tegata)
    Import { role: String, dir: PathBuf },
    /// Fill `__NAME__` in templates with values from the store (on servers; does not read the configuration)
    Render {
        /// The Secret Manager project (optional on GCP VMs)
        #[arg(long)]
        project: Option<String>,
        /// Use a directory store instead of Secret Manager (for tests)
        #[arg(long)]
        store_dir: Option<PathBuf>,
        templates: PathBuf,
        out: PathBuf,
    },
}

/// Confirmation before creating new generations
#[derive(clap::Args)]
struct ApprovalArgs {
    /// Create without asking, even when the previous value stops working at once
    #[arg(long, conflicts_with = "dry_run")]
    yes: bool,
    /// Only show what would get a new generation; change nothing
    #[arg(long)]
    dry_run: bool,
}

impl ApprovalArgs {
    fn approval(&self) -> Approval {
        if self.dry_run {
            Approval::DryRun
        } else if self.yes {
            Approval::Yes
        } else {
            Approval::Ask
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Command::Render {
        project,
        store_dir,
        templates,
        out,
    } = &cli.command
    {
        let store = tegata::open_render_store(project.clone(), store_dir.as_deref()).await?;
        let r = tegata::render::render_dir(store.as_ref(), templates, out).await?;
        println!("filled {} files", r.files);
        if !r.missing.is_empty() {
            println!(
                "  not in the store: {}",
                r.missing.iter().cloned().collect::<Vec<_>>().join(" ")
            );
        }
        return Ok(());
    }
    let loaded = Loaded::load(&cli.file)?;
    let store = tegata::open_store(&loaded).await?;
    let engine = Engine::new(&loaded, store.as_ref());
    match cli.command {
        Command::Check => {
            let problems = engine.check().await?;
            for p in &problems {
                println!("{p}");
            }
            anyhow::ensure!(problems.is_empty(), "{} problems", problems.len());
            println!("ok");
        }
        Command::Status => engine.status().await?,
        Command::Apply { approval } => engine.with_approval(approval.approval()).apply().await?,
        Command::Rotate {
            resources,
            approval,
        } => {
            engine
                .with_approval(approval.approval())
                .rotate(&resources)
                .await?
        }
        Command::Set { resource } => engine.set(&resource).await?,
        Command::Settle { resources } => engine.settle(&resources).await?,
        Command::Prune { resources } => engine.prune(&resources).await?,
        Command::Import { role, dir } => engine.import(&role, &dir).await?,
        Command::Render { .. } => unreachable!("handled above"),
    }
    Ok(())
}
