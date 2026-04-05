//! Gratia Multi-Node Network Simulator
//!
//! Runs N consensus nodes as async tasks within a single process,
//! communicating via in-memory channels. No real networking required.
//!
//! Usage:
//!   cargo run -p gratia-simulator -- --nodes 21 --duration 60 --scenario basic

mod node;
#[allow(dead_code)]
mod network;
mod scenarios;

use clap::Parser;

/// Local multi-node network simulator for Gratia consensus.
#[derive(Parser, Debug)]
#[command(name = "gratia-simulator")]
#[command(about = "Run a local multi-node Gratia consensus simulation")]
struct Args {
    /// Number of nodes to simulate (default: 21).
    #[arg(short, long, default_value_t = 21)]
    nodes: usize,

    /// Duration of the simulation in seconds (default: 60).
    #[arg(short, long, default_value_t = 60)]
    duration: u64,

    /// Scenario to run: basic, partition, churn.
    #[arg(short, long, default_value = "basic")]
    scenario: String,
}

#[tokio::main]
async fn main() {
    // Initialize tracing (logs).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();

    println!("=== Gratia Network Simulator ===");
    println!(
        "Scenario: {} | Nodes: {} | Duration: {}s\n",
        args.scenario, args.nodes, args.duration
    );

    let result = match args.scenario.as_str() {
        "basic" => scenarios::run_basic(args.nodes, args.duration).await,
        "partition" => scenarios::run_partition(args.duration).await,
        "churn" => scenarios::run_churn(args.duration).await,
        other => {
            eprintln!("Unknown scenario: '{}'. Available: basic, partition, churn", other);
            std::process::exit(1);
        }
    };

    print!("{}", result);

    if !result.passed {
        std::process::exit(1);
    }
}
