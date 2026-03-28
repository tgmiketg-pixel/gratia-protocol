/* ==========================================================================
   Gratia Block Explorer — Application Logic
   Vanilla JS, no dependencies. Works by opening index.html directly.
   ========================================================================== */

// ---------------------------------------------------------------------------
// Demo Data
// ---------------------------------------------------------------------------

// Two wallet addresses representing the two test phones on the network
const WALLET_A = "grat:0960e2fd0023dbb060db362bf87a646d2babadad3748f47836161990a5ef7c12";
const WALLET_B = "grat:e83063877f6cee7115989c2d6fba392782425ba5f1ef4ceb452a6a38d91b04f7";

// Shard names for geographic sharding (10 shards)
const SHARD_NAMES = [
  "North America",
  "South America",
  "Western Europe",
  "Eastern Europe",
  "Sub-Saharan Africa",
  "MENA",
  "South Asia",
  "East Asia",
  "Southeast Asia",
  "Oceania"
];

// Transaction types for Phase 3
const TX_TYPES = ["standard", "shielded", "cross-shard", "mesh-relayed"];

/**
 * Generate realistic demo data for the explorer.
 * Produces blocks counting down from the tip and a handful of transactions
 * scattered among them.
 */
function generateDemoData() {
  const now = new Date("2026-03-26T22:40:34Z");
  // 4-second block time
  const BLOCK_TIME_MS = 4000;
  const TIP_HEIGHT = 1947;
  const TOTAL_BLOCKS = 30;

  // --- Transactions (placed at specific block heights) ---
  // Each tx now includes: txType (standard|shielded|cross-shard|mesh-relayed), shard, crossShardTarget
  const transactions = [
    {
      hash: "3c1e6ee59ae34c23d895d6732c9f192df865d2011679d787347edbdd73e4fd93",
      blockHeight: 1945,
      from: WALLET_A,
      to: WALLET_B,
      amount: 100000000000,   // 100,000 GRAT
      fee: 1000,
      nonce: 12,
      status: "confirmed",
      txType: "standard",
      shard: 0,
      timestamp: null         // filled from block
    },
    {
      hash: "8fa24b9e10cc4f71a83204ddf98abd263e7017ab54c7e1dcb4490f3a6fd22e0a",
      blockHeight: 1938,
      from: WALLET_B,
      to: WALLET_A,
      amount: 25000000000,    // 25,000 GRAT
      fee: 1000,
      nonce: 4,
      status: "confirmed",
      txType: "shielded",
      shard: 0,
      timestamp: null
    },
    {
      hash: "d4c0aef7231948e6bc35f1029edab46710c8935f4a22d80ce6598172b0375117",
      blockHeight: 1930,
      from: WALLET_A,
      to: WALLET_B,
      amount: 50000000000,    // 50,000 GRAT
      fee: 1000,
      nonce: 11,
      status: "confirmed",
      txType: "cross-shard",
      shard: 0,
      crossShardTarget: 2,    // Western Europe
      timestamp: null
    },
    {
      hash: "a99e1b3c587040d2b8ef6471fdc7e28d9a03154ea62c4809857f2319bca610dd",
      blockHeight: 1925,
      from: WALLET_B,
      to: WALLET_A,
      amount: 10000000000,    // 10,000 GRAT
      fee: 1000,
      nonce: 3,
      status: "confirmed",
      txType: "mesh-relayed",
      shard: 0,
      timestamp: null
    },
    {
      hash: "f612de8a9c2347b0a1d54389c0bee7263d4f910872adce4f63b128099e57a4c8",
      blockHeight: 1920,
      from: WALLET_A,
      to: WALLET_B,
      amount: 250000000000,   // 250,000 GRAT
      fee: 1500,
      nonce: 10,
      status: "confirmed",
      txType: "standard",
      shard: 0,
      timestamp: null
    }
  ];

  // Build a set of block heights that contain transactions
  const txByBlock = {};
  for (const tx of transactions) {
    if (!txByBlock[tx.blockHeight]) txByBlock[tx.blockHeight] = [];
    txByBlock[tx.blockHeight].push(tx);
  }

  // --- Blocks ---
  const blocks = [];
  // Simple deterministic pseudo-random for demo hash generation
  const hexChars = "0123456789abcdef";
  function pseudoHash(seed) {
    let h = "";
    let s = seed;
    for (let i = 0; i < 64; i++) {
      s = (s * 1103515245 + 12345) & 0x7fffffff;
      h += hexChars[s % 16];
    }
    return h;
  }

  for (let i = 0; i < TOTAL_BLOCKS; i++) {
    const height = TIP_HEIGHT - i;
    const timestamp = new Date(now.getTime() - i * BLOCK_TIME_MS);
    // Alternate producer between the two phones
    const producer = (height % 3 === 0) ? WALLET_B : WALLET_A;
    const txCount = txByBlock[height] ? txByBlock[height].length : 0;
    // Base size: ~340 bytes for empty block, +250 per tx
    const size = 340 + txCount * 250;

    // Assign shard based on height — most blocks in shard 0 (our local shard), some in others
    const blockShard = (height % 7 === 0) ? ((height % 10)) : 0;

    blocks.push({
      height,
      hash: pseudoHash(height * 7 + 3),
      parentHash: pseudoHash((height - 1) * 7 + 3),
      timestamp: timestamp.toISOString(),
      producer,
      transactionCount: txCount,
      attestationCount: 0,
      signatures: 2,   // both phones sign every block on testnet
      size,
      shard: blockShard
    });

    // Assign timestamps to transactions in this block
    if (txByBlock[height]) {
      for (const tx of txByBlock[height]) {
        // tx timestamp is ~1 second before block timestamp
        tx.timestamp = new Date(timestamp.getTime() - 1000).toISOString();
      }
    }
  }

  // Build shard stats — count blocks per shard
  const shardStats = SHARD_NAMES.map((name, idx) => {
    const shardBlocks = blocks.filter(b => b.shard === idx);
    return {
      id: idx,
      name,
      blockCount: shardBlocks.length,
      active: idx < 3 || idx === 7, // Demo: shards 0-2 and 7 are active
      isLocal: idx === 0,
      tps: idx === 0 ? (transactions.length / (TOTAL_BLOCKS * 4)).toFixed(3) : (Math.random() * 0.05).toFixed(3)
    };
  });

  return {
    network: {
      name: "Gratia Testnet",
      blockHeight: TIP_HEIGHT,
      totalTransactions: transactions.length,
      activeNodes: 2,
      avgBlockTime: 4.0,
      tps: (transactions.length / (TOTAL_BLOCKS * 4)).toFixed(3),
      tpsPerShard: (transactions.length / (TOTAL_BLOCKS * 4)).toFixed(3),
      activeShards: 4,
      totalShards: 10
    },
    mesh: {
      blePeers: 0,
      wifiDirectPeers: 0,
      bridgePeers: 1,       // Bootstrap node acts as bridge
      meshRelayedTxs: 1     // One mesh-relayed tx in demo data
    },
    shards: shardStats,
    blocks,
    transactions: transactions.sort((a, b) => b.blockHeight - a.blockHeight)
  };
}

