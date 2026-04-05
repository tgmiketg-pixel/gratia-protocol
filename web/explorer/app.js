// Gratia Block Explorer — fetches from local node API and renders chain data
(function () {
  'use strict';

  const API = 'http://localhost:8080/api/explorer/data';
  const REFRESH_MS = 4000;
  let connected = false;

  // DOM refs
  const $height    = document.getElementById('chain-height');
  const $peers     = document.getElementById('peer-count');
  const $dot       = document.getElementById('status-dot');
  const $statusTxt = document.getElementById('status-text');
  const $statBlocks  = document.getElementById('stat-blocks');
  const $statMiners  = document.getElementById('stat-miners');
  const $statRewards = document.getElementById('stat-rewards');
  const $statBurned  = document.getElementById('stat-burned');
  const $statTps     = document.getElementById('stat-tps');
  const $statBtime   = document.getElementById('stat-btime');
  const $tbody       = document.getElementById('blocks-body');
  const $content     = document.getElementById('content');
  const $connecting  = document.getElementById('connecting');

  function shorten(hex, pre, suf) {
    pre = pre || 8; suf = suf || 6;
    if (!hex || hex.length <= pre + suf + 2) return hex || '';
    return hex.slice(0, pre) + '...' + hex.slice(-suf);
  }

  function formatLux(lux) {
    // 1 GRAT = 1,000,000 Lux
    if (lux >= 1000000) return (lux / 1000000).toFixed(2) + ' GRAT';
    return lux.toLocaleString() + ' Lux';
  }

  function timeAgo(isoStr) {
    var diff = Math.floor((Date.now() - new Date(isoStr).getTime()) / 1000);
    if (diff < 5) return 'just now';
    if (diff < 60) return diff + 's ago';
    if (diff < 3600) return Math.floor(diff / 60) + 'm ago';
    return Math.floor(diff / 3600) + 'h ago';
  }

  function setConnected(yes) {
    if (connected === yes) return;
    connected = yes;
    $dot.className = yes ? 'connected' : '';
    $statusTxt.textContent = yes ? 'Connected' : 'Connecting...';
    $content.style.display = yes ? '' : 'none';
    $connecting.style.display = yes ? 'none' : '';
  }

  function render(data) {
    var net = data.network;

    // Header
    $height.textContent = 'Block #' + net.blockHeight.toLocaleString();
    $peers.textContent = net.activeNodes + ' node' + (net.activeNodes !== 1 ? 's' : '');

    // Stats
    $statBlocks.textContent = net.blockHeight.toLocaleString();
    $statMiners.textContent = net.activeNodes;
    $statRewards.textContent = formatLux(data.wallet.balance);
    $statBurned.textContent = formatLux(net.burnedFees);
    $statTps.textContent = net.tps.toFixed(2);
    $statBtime.textContent = net.avgBlockTime.toFixed(1) + 's';

    // Blocks table
    var rows = '';
    var blocks = data.blocks || [];
    for (var i = 0; i < blocks.length && i < 20; i++) {
      var b = blocks[i];
      rows += '<tr>'
        + '<td>' + b.height + '</td>'
        + '<td class="mono">' + shorten(b.producer, 9, 6) + '</td>'
        + '<td>' + timeAgo(b.timestamp) + '</td>'
        + '<td>' + b.transactionCount + '</td>'
        + '<td class="mono hide-mobile">' + shorten(b.hash, 8, 6) + '</td>'
        + '<td class="hide-mobile">' + b.size + ' B</td>'
        + '</tr>';
    }
    $tbody.innerHTML = rows || '<tr><td colspan="6" style="text-align:center;color:var(--text-dim)">No blocks yet</td></tr>';
  }

  function poll() {
    fetch(API)
      .then(function (r) {
        if (!r.ok) throw new Error(r.status);
        return r.json();
      })
      .then(function (data) {
        if (data.error) throw new Error(data.error);
        setConnected(true);
        render(data);
      })
      .catch(function () {
        setConnected(false);
      });
  }

  // Start
  setConnected(false);
  poll();
  setInterval(poll, REFRESH_MS);
})();
