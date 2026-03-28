# Gratia Network Storage Model

## Where the Blockchain Lives

There is no central server. The blockchain is stored on every phone running the Gratia app. Each phone holds a copy in a RocksDB database within the app's local data directory.

When the first phone mines the genesis block, that block exists on that one phone. When the second phone joins and connects via libp2p, it syncs the genesis block. Now two phones have it. The chain is replicated across every participating phone on earth.

---

## What Each Phone Stores

Each phone does NOT store the entire blockchain history forever. It stores a pruned subset:

| Data | Description | Estimated Size |
|------|------------|---------------|
| Current state | Who owns what, active stakes, contract state — a snapshot of "right now" | ~1-2 GB at maturity |
| Recent blocks | Last N blocks for verification and reorganization handling | ~0.5-1 GB |
| PoL attestations | Your own Proof of Life history | Small (KBs) |
| Shard data | Only your geographic shard's transactions, not the whole world | Fraction of total |
| **Total target** | | **2-5 GB** |

Old blocks are **pruned** — deleted from the phone after they are no longer needed for verification. The phone doesn't need to know about a transaction from two years ago. It only needs to know the *result* of all past transactions (the current state).

---

## Storage Engine: RocksDB

- **Crate:** `rust-rocksdb`
- **Why RocksDB:** Proven, battle-tested, excellent ARM performance. Used by Ethereum (geth), Solana, CockroachDB, and many others. Optimized for SSD/flash storage, which is critical for mobile NAND flash.
- **Mobile tuning:** Memory-mapped I/O limits, constrained write buffer sizes, scheduled compaction to avoid performance spikes during mining.

---

## Archive Nodes

Full blockchain history is preserved on **archive nodes** — regular servers that anyone can run.

| Property | Phone Node | Archive Node |
|----------|-----------|-------------|
| Stores full history | No (pruned) | Yes (every block since genesis) |
| Participates in consensus | Yes | **No** |
| Can produce blocks | Yes | **No** |
| Can vote in governance | Yes | **No** |
| Purpose | Run the network | Historical queries, block explorer, auditing |

**Critical design rule:** Archive nodes can store history but CANNOT participate in consensus. They have no voting power, no block production rights, no influence over the network. They are read-only libraries, not decision-makers.

This ensures that the network remains phone-governed. Server operators cannot gain consensus power regardless of how much history they store or how powerful their hardware is.

---

## Launch Day Reality

```
Genesis block mined on founding team's phones (10-20 phones)
     ↓
Each phone stores the full chain (it's tiny — one block)
     ↓
Early adopters join, sync the chain from peers via libp2p
     ↓
Every phone has a complete copy (chain is still small enough)
     ↓
Months/years later, chain grows past practical phone storage
     ↓
Pruning activates — phones keep current state, archive nodes keep full history
```

For the first weeks or months, every phone will have the complete chain because it will be small enough to fit entirely in the 2-5 GB budget. Pruning only matters once the chain grows past what's practical for phone storage.

---

## Data Availability and Recovery

**As long as even one phone or archive node has the state, the chain can recover.**

When phones come back online after being offline, they sync from peers. Geographic distribution ensures it is essentially impossible for every phone worldwide to go offline simultaneously.

### Sync Process for New Nodes

1. New phone installs Gratia and connects to the network via libp2p
2. Discovers peers via mDNS (local Wi-Fi) or Kademlia DHT (internet)
3. Downloads current state snapshot from peers (not full history)
4. Verifies state against Merkle root in the latest finalized block
5. Begins receiving new blocks via Gossipsub
6. Fully operational — can mine, transact, and vote

A new phone does NOT need to download and replay the entire chain history. It only needs the current state snapshot plus recent blocks, verified against the consensus-agreed Merkle state root.

---

## Geographic Sharding Impact on Storage

With geographic sharding (target: 10 shards at scale):

- Each phone only stores state and blocks for its own shard
- Cross-shard transactions are handled via shard headers exchanged between shards
- Per-phone storage burden is roughly 1/N of what it would be without sharding (where N = number of shards)
- This keeps the 2-5 GB target achievable even as total network state grows

---

## Storage Growth Projections

| Network Size | Est. Total State | Per-Shard (10 shards) | Fits Phone Budget? |
|-------------|-----------------|----------------------|-------------------|
| 100K users | ~500 MB | ~50 MB | Yes (easily) |
| 1M users | ~2 GB | ~200 MB | Yes |
| 10M users | ~10 GB | ~1 GB | Yes |
| 100M users | ~50 GB | ~5 GB | Yes (at limit) |
| 1B users | ~200 GB | ~20 GB | Needs more shards or aggressive pruning |

At very large scale (100M+ users), the protocol would need to either increase the number of shards beyond 10 or implement more aggressive state pruning to stay within phone storage constraints. This is a governance-adjustable parameter.