const DEMO_DATA = generateDemoData();

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

let appData = DEMO_DATA;
let liveApiUrl = null;
let refreshInterval = null;

// Pagination state
const PAGE_SIZE = 10;
let blocksPage = 0;
let txsPage = 0;

// ---------------------------------------------------------------------------
// Utility Functions
// ---------------------------------------------------------------------------

/**
 * Convert a Lux amount to a human-readable GRAT string.
 * 1 GRAT = 1,000,000 Lux.
 * Example: 100000000000 => "100,000.000000 GRAT"
 */
function formatGrat(lux) {
  const grat = lux / 1_000_000;
  const parts = grat.toFixed(6).split(".");
  parts[0] = parts[0].replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return parts.join(".") + " GRAT";
}

/**
 * Truncate a hex hash for display.
 * "a1b2c3d4e5f6..." => "a1b2c3d4..."
 */
function truncateHash(hash, len = 8) {
  if (!hash) return "—";
  if (hash.length <= len + 3) return hash;
  return hash.slice(0, len) + "...";
}

/**
 * Truncate a grat: address for display.
 * "grat:0960e2fd..." => "grat:0960e2fd0023..."
 */
function truncateAddress(addr, len = 12) {
  if (!addr) return "—";
  if (!addr.startsWith("grat:")) return truncateHash(addr, len);
  const hex = addr.slice(5);
  if (hex.length <= len + 3) return addr;
  return "grat:" + hex.slice(0, len) + "...";
}

