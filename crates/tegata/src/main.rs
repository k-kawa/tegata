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
    /// The environment, when the configuration has `environments`
    #[arg(short = 'e', long = "env", global = true, env = "TEGATA_ENV")]
    environment: Option<String>,
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
    /// Check the configuration and providers; with an identity and templates, also that the identity can read every name in them
    #[command(after_help = command_help!("check"))]
    Check {
        /// An identity in the config
        #[arg(requires = "templates")]
        identity: Option<String>,
        /// The templates it renders (a directory, as given to `tegata render`)
        templates: Option<PathBuf>,
    },
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
    Set {
        resource: String,
        #[command(flatten)]
        yes: YesArg,
    },
    /// Make `previous` equal to the current value (the end of a rotation)
    #[command(after_help = command_help!("settle"))]
    Settle {
        #[arg(required = true)]
        resources: Vec<String>,
        #[command(flatten)]
        yes: YesArg,
    },
    /// Clean up generations older than the latest: delete what providers created and disable the versions. All resources if none are given
    #[command(after_help = command_help!("prune"))]
    Prune {
        resources: Vec<String>,
        #[command(flatten)]
        yes: YesArg,
    },
    /// Fill secrets that have no value from files filled in by hand (for moving to tegata)
    #[command(after_help = command_help!("import"))]
    Import {
        /// The templates the files in <DIR> were filled from (a directory)
        templates: PathBuf,
        /// Files with the same names as the templates, filled in by hand
        dir: PathBuf,
        #[command(flatten)]
        yes: YesArg,
    },
    /// Print the configuration of one environment: the shared part with its overrides merged in
    #[command(after_help = command_help!("env"))]
    Env {
        /// An environment under `environments`
        name: String,
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

/// For commands that change the store but make no new generation
#[derive(clap::Args)]
struct YesArg {
    /// Do not ask, even in an environment with `confirm: true`
    #[arg(long)]
    yes: bool,
}

impl YesArg {
    fn approval(&self) -> Approval {
        if self.yes {
            Approval::Yes
        } else {
            Approval::Ask
        }
    }
}

/// Confirmation before creating new generations
#[derive(clap::Args)]
struct ApprovalArgs {
    /// Do not ask: neither in an environment with `confirm: true`, nor when the previous value stops working at once
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
            Command::Check { .. } => command_docs!("check"),
            Command::Status => command_docs!("status"),
            Command::Apply { .. } => command_docs!("apply"),
            Command::Rotate { .. } => command_docs!("rotate"),
            Command::Set { .. } => command_docs!("set"),
            Command::Settle { .. } => command_docs!("settle"),
            Command::Prune { .. } => command_docs!("prune"),
            Command::Import { .. } => command_docs!("import"),
            Command::Migrate => command_docs!("migrate"),
            Command::Env { .. } => command_docs!("env"),
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
    let config_failed = |error| Failed {
        error,
        docs: Some(docs!("reference/configuration.md#environments")),
    };
    if let Command::Env { name } = &cli.command {
        print!(
            "{}",
            tegata::config::merged_yaml(&cli.file, name).map_err(config_failed)?
        );
        return Ok(());
    }
    let names = tegata::config::environment_names(&cli.file).map_err(|error| Failed {
        error,
        docs: Some(docs!("reference/configuration.md")),
    })?;
    if let Command::Migrate = &cli.command {
        if !names.is_empty() {
            return Err(anyhow::anyhow!(
                "{} has environments, so it is in the current form already",
                cli.file.display()
            )
            .into());
        }
    }
    let env = match (&cli.command, cli.environment.as_deref()) {
        // Without -e, check looks at every environment, and they are compared with each other
        (
            Command::Check {
                identity,
                templates,
            },
            None,
        ) if !names.is_empty() => {
            return check_environments(
                &cli.file,
                &names,
                identity.as_deref(),
                templates.as_deref(),
            )
            .await;
        }
        // Providers are the same in every environment
        (Command::Init { .. }, None) => names.first().map(String::as_str),
        (_, env) => env,
    };
    let loaded = Loaded::load_env(&cli.file, env).map_err(|error| Failed {
        error,
        docs: Some(if names.is_empty() {
            docs!("reference/configuration.md")
        } else {
            docs!("reference/configuration.md#environments")
        }),
    })?;
    if let Command::Migrate = &cli.command {
        let (yaml, checks) = loaded.migrate()?;
        print!("{yaml}");
        if !checks.is_empty() {
            eprintln!(
                "# read access no longer comes from templates; to check that each identity can read its templates:"
            );
            for c in checks {
                eprintln!("#   {c}");
            }
        }
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
        Command::Check {
            identity,
            templates,
        } => {
            let pair = identity.as_deref().zip(templates.as_deref());
            let problems = report(&engine.check(pair).await?, "");
            if problems > 0 {
                return Err(anyhow::anyhow!("{problems} problems").into());
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
        Command::Set { resource, yes } => {
            engine.with_approval(yes.approval()).set(&resource).await?
        }
        Command::Settle { resources, yes } => {
            engine
                .with_approval(yes.approval())
                .settle(&resources)
                .await?
        }
        Command::Prune { resources, yes } => {
            engine
                .with_approval(yes.approval())
                .prune(&resources)
                .await?
        }
        Command::Import {
            templates,
            dir,
            yes,
        } => {
            engine
                .with_approval(yes.approval())
                .import(&templates, &dir)
                .await?
        }
        Command::Render { .. } | Command::Init { .. } | Command::Migrate | Command::Env { .. } => {
            unreachable!("handled above")
        }
    }
    Ok(())
}

/// Prints what check found (each line after `prefix`), and returns the number of problems
fn report(checked: &tegata::engine::Checked, prefix: &str) -> usize {
    for w in &checked.warnings {
        println!("warning: {prefix}{w}");
    }
    for p in &checked.problems {
        println!("{prefix}{p}");
    }
    checked.problems.len()
}

/// `tegata check` without -e: every environment, and environments that share a project or a member
async fn check_environments(
    file: &std::path::Path,
    names: &[String],
    identity: Option<&str>,
    templates: Option<&std::path::Path>,
) -> Result<(), Failed> {
    let mut problems = 0;
    let mut loaded = Vec::new();
    for name in names {
        match Loaded::load_env(file, Some(name)) {
            Ok(l) => loaded.push(l),
            Err(e) => {
                println!("{name}: {e:#}");
                problems += 1;
            }
        }
    }
    for l in &loaded {
        let name = l.config.environment.as_deref().unwrap_or_default();
        let store = tegata::open_store(l).await?;
        let checked = Engine::new(l, store.as_ref())
            .check(identity.zip(templates))
            .await?;
        problems += report(&checked, &format!("{name}: "));
    }
    for p in tegata::config::isolation_problems(&loaded) {
        println!("{p}");
        problems += 1;
    }
    if problems > 0 {
        return Err(anyhow::anyhow!("{problems} problems").into());
    }
    println!("ok ({})", names.join(", "));
    Ok(())
}
