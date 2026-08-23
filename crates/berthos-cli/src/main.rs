//! `berth` — operator/agent CLI for a Berthos node.
//!
//! This binary does not list inventory, open a wallet, or speak x402.
//! Those live in https://github.com/hexuria/berth-market.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use berthos_node::config::NodeConfig;
use berthos_node::guest::DockerGuest;
use berthos_node::http::{new_state_with_report, reject_if_bind_all};
use berthos_node::probes::{observe, simulate, SimulateCase};
use berthos_node::{berthos_home, evaluate};
use berthos_protocol::{
    CheckStatus, CreateLeaseRequest, DoctorReport, GuestOs, Intent, Lease, DEFAULT_DISK_GIB,
    DEFAULT_MEM_GIB, DEFAULT_VCPU,
};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Parser, Debug)]
#[command(
    name = "berth",
    version,
    about = "Berthos node CLI. Isolated guests only. Payments live in berth-market."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the eligibility doctor. Fail closed. Exit 1 if ineligible.
    Doctor(DoctorArgs),
    /// Node process commands.
    #[command(subcommand)]
    Node(NodeCommand),
    /// Exchange a pairing code for a capability token.
    Pair(PairArgs),
    /// Create a local loopback Linux lease (requires a paired node).
    Up(UpArgs),
}

#[derive(Parser, Debug)]
struct DoctorArgs {
    /// Judge private loopback or public participation.
    #[arg(long, value_enum, default_value = "private")]
    intent: IntentArg,
    /// Simulate a known fail-closed case (smoke tests). Cannot simulate success.
    #[arg(long)]
    simulate: Option<String>,
    /// Print the doctor report as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum NodeCommand {
    /// Start the node on loopback after a green doctor.
    Up(NodeUpArgs),
}

#[derive(Parser, Debug)]
struct NodeUpArgs {
    /// Bind address. Must be loopback.
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,
    /// TCP port.
    #[arg(long, default_value_t = 7432)]
    port: u16,
    /// Private (default) or public participation intent.
    #[arg(long, value_enum, default_value = "private")]
    intent: IntentArg,
}

#[derive(Parser, Debug)]
struct PairArgs {
    /// Node URL. Defaults to loopback.
    #[arg(long, default_value = "http://127.0.0.1:7432")]
    url: String,
    /// Pairing code printed by `berth node up` (XXXX-XXXX).
    #[arg(long)]
    code: String,
}