/**
 * Human-readable relative time.
 * "2 min ago", "1 hour ago", etc.
 */
function timeAgo(timestamp) {
  const now = new Date();
  const then = new Date(timestamp);
  const seconds = Math.floor((now - then) / 1000);

  if (seconds < 5) return "just now";
  if (seconds < 60) return seconds + "s ago";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return minutes + " min ago";
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return hours + " hour" + (hours > 1 ? "s" : "") + " ago";
  const days = Math.floor(hours / 24);
  return days + " day" + (days > 1 ? "s" : "") + " ago";
}

/**
 * Format a timestamp to a readable date string.
 * "Mar 26, 2026 22:39:26"
 */
function formatTimestamp(ts) {
  const d = new Date(ts);
  const months = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
  const mon = months[d.getUTCMonth()];
  const day = d.getUTCDate();
  const year = d.getUTCFullYear();
  const hh = String(d.getUTCHours()).padStart(2, "0");
  const mm = String(d.getUTCMinutes()).padStart(2, "0");
  const ss = String(d.getUTCSeconds()).padStart(2, "0");
  return `${mon} ${day}, ${year} ${hh}:${mm}:${ss} UTC`;
}

/**
 * Format byte size to human-readable.
 */
function formatSize(bytes) {
  if (bytes < 1024) return bytes + " B";
  return (bytes / 1024).toFixed(1) + " KB";
}

/**
 * Safely set text content of an element by ID.
 */
function setText(id, text) {
  const el = document.getElementById(id);
  if (el) el.textContent = text;
}

// ---------------------------------------------------------------------------
// Rendering — Dashboard
// ---------------------------------------------------------------------------

/**
 * Return the CSS class suffix for a transaction type badge.
 */
function txTypeBadge(txType) {
  const label = txType || "standard";
  return `<span class="badge badge-${label}">${label}</span>`;
}

