# Gratia Block Explorer

A single-page block explorer for the Gratia blockchain. Built with vanilla HTML, CSS, and JavaScript -- no frameworks, no build step.

## Quick Start

### Option 1: Open directly

Double-click `index.html` or open it in any browser. The explorer will auto-probe `localhost:8080`, `localhost:8081`, and `localhost:9090` for a running Gratia node. If no node is found, it displays demo data.

### Option 2: Serve with a static file server

```bash
# Python
python -m http.server 3000

# Node.js (npx)
npx serve .

# Any other static server
```

Then open `http://localhost:3000` in your browser.

### Option 3: Connect to a specific node

Append `?api=` to the URL with the node's address:

```
index.html?api=http://192.168.1.42:8080
```

This is useful when the Gratia node is running on a phone and you want to view the explorer on a desktop browser connected to the same Wi-Fi.

## Features

- **Network stats**: block height, total transactions, active nodes, average block time, TPS, mining state
- **Wallet info**: address and balance from the connected node
- **Recent blocks**: height, timestamp, producer (truncated), transaction count
- **Recent transactions**: hash, from, to, amount in GRAT, confirmation status
- **Search**: filter by block height or transaction hash
- **Auto-refresh**: fetches new data from the API every 5 seconds
- **Detail modals**: click any block or transaction row to see full details
- **Responsive**: works on mobile and desktop screens
- **Demo mode**: shows synthetic data when no live node is available

## API Endpoint

The explorer fetches data from:

```
GET {api_url}/api/explorer/data
```

This endpoint is served by the Gratia node's built-in HTTP server (started via `start_explorer_api()` in the FFI layer). The default port is 8080.

## Branding

- Background: DeepNavy `#1A2744`
- Accents: AmberGold `#F5A623`
- Text: WarmWhite `#FAF5EB`