#[derive(Parser, Debug)]
struct UpArgs {
    /// Guest OS. v1 accepts linux only.
    #[arg(long, default_value = "linux")]
    os: String,
    /// Node URL. Defaults to the last `berth pair` URL.
    #[arg(long)]
    url: Option<String>,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum IntentArg {
    Private,
    Public,
}

impl From<IntentArg> for Intent {
    fn from(value: IntentArg) -> Self {
        match value {
            IntentArg::Private => Intent::Private,
            IntentArg::Public => Intent::Public,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ClientConfig {
    url: String,
    token: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Doctor(args) => cmd_doctor(args),
        Command::Node(NodeCommand::Up(args)) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(cmd_node_up(args))
        }
        Command::Pair(args) => cmd_pair(args),
        Command::Up(args) => cmd_up(args),
    }
}

fn cmd_doctor(args: DoctorArgs) -> Result<()> {
    let report = if let Some(raw) = args.simulate.as_deref() {
        let case = SimulateCase::parse(raw).map_err(|e| anyhow::anyhow!(e))?;
        evaluate(&simulate(case))
    } else {
        let mut config = load_node_config()?;
        config.intent = args.intent.into();
        evaluate(&observe(&config))
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    if report.eligible {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

async fn cmd_node_up(args: NodeUpArgs) -> Result<()> {
    let mut config = load_node_config()?;
    config.intent = args.intent.into();
    config.bind_ip = args
        .bind
        .parse()
        .with_context(|| format!("invalid --bind {}", args.bind))?;
    config.port = args.port;
    reject_if_bind_all(config.bind_ip)?;

    let facts = observe(&config);
    let report = evaluate(&facts);
    print_report(&report);
    if !report.eligible {
        bail!("node refused to start: eligibility doctor failed closed");
    }

    let home = berthos_home();
    std::fs::create_dir_all(&home)?;
    config.save(&home)?;

    let bind = SocketAddr::new(config.bind_ip, config.port);
    let state = new_state_with_report(config, report, Arc::new(DockerGuest));
    let code = state.lock().await.pairing.code.clone();
    eprintln!("pairing code: {code}");
    eprintln!("listening on http://{bind}");
    eprintln!("eligibility: GET http://{bind}/v1/eligibility");
    eprintln!("this repo does not take payments; listings live in https://github.com/hexuria/berth-market");
    eprintln!("next: berth pair --code {code}   then   berth up --os linux");

    berthos_node::serve(state, bind).await?;
    Ok(())
}

fn cmd_pair(args: PairArgs) -> Result<()> {
    let url = args.url.trim_end_matches('/').to_string();
    let body = serde_json::json!({ "code": args.code });
    let resp = ureq::post(&format!("{url}/v1/pair"))
        .set("content-type", "application/json")
        .send_json(body)
        .context("pair request failed")?;
    let value: serde_json::Value = resp.into_json()?;
    let token = value
        .get("token")
        .and_then(|v| v.as_str())
        .context("pair response missing token")?;
    let home = berthos_home();
    std::fs::create_dir_all(&home)?;
    write_client_config(
        &home,
        &ClientConfig {
            url: url.clone(),
            token: token.to_string(),
        },
    )?;
    eprintln!("paired with {url}");
    eprintln!(
        "token stored in {} (mode 0600)",
        home.join("client.toml").display()
    );
    eprintln!("quoted occupancy is not charged in this repo");
    Ok(())
}

fn cmd_up(args: UpArgs) -> Result<()> {
    let os = parse_os(&args.os)?;
    if os != GuestOs::Linux {
        bail!("v1 only accepts --os linux");
    }
    let client = read_client_config(&berthos_home()).context("not paired; run berth pair")?;
    let url = args
        .url
        .as_deref()
        .unwrap_or(&client.url)
        .trim_end_matches('/');
    let req = CreateLeaseRequest {
        os,
        vcpu: Some(DEFAULT_VCPU),
        mem_gib: Some(DEFAULT_MEM_GIB),
        disk_gib: Some(DEFAULT_DISK_GIB),
    };
    let resp = ureq::post(&format!("{url}/v1/leases"))
        .set("authorization", &format!("Bearer {}", client.token))
        .set("content-type", "application/json")
        .send_json(serde_json::to_value(&req)?)
        .context("lease create failed")?;
    let lease: Lease = resp.into_json()?;
    println!("lease {}", lease.id);
    println!(
        "quote {} occupancy-seconds (min {}s) notional ${}/hr — not charged",
        lease.quote.occupancy_unit_label(),
        lease.quote.min_seconds,
        lease.quote.notional_usd_per_hour
    );
    println!("settlement: {}", lease.quote.settlement.note);
    Ok(())
}

fn parse_os(raw: &str) -> Result<GuestOs> {
    match raw.to_ascii_lowercase().as_str() {
        "linux" => Ok(GuestOs::Linux),
        "macos" | "mac" => Ok(GuestOs::Macos),
        "windows" | "windows-home-oem" => Ok(GuestOs::WindowsHomeOem),
        "windows-pro-oem" => Ok(GuestOs::WindowsProOem),
        other => bail!("unknown --os {other}; v1 accepts linux"),
    }
}

fn load_node_config() -> Result<NodeConfig> {
    NodeConfig::load(&berthos_home()).context("load node.toml")
}

fn print_report(report: &DoctorReport) {
    eprintln!("Berthos doctor (fail-closed)  intent={:?}", report.intent);
    eprintln!();
    for check in &report.checks {
        let mark = match check.status {
            CheckStatus::Pass => "pass",
            CheckStatus::Fail => "FAIL",
            CheckStatus::Warn => "warn",
        };
        eprintln!("  [{mark:4}] {:<12} {}", check.id, check.detail);
    }
    eprintln!();
    if report.eligible {
        eprintln!("eligible: yes");
    } else {
        eprintln!("eligible: no — this node cannot participate until every required check passes");
    }
    eprintln!("payments/listings: not this repo — https://github.com/hexuria/berth-market");
}

fn write_client_config(home: &std::path::Path, cfg: &ClientConfig) -> Result<()> {
    let path: PathBuf = home.join("client.toml");
    let text = toml::to_string_pretty(cfg)?;
    std::fs::write(&path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn read_client_config(home: &std::path::Path) -> Result<ClientConfig> {
    let path = home.join("client.toml");
    let text = std::fs::read_to_string(&path)?;
    Ok(toml::from_str(&text)?)
}

// Small helper so clap ValueEnum isn't required on the protocol crate.
trait OccupancyLabel {
    fn occupancy_unit_label(&self) -> &'static str;
}

impl OccupancyLabel for berthos_protocol::Quote {
    fn occupancy_unit_label(&self) -> &'static str {
        "seconds"
    }
}
