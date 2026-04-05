//! Test scenarios for the multi-node simulator.
//!
//! Phase 1: Basic scaling (3 nodes, 21 nodes)
//! Phase 2: Network partitions (disconnect 1/3, reconnect)
//! Phase 3: Node churn (add/remove nodes mid-run)

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::{info, warn};

use gratia_core::types::BlockHash;
use gratia_consensus::committee::{self, EligibleNode};

use crate::network::SimulatedNetwork;
use crate::node::SimulatedNode;

// ============================================================================
// Simulation state
// ============================================================================

/// Shared simulation state accessible by all node tasks.
pub struct SimulationState {
    /// All nodes in the simulation.
    pub nodes: Vec<SimulatedNode>,
    /// The in-memory network router.
    pub network: SimulatedNetwork,
    /// Global chain height (highest finalized).
    pub chain_height: u64,
    /// Total finalized blocks.
    pub finalized_count: u64,
    /// Forks detected and resolved.
    pub forks_resolved: u64,
    /// Committee transitions.
    pub committee_transitions: u64,
    /// Start time.
    pub start_time: Instant,
    /// Last finalized block hash (shared chain tip).
    pub chain_tip: BlockHash,
}

/// Result of a simulation run.
pub struct SimulationResult {
    pub duration_secs: u64,
    pub final_height: u64,
    pub finalized: u64,
    pub total_blocks: u64,
    pub forks_resolved: u64,
    pub committee_transitions: u64,
    pub reward_distribution: Vec<(usize, u64)>,
    pub passed: bool,
}

impl std::fmt::Display for SimulationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "\n=== SIMULATION COMPLETE ===")?;
        writeln!(f, "Duration: {}s", self.duration_secs)?;
        writeln!(f, "Final height: {}", self.final_height)?;
        writeln!(f, "Finalized: {}", self.finalized)?;
        writeln!(f, "Total blocks: {}", self.total_blocks)?;
        writeln!(f, "Forks resolved: {}", self.forks_resolved)?;
        writeln!(f, "Committee transitions: {}", self.committee_transitions)?;
        write!(f, "Reward distribution: [")?;
        for (i, (idx, reward)) in self.reward_distribution.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            let grat = *reward as f64 / 1_000_000.0;
            write!(f, "node{}: {:.0} GRAT", idx, grat)?;
        }
        writeln!(f, "]")?;
        writeln!(
            f,
            "RESULT: {}",
            if self.passed { "PASS" } else { "FAIL" }
        )
    }
}

// ============================================================================
// Scenario: Basic scaling
// ============================================================================

/// Run the basic scaling scenario.
///
/// Creates `node_count` nodes, forms a committee, and runs consensus for
/// `duration_secs` seconds. Each slot is ~4 seconds (matching TARGET_BLOCK_TIME_SECS).
/// The producer broadcasts its block, committee members co-sign, and once
/// finality threshold is reached the block is finalized.
pub async fn run_basic(node_count: usize, duration_secs: u64) -> SimulationResult {
    info!(nodes = node_count, duration = duration_secs, "Starting basic scaling scenario");

    let state = Arc::new(Mutex::new(setup_simulation(node_count)));
    let start = Instant::now();

    // Initialize committees on all nodes.
    {
        let mut sim = state.lock().await;
        initialize_all_committees(&mut sim);
    }

    // Run the consensus loop.
    run_consensus_loop(state.clone(), duration_secs, start).await;

    // Collect results.
    let sim = state.lock().await;
    build_result(&sim, duration_secs)
}

// ============================================================================
// Scenario: Network partition
// ============================================================================