function renderDashboard() {
  const net = appData.network;
  setText("stat-height", net.blockHeight.toLocaleString());
  setText("stat-txs", net.totalTransactions.toLocaleString());
  setText("stat-nodes", net.activeNodes.toString());
  setText("stat-blocktime", net.avgBlockTime.toFixed(1) + "s");
  setText("stat-tps", net.tps);
  setText("stat-source", liveApiUrl ? "Live" : "Demo");
  setText("network-name", net.name);

  // New Phase 3 stats
  setText("stat-mesh-peers", (appData.mesh ? (appData.mesh.blePeers + appData.mesh.wifiDirectPeers) : 0).toString());
  setText("stat-shards", net.activeShards ? (net.activeShards + " / " + net.totalShards) : "--");
  const tpsPerShardEl = document.getElementById("stat-tps-per-shard");
  if (tpsPerShardEl && net.tpsPerShard) {
    tpsPerShardEl.textContent = net.tpsPerShard + " per shard";
  }

  // Render shard visualization
  renderShardGrid();

  // Render mesh network stats
  renderMeshStats();

  // Latest 5 blocks
  const tbody = document.getElementById("latest-blocks");
  tbody.innerHTML = "";
  const latestBlocks = appData.blocks.slice(0, 5);
  for (const b of latestBlocks) {
    const tr = document.createElement("tr");
    tr.className = "clickable";
    tr.onclick = () => showBlockDetail(b);
    tr.innerHTML = `
      <td class="mono hash-link">${b.height}</td>
      <td class="text-dim">${timeAgo(b.timestamp)}</td>
      <td class="mono">${truncateAddress(b.producer, 10)}</td>
      <td>${b.transactionCount}</td>
      <td class="text-dim">${formatSize(b.size)}</td>
    `;
    tbody.appendChild(tr);
  }

  // Latest 5 transactions — now with Type column
  const txBody = document.getElementById("latest-txs");
  txBody.innerHTML = "";
  const latestTxs = appData.transactions.slice(0, 5);
  if (latestTxs.length === 0) {
    txBody.innerHTML = '<tr><td colspan="5" class="text-muted" style="text-align:center;padding:24px;">No transactions yet</td></tr>';
    return;
  }
  for (const tx of latestTxs) {
    const tr = document.createElement("tr");
    tr.className = "clickable";
    tr.onclick = () => showTxDetail(tx);
    tr.innerHTML = `
      <td class="mono hash-link">${truncateHash(tx.hash)}</td>
      <td>${txTypeBadge(tx.txType)}</td>
      <td>
        <span class="mono">${truncateAddress(tx.from, 8)}</span>
        <span class="arrow">&rarr;</span>
        <span class="mono">${truncateAddress(tx.to, 8)}</span>
      </td>
      <td class="amount-positive">${formatGrat(tx.amount)}</td>
      <td><span class="badge badge-${tx.status}">${tx.status}</span></td>
    `;
    txBody.appendChild(tr);
  }
}

// ---------------------------------------------------------------------------
// Rendering — Shard Grid
// ---------------------------------------------------------------------------

function renderShardGrid() {
  const grid = document.getElementById("shard-grid");
  const select = document.getElementById("shard-select");
  if (!grid || !appData.shards) return;

  grid.innerHTML = "";
  // Populate shard selector if needed
  if (select && select.options.length <= 1) {
    for (const s of appData.shards) {
      const opt = document.createElement("option");
      opt.value = s.id;
      opt.textContent = s.name;
      select.appendChild(opt);
    }
  }

  for (const s of appData.shards) {
    const tile = document.createElement("div");
    let cls = "shard-tile";
    if (s.isLocal) cls += " shard-tile-local";
    if (!s.active) cls += " shard-tile-inactive";
    tile.className = cls;
    tile.innerHTML = `
      <div class="shard-tile-name">${s.name}</div>
      <div class="shard-tile-blocks">${s.blockCount}</div>
      <div class="shard-tile-tps">${s.active ? s.tps + " TPS" : "inactive"}</div>
    `;
    tile.onclick = () => {
      if (select) select.value = s.id;
      // Could filter blocks by shard here
    };
    grid.appendChild(tile);
  }
}

// ---------------------------------------------------------------------------
// Rendering — Mesh Network Stats
// ---------------------------------------------------------------------------

function renderMeshStats() {
  if (!appData.mesh) return;
  setText("mesh-ble-peers", appData.mesh.blePeers.toString());
  setText("mesh-wifi-peers", appData.mesh.wifiDirectPeers.toString());
  setText("mesh-bridge-peers", appData.mesh.bridgePeers.toString());
  setText("mesh-relayed-txs", appData.mesh.meshRelayedTxs.toString());
}

// ---------------------------------------------------------------------------
// Rendering — Blocks Tab
// ---------------------------------------------------------------------------

