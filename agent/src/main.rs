use agent::installer::PixiConfig;
use agent::node::Config;
use agent::release::{short, Bundle};
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "agent",
    version,
    about = "pixi-fleet-mini: keep Pixi environments in sync across a fleet of robots"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run an agent (one per robot, plus any number of operator nodes)
    Run(RunArgs),
    /// Pack a release directory and upload it to an agent
    Publish {
        dir: PathBuf,
        #[arg(long)]
        to: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// Tell the fleet which (already published) release to run
    Desire {
        #[arg(long)]
        to: String,
        #[arg(long)]
        hash: String,
        /// Robot that goes first; the others follow only if it ends up healthy
        #[arg(long)]
        canary: Option<String>,
    },
    /// Publish a release directory and make it the desired release in one step
    Rollout {
        dir: PathBuf,
        #[arg(long)]
        to: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        canary: Option<String>,
    },
    /// Mark the robot behind --to as busy (postpones updates) or idle
    Busy {
        #[arg(long)]
        to: String,
        state: OnOff,
    },
    /// Show the fleet as seen by the agent behind --to
    Status {
        #[arg(long)]
        to: String,
    },
    /// Wait until robots run a release (or, with --rejected, have rejected it)
    Wait {
        #[arg(long)]
        to: String,
        #[arg(long)]
        hash: String,
        /// Comma-separated robot ids to wait for
        #[arg(long, value_delimiter = ',')]
        robots: Vec<String>,
        #[arg(long)]
        rejected: bool,
        #[arg(long, default_value_t = 300)]
        timeout_secs: u64,
    },
    /// Print the content hash of a release directory
    Hash { dir: PathBuf },
}

#[derive(Clone, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Args)]
struct RunArgs {
    /// Unique robot id
    #[arg(long)]
    id: String,
    #[arg(long, default_value = "0.0.0.0:8000")]
    listen: String,
    /// URL other nodes use to reach this agent (default: http://127.0.0.1:<port>)
    #[arg(long)]
    advertise: Option<String>,
    #[arg(long, default_value = "./data")]
    data_dir: PathBuf,
    /// Bootstrap peer URL; repeat for several. More peers are discovered via gossip.
    #[arg(long = "peer")]
    peers: Vec<String>,
    #[arg(long, default_value = "pixi")]
    pixi_bin: String,
    /// Do not run the robot application (the robot still installs and health-checks releases)
    #[arg(long)]
    no_app: bool,
    /// Operator-only node: stores releases and sets the desired state, installs nothing itself
    #[arg(long)]
    operator: bool,
    #[arg(long, default_value_t = 2000)]
    gossip_ms: u64,
    #[arg(long, default_value_t = 1000)]
    reconcile_ms: u64,
    #[arg(long, default_value_t = 900)]
    install_timeout_secs: u64,
    #[arg(long, default_value_t = 60)]
    health_timeout_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    match Cli::parse().cmd {
        Cmd::Run(args) => run(args).await,
        Cmd::Publish { dir, to, label } => {
            let hash = agent::client::publish(&dir, label, &to).await?;
            println!("{hash}");
            Ok(())
        }
        Cmd::Desire { to, hash, canary } => {
            let d = agent::client::desire(&to, &hash, canary).await?;
            println!(
                "desired release is now {} (#{})",
                d.release_hash.as_deref().map(short).unwrap_or("-"),
                d.counter
            );
            Ok(())
        }
        Cmd::Rollout {
            dir,
            to,
            label,
            canary,
        } => {
            let hash = agent::client::publish(&dir, label, &to).await?;
            let d = agent::client::desire(&to, &hash, canary).await?;
            println!("rolling out {} (#{})", short(&hash), d.counter);
            Ok(())
        }
        Cmd::Busy { to, state } => agent::client::set_busy(&to, matches!(state, OnOff::On)).await,
        Cmd::Status { to } => agent::client::print_status(&to).await,
        Cmd::Wait {
            to,
            hash,
            robots,
            rejected,
            timeout_secs,
        } => {
            agent::client::wait(
                &to,
                &hash,
                &robots,
                rejected,
                Duration::from_secs(timeout_secs),
            )
            .await
        }
        Cmd::Hash { dir } => {
            println!("{}", Bundle::from_dir(&dir, None)?.hash());
            Ok(())
        }
    }
}

async fn run(args: RunArgs) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&args.listen)
        .await
        .with_context(|| format!("cannot listen on {}", args.listen))?;
    let port = listener.local_addr()?.port();
    let cfg = Config {
        id: args.id.clone(),
        addr: args
            .advertise
            .unwrap_or_else(|| format!("http://127.0.0.1:{port}")),
        data_dir: args.data_dir,
        peers: args.peers,
        pixi: PixiConfig {
            bin: args.pixi_bin,
            install_timeout: Duration::from_secs(args.install_timeout_secs),
            health_timeout: Duration::from_secs(args.health_timeout_secs),
        },
        run_app: !(args.no_app || args.operator),
        operator_only: args.operator,
        gossip_interval: Duration::from_millis(args.gossip_ms),
        reconcile_interval: Duration::from_millis(args.reconcile_ms),
    };
    tracing::info!(
        "agent {} listening on {} (advertised as {})",
        cfg.id,
        args.listen,
        cfg.addr
    );
    let running = agent::start(cfg, listener)?;

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    tracing::info!("shutting down");
    running.shutdown().await;
    Ok(())
}