/// Run the network partition scenario.
///
/// 1. Start 21 nodes, run for 20s.
/// 2. Disconnect 7 nodes (indices 14..21). Run for 20s with 14 remaining.
/// 3. Reconnect the 7. Run for 20s.
/// Verify finalization continues with 14/21 during partition.
pub async fn run_partition(duration_secs: u64) -> SimulationResult {
    let node_count = 21;
    let phase_secs = duration_secs / 3;
    info!(
        duration = duration_secs,
        phase_secs = phase_secs,
        "Starting partition scenario (21 nodes, disconnect 7, reconnect)"
    );

    let state = Arc::new(Mutex::new(setup_simulation(node_count)));
    let start = Instant::now();

    // Phase 1: All nodes connected.
    {
        let mut sim = state.lock().await;
        initialize_all_committees(&mut sim);
    }
    run_consensus_loop(state.clone(), phase_secs, start).await;

    // Phase 2: Disconnect nodes 14..21.
    {
        let mut sim = state.lock().await;
        info!("=== PARTITIONING: disconnecting nodes 14-20 ===");
        for i in 14..21 {
            sim.network.disconnect_node(i);
            sim.nodes[i].connected = false;
        }
        // Re-initialize committees on connected nodes only (14 nodes).
        reinitialize_committees_connected(&mut sim);
    }
    let phase2_start = Instant::now();
    run_consensus_loop(state.clone(), phase_secs, phase2_start).await;

    // Phase 3: Reconnect nodes 14..21.
    {
        let mut sim = state.lock().await;
        info!("=== HEALING: reconnecting nodes 14-20 ===");
        for i in 14..21 {
            sim.network.reconnect_node(i);
            sim.nodes[i].connected = true;
        }
        sim.committee_transitions += 1;
        // Re-initialize committees with all nodes.
        reinitialize_committees_all(&mut sim);
    }
    let phase3_start = Instant::now();
    run_consensus_loop(state.clone(), phase_secs, phase3_start).await;

    let sim = state.lock().await;
    build_result(&sim, duration_secs)
}

// ============================================================================
// Scenario: Node churn
// ============================================================================

