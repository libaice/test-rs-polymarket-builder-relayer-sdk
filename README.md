# Polymarket Relayer Test Utility

A Rust script to query all settled Polymarket positions for a specific wallet and batch redeem them using the `rs-builder-relayer-client` SDK.

## Setup

1. Copy `.env.example` to `.env`:
   ```bash
   cp .env.example .env
   ```

2. Configure your environment variables in `.env`:
   - `POLYMARKET_PRIVATE_KEY`: Your signer's private key.
   - `POLY_RELAYER_ADDRESS`: Your Proxy or Safe wallet address.
   - `SIGNATURE_TYPE`: `1` for Proxy, `2` for Safe (Default).
   - `BUILDER_KEY`/`SECRET`/`PASSPHRASE`: Your Relayer credentials.
   - `POLYGON_RPC_URL`: A reliable Polygon RPC (required for nonce tracking).

## Usage

### Dry-run (Scan Only)
List all your current positions and identify WON markets without sending any transactions:
```bash
cargo run
```

### Execute Redemption (Sequential Mode - Recommended)
By default, executing will sequentially redeem your winning positions one by one with a 5-second delay. This prevents relayer queue bottlenecks (especially for Proxy wallets) and features an **automatic "Direct" fallback** if the gasless endpoint fails.
```bash
cargo run -- --execute
```

You can customize the delay:
```bash
cargo run -- --execute --delay 8
```

### Execute Redemption (Batch Mode)
Merge all redeems into a single gasless API request.
> ⚠️ **Warning**: Do not use this for Proxy wallets if you have more than 2 conditions due to the relayer's internal tight gas limit overhead.
```bash
cargo run -- --execute --batch
```