function renderBlocksTab() {
  // Apply shard filter
  const shardFilter = document.getElementById("blocks-shard-filter");
  const filterValue = shardFilter ? shardFilter.value : "all";
  const blocks = filterValue === "all"
    ? appData.blocks
    : appData.blocks.filter(b => b.shard === parseInt(filterValue, 10));

  const totalPages = Math.max(1, Math.ceil(blocks.length / PAGE_SIZE));
  if (blocksPage >= totalPages) blocksPage = 0;
  const start = blocksPage * PAGE_SIZE;
  const page = blocks.slice(start, start + PAGE_SIZE);

  // Populate shard filter options if not yet done
  if (shardFilter && shardFilter.options.length <= 1 && appData.shards) {
    for (const s of appData.shards) {
      const opt = document.createElement("option");
      opt.value = s.id;
      opt.textContent = "Shard " + s.id + " — " + s.name;
      shardFilter.appendChild(opt);
    }
  }

  const tbody = document.getElementById("blocks-table");
  tbody.innerHTML = "";
  for (const b of page) {
    const shardName = SHARD_NAMES[b.shard] || "Shard " + b.shard;
    const tr = document.createElement("tr");
    tr.className = "clickable";
    tr.onclick = () => showBlockDetail(b);
    tr.innerHTML = `
      <td class="mono hash-link">${b.height}</td>
      <td class="text-dim" title="${shardName}">S${b.shard}</td>
      <td class="mono">${truncateHash(b.hash, 12)}</td>
      <td class="text-dim">${timeAgo(b.timestamp)}</td>
      <td class="mono">${truncateAddress(b.producer, 10)}</td>
      <td>${b.transactionCount}</td>
      <td>${b.signatures}</td>
      <td class="text-dim">${formatSize(b.size)}</td>
    `;
    tbody.appendChild(tr);
  }

  renderPagination("blocks-pagination", blocksPage, totalPages, (p) => {
    blocksPage = p;
    renderBlocksTab();
  });
}

// ---------------------------------------------------------------------------
// Rendering — Transactions Tab
// ---------------------------------------------------------------------------

