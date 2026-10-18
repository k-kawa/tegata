//! The tegata command line

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tegata::config::Loaded;
use tegata::engine::{Approval, Engine};

/// A page of the documentation on GitHub (`docs!("reference/commands.md#tegata-apply")`)
macro_rules! docs {
    ($page:literal) => {
        concat!("https://github.com/k-kawa/tegata/blob/main/docs/", $page)
    };
}

/// The section of the command reference for one command
macro_rules! command_docs {
    ($name:literal) => {
        concat!(docs!("reference/commands.md#tegata-"), $name)
    };
}

/// The help of one command: where its reference is
macro_rules! command_help {
    ($name:literal) => {
        concat!("Reference: ", command_docs!($name))
    };
}

const MAIN_HELP: &str = concat!(
    "Documentation:\n",
    "  Getting started:  ",
    docs!("guides/getting-started.md"),
    "\n  Commands:         ",
    docs!("reference/commands.md"),
    "\n  Configuration:    ",
    docs!("reference/configuration.md"),
    "\n  Providers:        ",
    docs!("reference/providers.md"),
    "\n  All:              ",
    docs!("README.md"),
    "\nRepository: https://github.com/k-kawa/tegata"
);

#[derive(Parser)]
#[command(
    name = "tegata",
    version,
    about = "Create, store, rotate, and fill in secrets",
    after_help = MAIN_HELP
)]
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
    /// Install the providers that have a `source`, and write tegata.lock
    #[command(after_help = command_help!("init"))]
    Init {
        /// Choose the newest versions the requirements allow, instead of the locked ones
        #[arg(long)]
        upgrade: bool,
    },
    /// Check the configuration, providers, and the names used in templates
    #[command(after_help = command_help!("check"))]
    Check,
    /// Show each resource and secret: generations, enabled versions, readers
    #[command(after_help = command_help!("status"))]
    Status,
    /// Create secret containers and read access, create missing values, and re-read data resources
    #[command(after_help = command_help!("apply"))]
    Apply {
        #[command(flatten)]
        approval: ApprovalArgs,
    },
    /// Create a new generation (copying the current value to `previous` first, if set)
    #[command(after_help = command_help!("rotate"))]
    Rotate {
        #[arg(required = true)]
        resources: Vec<String>,
        #[command(flatten)]
        approval: ApprovalArgs,
    },
    /// Put a value by hand (asked with hidden input on a terminal, otherwise read from stdin)
    #[command(after_help = command_help!("set"))]
    Set { resource: String },
    /// Make `previous` equal to the current value (the end of a rotation)
    #[command(after_help = command_help!("settle"))]
    Settle {
        #[arg(required = true)]
        resources: Vec<String>,
    },
    /// Clean up generations older than the latest: delete what providers created and disable the versions. All resources if none are given
    #[command(after_help = command_help!("prune"))]
    Prune { resources: Vec<String> },
    /// Fill secrets that have no value from files filled in by hand (for moving to tegata)
    #[command(after_help = command_help!("import"))]
    Import {
        /// An identity with `templates`; the files in <DIR> are matched against them
        identity: String,
        dir: PathBuf,
    },
    /// Print the configuration in the current form (for files in the old form: store, roles, secrets)
    #[command(after_help = command_help!("migrate"))]
    Migrate,
    /// Fill `__NAME__` in templates with values from the store (on servers; does not read the configuration)
    #[command(after_help = command_help!("render"))]
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

impl Command {
    /// The section of the command reference for this command
    fn docs(&self) -> &'static str {
        match self {
            Command::Init { .. } => command_docs!("init"),
            Command::Check => command_docs!("check"),
            Command::Status => command_docs!("status"),
            Command::Apply { .. } => command_docs!("apply"),
            Command::Rotate { .. } => command_docs!("rotate"),
            Command::Set { .. } => command_docs!("set"),
            Command::Settle { .. } => command_docs!("settle"),
            Command::Prune { .. } => command_docs!("prune"),
            Command::Import { .. } => command_docs!("import"),
            Command::Migrate => command_docs!("migrate"),
            Command::Render { .. } => command_docs!("render"),
        }
    }
}

/// An error, and the page that helps with it (the command's reference when `None`)
struct Failed {
    error: anyhow::Error,
    docs: Option<&'static str>,
}

impl From<anyhow::Error> for Failed {
    fn from(error: anyhow::Error) -> Self {
        Self { error, docs: None }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let command_docs = cli.command.docs();
    if let Err(f) = run(cli).await {
        eprintln!("error: {:#}", f.error);
        eprintln!("see: {}", f.docs.unwrap_or(command_docs));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Failed> {
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
    let loaded = Loaded::load(&cli.file).map_err(|error| Failed {
        error,
        docs: Some(docs!("reference/configuration.md")),
    })?;
    if let Command::Migrate = &cli.command {
        print!("{}", loaded.migrate()?);
        return Ok(());
    }
    for w in &loaded.warnings {
        eprintln!("warning: {}: {w}", cli.file.display());
    }
    if let Command::Init { upgrade } = &cli.command {
        // TEGATA_PROVIDER_MIRROR: a local copy of releases, for machines without internet access
        let lines = match std::env::var_os("TEGATA_PROVIDER_MIRROR") {
            Some(dir) => {
                let mirror = tegata::install::Mirror(dir.into());
                tegata::install::init(&loaded, *upgrade, &mirror).await?
            }
            None => {
                tegata::install::init(&loaded, *upgrade, &tegata::install::GitHub::new()).await?
            }
        };
        for line in &lines {
            println!("{line}");
        }
        if lines.is_empty() {
            println!("no providers with a source");
        }
        return Ok(());
    }
    let store = tegata::open_store(&loaded).await?;
    let engine = Engine::new(&loaded, store.as_ref());
    match cli.command {
        Command::Check => {
            let checked = engine.check().await?;
            for w in &checked.warnings {
                println!("warning: {w}");
            }
            for p in &checked.problems {
                println!("{p}");
            }
            if !checked.problems.is_empty() {
                return Err(anyhow::anyhow!("{} problems", checked.problems.len()).into());
            }
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
        Command::Import { identity, dir } => engine.import(&identity, &dir).await?,
        Command::Render { .. } | Command::Init { .. } | Command::Migrate => {
            unreachable!("handled above")
        }
    }
    Ok(())
}