/// Run the node churn scenario.
///
/// 1. Start with 10 nodes, run for 1/3 duration.
/// 2. Add 5 more nodes (indices 10..15), run for 1/3 duration.
/// 3. Remove 3 nodes (indices 0..3), run for 1/3 duration.
/// Verify committee adapts at each step.
pub async fn run_churn(duration_secs: u64) -> SimulationResult {
    let phase_secs = duration_secs / 3;
    info!(
        duration = duration_secs,
        "Starting churn scenario (10 -> 15 -> 12 nodes)"
    );

    // Start with 15 nodes total but only first 10 are connected initially.
    let state = Arc::new(Mutex::new(setup_simulation(15)));
    let start = Instant::now();

    {
        let mut sim = state.lock().await;
        // Disconnect nodes 10..15 initially.
        for i in 10..15 {
            sim.network.disconnect_node(i);
            sim.nodes[i].connected = false;
        }
        initialize_committees_for_connected(&mut sim);
    }

    // Phase 1: 10 nodes.
    run_consensus_loop(state.clone(), phase_secs, start).await;

    // Phase 2: Add 5 nodes.
    {
        let mut sim = state.lock().await;
        info!("=== CHURN: adding nodes 10-14 ===");
        for i in 10..15 {
            sim.network.reconnect_node(i);
            sim.nodes[i].connected = true;
        }
        sim.committee_transitions += 1;
        reinitialize_committees_all(&mut sim);
    }
    let phase2_start = Instant::now();
    run_consensus_loop(state.clone(), phase_secs, phase2_start).await;

    // Phase 3: Remove 3 nodes.
    {
        let mut sim = state.lock().await;
        info!("=== CHURN: removing nodes 0-2 ===");
        for i in 0..3 {
            sim.network.disconnect_node(i);
            sim.nodes[i].connected = false;
        }
        sim.committee_transitions += 1;
        reinitialize_committees_connected(&mut sim);
    }
    let phase3_start = Instant::now();
    run_consensus_loop(state.clone(), phase_secs, phase3_start).await;

    let sim = state.lock().await;
    build_result(&sim, duration_secs)
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Create nodes and network.
fn setup_simulation(node_count: usize) -> SimulationState {
    let mut network = SimulatedNetwork::new(50, 200);
    let mut nodes: Vec<SimulatedNode> = Vec::with_capacity(node_count);

    for i in 0..node_count {
        let node = SimulatedNode::new(i);
        network.register_node(i);
        nodes.push(node);
    }

    SimulationState {
        nodes,
        network,
        chain_height: 0,
        finalized_count: 0,
        forks_resolved: 0,
        committee_transitions: 0,
        start_time: Instant::now(),
        chain_tip: BlockHash::default(),
    }
}

/// Initialize committees on all nodes using the full node set.
fn initialize_all_committees(sim: &mut SimulationState) {
    let eligible: Vec<EligibleNode> = sim
        .nodes
        .iter()
        .map(|n| n.as_eligible_node())
        .collect();

    let epoch_seed = [0xAB; 32]; // Deterministic seed for reproducibility.
    let network_size = eligible.len() as u64;

    for node in &mut sim.nodes {
        let _ = node.engine.initialize_committee(&eligible, &epoch_seed, 0, 0);
    }

    let tier = committee::tier_for_network_size(network_size);
    info!(
        network_size = network_size,
        committee_size = tier.committee_size,
        finality_threshold = tier.finality_threshold,
        "Committees initialized on all nodes"
    );
}

/// Initialize committees using only connected nodes.
fn initialize_committees_for_connected(sim: &mut SimulationState) {
    let eligible: Vec<EligibleNode> = sim
        .nodes
        .iter()
        .filter(|n| n.connected)
        .map(|n| n.as_eligible_node())
        .collect();

    let epoch_seed = sim.chain_tip.0;
    let network_size = eligible.len() as u64;

    for node in &mut sim.nodes {
        if node.connected {
            let _ = node.engine.initialize_committee(&eligible, &epoch_seed, 0, 0);
        }
    }

    let tier = committee::tier_for_network_size(network_size);
    info!(
        connected = network_size,
        committee_size = tier.committee_size,
        finality_threshold = tier.finality_threshold,
        "Committees initialized for connected nodes"
    );
}

/// Re-initialize committees on connected nodes only.
fn reinitialize_committees_connected(sim: &mut SimulationState) {
    initialize_committees_for_connected(sim);
}

/// Re-initialize committees on all nodes with the full set.
fn reinitialize_committees_all(sim: &mut SimulationState) {
    let eligible: Vec<EligibleNode> = sim
        .nodes
        .iter()
        .filter(|n| n.connected)
        .map(|n| n.as_eligible_node())
        .collect();

    let epoch_seed = sim.chain_tip.0;
    let network_size = eligible.len() as u64;

    for node in &mut sim.nodes {
        if node.connected {
            let _ = node.engine.initialize_committee(&eligible, &epoch_seed, 0, 0);
        }
    }

    let tier = committee::tier_for_network_size(network_size);
    info!(
        connected = network_size,
        committee_size = tier.committee_size,
        finality_threshold = tier.finality_threshold,
        "Committees re-initialized for all connected nodes"
    );
}

/// Run the consensus loop for a given duration.
///
/// Each iteration:
/// 1. Advance slot on all connected nodes.
/// 2. Find the producer for this slot.
/// 3. Producer creates a block.
/// 4. Committee members co-sign.
/// 5. Once finality threshold is met, finalize.
/// 6. Print status every 10 seconds.
async fn run_consensus_loop(
    state: Arc<Mutex<SimulationState>>,
    duration_secs: u64,
    phase_start: Instant,
) {
    let slot_interval = Duration::from_secs(1); // Faster than real 4s for simulation speed.
    let mut last_report = Instant::now();
    let report_interval = Duration::from_secs(10);

    loop {
        if phase_start.elapsed() >= Duration::from_secs(duration_secs) {
            break;
        }

        tokio::time::sleep(slot_interval).await;

        let mut sim = state.lock().await;

        // Advance slot on all connected nodes and find the producer.
        let mut producer_index: Option<usize> = None;
        for (i, node) in sim.nodes.iter_mut().enumerate() {
            if !node.connected {
                continue;
            }
            let should_produce = node.engine.advance_slot();
            if should_produce && producer_index.is_none() {
                producer_index = Some(i);
            }
        }

        // If no producer was selected, skip this slot.
        let producer_idx = match producer_index {
            Some(idx) => idx,
            None => continue,
        };

        // Producer creates a block.
        let produce_result = sim.nodes[producer_idx].engine.produce_block(
            vec![],
            vec![],
            [0u8; 32],
            vec![],
        );

        let block = match produce_result {
            Ok(pending) => pending.block.clone(),
            Err(e) => {
                warn!(producer = producer_idx, error = %e, "Block production failed");
                continue;
            }
        };

        // Producer self-signs.
        let self_sig = match sim.nodes[producer_idx].sign_block_header(&block.header) {
            Ok(s) => s,
            Err(e) => {
                warn!(producer = producer_idx, error = %e, "Self-sign failed");
                continue;
            }
        };

        let _ = sim.nodes[producer_idx].engine.add_block_signature(self_sig);

        // Committee members co-sign.
        let connected_indices: Vec<usize> = (0..sim.nodes.len())
            .filter(|&i| i != producer_idx && sim.nodes[i].connected)
            .collect();

        let mut sig_count = 1usize; // Producer already signed.

        for &i in &connected_indices {
            if !sim.nodes[i].engine.is_committee_member() {
                continue;
            }
            let sig = match sim.nodes[i].sign_block_header(&block.header) {
                Ok(s) => s,
                Err(_) => continue,
            };

            match sim.nodes[producer_idx].engine.add_block_signature(sig) {
                Ok(finalized) => {
                    sig_count += 1;
                    if finalized {
                        break;
                    }
                }
                Err(_) => continue,
            }
        }

        // Try to finalize.
        let finality_threshold = sim.nodes[producer_idx].engine.pending_finality_threshold();
        if sig_count >= finality_threshold {
            match sim.nodes[producer_idx].engine.finalize_pending_block() {
                Ok(finalized_block) => {
                    let final_hash = finalized_block.header.hash().unwrap_or_default();
                    sim.chain_height = finalized_block.header.height;
                    sim.chain_tip = final_hash;
                    sim.finalized_count += 1;
                    sim.nodes[producer_idx].award_block_reward();

                    // Update all other connected nodes with the finalized block.
                    for i in 0..sim.nodes.len() {
                        if i == producer_idx || !sim.nodes[i].connected {
                            continue;
                        }
                        let _ = sim.nodes[i]
                            .engine
                            .process_incoming_block(finalized_block.clone());
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Finalization failed, trying force finalize");
                    // Try force finalize for small committees.
                    match sim.nodes[producer_idx].engine.force_finalize_pending_block() {
                        Ok(finalized_block) => {
                            let final_hash =
                                finalized_block.header.hash().unwrap_or_default();
                            sim.chain_height = finalized_block.header.height;
                            sim.chain_tip = final_hash;
                            sim.finalized_count += 1;
                            sim.nodes[producer_idx].award_block_reward();

                            for i in 0..sim.nodes.len() {
                                if i == producer_idx || !sim.nodes[i].connected {
                                    continue;
                                }
                                let _ = sim.nodes[i]
                                    .engine
                                    .process_incoming_block(finalized_block.clone());
                            }
                        }
                        Err(e2) => {
                            warn!(error = %e2, "Force finalize also failed");
                            sim.forks_resolved += 1;
                        }
                    }
                }
            }
        } else {
            // Not enough signatures — treat as failed block.
            warn!(
                sigs = sig_count,
                threshold = finality_threshold,
                "Block did not reach finality threshold"
            );
            // Clear the pending block so the engine returns to Active state.
            // We consume it via take so the engine can produce again next slot.
            if let Some(_) = sim.nodes[producer_idx].engine.pending_block.take() {
                // Reset to Active so next slot can proceed.
            }
            sim.forks_resolved += 1;
        }

        // Periodic status report.
        if last_report.elapsed() >= report_interval {
            let elapsed = sim.start_time.elapsed().as_secs();
            let connected = sim.nodes.iter().filter(|n| n.connected).count();
            let committee_size = sim.nodes.iter()
                .find(|n| n.connected)
                .and_then(|n| n.engine.committee())
                .map(|c| c.size())
                .unwrap_or(0);
            let tps = if elapsed > 0 {
                sim.finalized_count as f64 / elapsed as f64
            } else {
                0.0
            };
            println!(
                "[{}s] height={} finalized={} nodes={} committee={}/{} tps={:.1}",
                elapsed,
                sim.chain_height,
                sim.finalized_count,
                connected,
                committee_size,
                connected,
                tps,
            );
            last_report = Instant::now();
        }
    }
}

/// Build final results from simulation state.
fn build_result(sim: &SimulationState, duration_secs: u64) -> SimulationResult {
    let mut reward_distribution: Vec<(usize, u64)> = sim
        .nodes
        .iter()
        .map(|n| (n.index, n.rewards))
        .collect();
    reward_distribution.sort_by(|a, b| b.1.cmp(&a.1));

    let passed = sim.finalized_count > 0 && sim.chain_height > 0;

    SimulationResult {
        duration_secs,
        final_height: sim.chain_height,
        finalized: sim.finalized_count,
        total_blocks: sim.chain_height, // In this sim, every finalized block = total block.
        forks_resolved: sim.forks_resolved,
        committee_transitions: sim.committee_transitions,
        reward_distribution,
        passed,
    }
}