function renderTxsTab() {
  const txs = appData.transactions;
  const totalPages = Math.max(1, Math.ceil(txs.length / PAGE_SIZE));
  const start = txsPage * PAGE_SIZE;
  const page = txs.slice(start, start + PAGE_SIZE);

  const tbody = document.getElementById("txs-table");
  tbody.innerHTML = "";

  if (txs.length === 0) {
    tbody.innerHTML = '<tr><td colspan="9" class="text-muted" style="text-align:center;padding:24px;">No transactions yet</td></tr>';
    renderPagination("txs-pagination", 0, 1, () => {});
    return;
  }

  for (const tx of page) {
    const crossShardIndicator = tx.txType === "cross-shard" && tx.crossShardTarget !== undefined
      ? ` <span class="text-muted" title="Cross-shard: S${tx.shard} → S${tx.crossShardTarget}">(S${tx.shard}→S${tx.crossShardTarget})</span>`
      : "";
    const tr = document.createElement("tr");
    tr.className = "clickable";
    tr.onclick = () => showTxDetail(tx);
    tr.innerHTML = `
      <td class="mono hash-link">${truncateHash(tx.hash)}</td>
      <td>${txTypeBadge(tx.txType)}${crossShardIndicator}</td>
      <td class="mono hash-link">${tx.blockHeight}</td>
      <td class="mono">${truncateAddress(tx.from, 8)}</td>
      <td class="mono">${truncateAddress(tx.to, 8)}</td>
      <td class="amount-positive">${formatGrat(tx.amount)}</td>
      <td class="mono text-dim">${tx.fee.toLocaleString()} Lux</td>
      <td><span class="badge badge-${tx.status}">${tx.status}</span></td>
      <td class="text-dim">${timeAgo(tx.timestamp)}</td>
    `;
    tbody.appendChild(tr);
  }

  renderPagination("txs-pagination", txsPage, totalPages, (p) => {
    txsPage = p;
    renderTxsTab();
  });
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

function renderPagination(containerId, currentPage, totalPages, onPageChange) {
  const container = document.getElementById(containerId);
  container.innerHTML = "";

  if (totalPages <= 1) return;

  // Previous button
  const prev = document.createElement("button");
  prev.textContent = "\u2190";
  prev.disabled = currentPage === 0;
  prev.onclick = () => onPageChange(currentPage - 1);
  container.appendChild(prev);

  // Page info
  const info = document.createElement("span");
  info.className = "page-info";
  info.textContent = `${currentPage + 1} / ${totalPages}`;
  container.appendChild(info);

  // Next button
  const next = document.createElement("button");
  next.textContent = "\u2192";
  next.disabled = currentPage >= totalPages - 1;
  next.onclick = () => onPageChange(currentPage + 1);
  container.appendChild(next);
}

// ---------------------------------------------------------------------------
// Detail Modals
// ---------------------------------------------------------------------------

function showBlockDetail(block) {
  const shardName = SHARD_NAMES[block.shard] || "Shard " + block.shard;
  document.getElementById("modal-title").textContent = `Block #${block.height}`;
  const body = document.getElementById("modal-body");
  body.innerHTML = `
    ${detailRow("Height", block.height.toLocaleString())}
    ${detailRow("Shard", "S" + block.shard + " — " + shardName)}
    ${detailRow("Block Hash", block.hash)}
    ${detailRow("Parent Hash", block.parentHash)}
    ${detailRow("Timestamp", formatTimestamp(block.timestamp))}
    ${detailRow("Producer", block.producer)}
    ${detailRow("Transactions", block.transactionCount)}
    ${detailRow("Attestations", block.attestationCount)}
    ${detailRow("Signatures", block.signatures + " / 2")}
    ${detailRow("Size", formatSize(block.size))}
  `;
  document.getElementById("detail-modal").classList.remove("hidden");
}

function showTxDetail(tx) {
  document.getElementById("modal-title").textContent = "Transaction Details";
  const body = document.getElementById("modal-body");
  const crossShardRow = tx.txType === "cross-shard" && tx.crossShardTarget !== undefined
    ? detailRow("Cross-Shard", "S" + tx.shard + " (" + (SHARD_NAMES[tx.shard] || "") + ") → S" + tx.crossShardTarget + " (" + (SHARD_NAMES[tx.crossShardTarget] || "") + ")")
    : "";
  body.innerHTML = `
    ${detailRow("Hash", tx.hash)}
    ${detailRow("Type", txTypeBadge(tx.txType))}
    ${detailRow("Block", tx.blockHeight.toLocaleString())}
    ${detailRow("Shard", "S" + (tx.shard !== undefined ? tx.shard : "0") + " — " + (SHARD_NAMES[tx.shard] || "Unknown"))}
    ${crossShardRow}
    ${detailRow("Timestamp", formatTimestamp(tx.timestamp))}
    ${detailRow("From", tx.txType === "shielded" ? '<span class="text-dim">[shielded]</span>' : tx.from)}
    ${detailRow("To", tx.txType === "shielded" ? '<span class="text-dim">[shielded]</span>' : tx.to)}
    ${detailRow("Amount", tx.txType === "shielded" ? '<span class="text-dim">[shielded]</span>' : formatGrat(tx.amount))}
    ${detailRow("Fee", tx.fee.toLocaleString() + " Lux")}
    ${detailRow("Nonce", tx.nonce)}
    ${detailRow("Status", '<span class="badge badge-' + tx.status + '">' + tx.status + '</span>')}
  `;
  document.getElementById("detail-modal").classList.remove("hidden");
}

function detailRow(label, value) {
  return `
    <div class="detail-row">
      <div class="detail-label">${label}</div>
      <div class="detail-value">${value}</div>
    </div>
  `;
}

function closeModal() {
  document.getElementById("detail-modal").classList.add("hidden");
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

function performSearch(query) {
  query = query.trim().toLowerCase();
  if (!query) return;

  const results = [];

  // Search by block height
  const asNum = parseInt(query, 10);
  if (!isNaN(asNum)) {
    const block = appData.blocks.find(b => b.height === asNum);
    if (block) {
      results.push({ type: "block", label: `Block #${block.height}`, item: block });
    }
  }

  // Search by block hash
  for (const b of appData.blocks) {
    if (b.hash.toLowerCase().includes(query)) {
      results.push({ type: "block", label: `Block #${b.height} (${truncateHash(b.hash)})`, item: b });
    }
  }

  // Search by tx hash
  for (const tx of appData.transactions) {
    if (tx.hash.toLowerCase().includes(query)) {
      results.push({ type: "transaction", label: `Tx ${truncateHash(tx.hash, 16)}`, item: tx });
    }
  }

  // Search by address (matches from or to)
  for (const tx of appData.transactions) {
    if (tx.from.toLowerCase().includes(query) || tx.to.toLowerCase().includes(query)) {
      const exists = results.find(r => r.type === "transaction" && r.item.hash === tx.hash);
      if (!exists) {
        results.push({ type: "transaction", label: `Tx ${truncateHash(tx.hash, 16)} (address match)`, item: tx });
      }
    }
  }

  // Also search blocks by producer address
  for (const b of appData.blocks) {
    if (b.producer.toLowerCase().includes(query)) {
      const exists = results.find(r => r.type === "block" && r.item.height === b.height);
      if (!exists) {
        results.push({ type: "block", label: `Block #${b.height} (producer match)`, item: b });
      }
    }
  }

  showSearchResults(results, query);
}

function showSearchResults(results, query) {
  const overlay = document.getElementById("search-results");
  const body = document.getElementById("search-results-body");

  if (results.length === 0) {
    body.innerHTML = `<div class="search-empty">No results found for "${query}"</div>`;
  } else {
    body.innerHTML = "";
    for (const r of results.slice(0, 20)) {
      const div = document.createElement("div");
      div.className = "search-result-item";
      div.innerHTML = `
        <div class="search-result-type">${r.type}</div>
        <div class="search-result-value">${r.label}</div>
      `;
      div.onclick = () => {
        overlay.classList.add("hidden");
        if (r.type === "block") showBlockDetail(r.item);
        else showTxDetail(r.item);
      };
      body.appendChild(div);
    }
  }

  overlay.classList.remove("hidden");
}

function closeSearchResults() {
  document.getElementById("search-results").classList.add("hidden");
}

// ---------------------------------------------------------------------------
// Tab Navigation
// ---------------------------------------------------------------------------

function switchTab(tabName) {
  // Update nav buttons
  document.querySelectorAll(".nav-tab").forEach(btn => {
    btn.classList.toggle("active", btn.dataset.tab === tabName);
  });

  // Show/hide content
  document.querySelectorAll(".tab-content").forEach(el => {
    el.classList.toggle("active", el.id === "tab-" + tabName);
  });

  // Re-render the active tab
  if (tabName === "dashboard") renderDashboard();
  else if (tabName === "blocks") renderBlocksTab();
  else if (tabName === "transactions") renderTxsTab();
}

// ---------------------------------------------------------------------------
// Live Mode — Fetch from API
// ---------------------------------------------------------------------------

/**
 * Attempt to fetch data from a live node API.
 * Expected endpoint: GET {apiUrl}/explorer/data
 * Returns the same shape as DEMO_DATA.
 */
async function fetchLiveData() {
  if (!liveApiUrl) return;

  try {
    const resp = await fetch(liveApiUrl + "/explorer/data", {
      signal: AbortSignal.timeout(5000) // 5 second timeout
    });
    if (!resp.ok) throw new Error("HTTP " + resp.status);
    const data = await resp.json();
    appData = data;
    updateConnectionStatus(true);
    renderCurrentTab();
  } catch (err) {
    console.warn("Live fetch failed:", err.message);
    updateConnectionStatus(false);
  }
}

/**
 * Auto-probe common local ports for a running Gratia node API.
 * Tries ports 8080, 8081, 9090 on localhost and the current hostname.
 * Falls back to demo data if no live node is found.
 */
async function autoProbeApi() {
  const ports = [8080, 8081, 9090];
  const hosts = ["localhost", "127.0.0.1"];

  // Also try the phone's IP if we're on the same WiFi
  // (user would set ?api= manually for that case)

  for (const host of hosts) {
    for (const port of ports) {
      const url = `http://${host}:${port}`;
      try {
        const resp = await fetch(url + "/api", {
          signal: AbortSignal.timeout(2000)
        });
        if (resp.ok) {
          const data = await resp.json();
          if (data.service && data.service.includes("Gratia")) {
            console.log("Auto-detected Gratia node at", url);
            return url;
          }
        }
      } catch {
        // Silently skip — port not responding
      }
    }
  }
  return null;
}

/**
 * Update the connection status indicator in the nav bar.
 */
function updateConnectionStatus(connected) {
  const dot = document.getElementById("status-dot");
  const label = document.getElementById("connection-label");
  if (!dot || !label) return;

  if (connected && liveApiUrl) {
    dot.style.background = "#22c55e";
    label.textContent = "Live";
    label.style.color = "#22c55e";
  } else if (liveApiUrl) {
    dot.style.background = "#f59e0b";
    label.textContent = "Reconnecting...";
    label.style.color = "#f59e0b";
  } else {
    dot.style.background = "#6b7280";
    label.textContent = "Demo";
    label.style.color = "#6b7280";
  }
}

function renderCurrentTab() {
  const active = document.querySelector(".nav-tab.active");
  if (active) switchTab(active.dataset.tab);
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

async function init() {
  // Check for ?api= URL parameter for live mode
  const params = new URLSearchParams(window.location.search);
  const apiParam = params.get("api");

  if (apiParam) {
    liveApiUrl = apiParam.replace(/\/+$/, ""); // strip trailing slashes
    console.log("Live mode: connecting to", liveApiUrl);
  } else {
    // Auto-probe for a local Gratia node
    console.log("No ?api= param — auto-probing for local node...");
    const detected = await autoProbeApi();
    if (detected) {
      liveApiUrl = detected;
      console.log("Auto-connected to", liveApiUrl);
    }
  }

  if (liveApiUrl) {
    // Fetch immediately and set up auto-refresh every 4 seconds (one block time)
    fetchLiveData();
    refreshInterval = setInterval(fetchLiveData, 4000);
    updateConnectionStatus(false); // Will flip to true on first successful fetch
  } else {
    updateConnectionStatus(false);
  }

  // Tab navigation
  document.querySelectorAll(".nav-tab").forEach(btn => {
    btn.addEventListener("click", () => switchTab(btn.dataset.tab));
  });

  // "View All" buttons on dashboard
  document.querySelectorAll(".view-all-btn").forEach(btn => {
    btn.addEventListener("click", () => switchTab(btn.dataset.goto));
  });

  // Search
  document.getElementById("search-btn").addEventListener("click", () => {
    performSearch(document.getElementById("search-input").value);
  });
  document.getElementById("search-input").addEventListener("keydown", (e) => {
    if (e.key === "Enter") performSearch(e.target.value);
  });

  // Shard filter on blocks tab
  const blocksShardFilter = document.getElementById("blocks-shard-filter");
  if (blocksShardFilter) {
    blocksShardFilter.addEventListener("change", () => {
      blocksPage = 0;
      renderBlocksTab();
    });
  }

  // Close modals
  document.getElementById("modal-close").addEventListener("click", closeModal);
  document.querySelector(".modal-backdrop").addEventListener("click", closeModal);
  document.getElementById("search-close").addEventListener("click", closeSearchResults);

  // Escape key closes any overlay
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      closeModal();
      closeSearchResults();
    }
  });

  // Initial render
  renderDashboard();
}

// Start when DOM is ready
if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
