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

### Execute Redemption
Batch redeem all winning positions through the relayer:
```bash
cargo run -- --execute
```
