//! Test script: query all settled Polymarket positions and batch redeem them.
//!
//! Usage:
//!   cargo run                          # dry-run (list positions only)
//!   cargo run -- --execute             # sequential redeem (default, 5s delay)
//!   cargo run -- --execute --batch     # batch all into one relay request
//!   cargo run -- --execute --delay 8   # sequential with custom delay

use ethers::signers::LocalWallet;
use ethers::types::Address;
use polymarket_client_sdk::data::Client as DataClient;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_relayer::{
    operations, AuthMethod, DirectExecutor, RelayClient, RelayerTxType, Transaction,
};
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::env;
use tokio::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let _ = dotenvy::dotenv();

    // ── Parse CLI args ─────────────────────────────────────────────────
    let args: Vec<String> = env::args().collect();
    let execute = args.iter().any(|a| a == "--execute");
    let batch_mode = args.iter().any(|a| a == "--batch");
    let delay_secs: u64 = args.iter()
        .position(|a| a == "--delay")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    // ── 1. Load keys & build clients ────────────────────────────────────

    let private_key = env::var("POLYMARKET_PRIVATE_KEY")
        .map_err(|_| anyhow::anyhow!("Missing POLYMARKET_PRIVATE_KEY in .env"))?;

    let wallet_address = env::var("POLY_RELAYER_ADDRESS")
        .map_err(|_| anyhow::anyhow!("Missing POLY_RELAYER_ADDRESS in .env"))?;

    let rpc_url = env::var("POLYGON_RPC_URL")
        .map_err(|_| anyhow::anyhow!("Missing POLYGON_RPC_URL in .env"))?;

    let auth = if let (Ok(key), Ok(secret), Ok(pass)) = (
        env::var("BUILDER_KEY"),
        env::var("BUILDER_SECRET"),
        env::var("BUILDER_PASSPHRASE"),
    ) {
        println!("[auth] Builder HMAC — gasless mode");
        AuthMethod::builder(&key, &secret, &pass)
    } else if let Ok(api_key) = env::var("POLY_RELAYER_API_KEY") {
        println!("[auth] Relayer API key");
        AuthMethod::relayer_key(&api_key, &wallet_address)
    } else {
        anyhow::bail!(
            "Set BUILDER_KEY/SECRET/PASSPHRASE or POLY_RELAYER_API_KEY in .env"
        );
    };

    let wallet: LocalWallet = private_key.parse()?;

    // Wallet type: 0=EOA, 1=Proxy (magic.link), 2=Safe (default)
    let sig_type: u8 = env::var("SIGNATURE_TYPE")
        .unwrap_or_else(|_| "2".to_string())
        .parse()
        .unwrap_or(2);
    let tx_type = RelayerTxType::from_signature_type(sig_type)
        .unwrap_or_else(|| {
            eprintln!("[warn] Unknown SIGNATURE_TYPE={sig_type}, defaulting to Safe (2)");
            RelayerTxType::Safe
        });

    let mut client = RelayClient::new(137, wallet.clone(), auth, tx_type).await?;
    client.set_rpc_url(rpc_url.clone());

    let direct = match tx_type {
        RelayerTxType::Proxy => {
            let proxy_addr: Address = wallet_address.parse()?;
            DirectExecutor::new_proxy_with_address(&rpc_url, wallet, 137, proxy_addr)?
        }
        _ => DirectExecutor::with_type(&rpc_url, wallet, 137, tx_type)?,
    };
    let matic = direct.get_matic_balance().await.unwrap_or(0.0);

    let mode_str = if batch_mode { "batch".to_string() } else { format!("sequential ({}s delay)", delay_secs) };
    println!("[info] EOA:    {:?}", client.signer_address());
    println!("[info] Wallet: {:?} ({})", client.wallet_address()?, tx_type.as_str());
    println!("[info] MATIC:  {:.4}", matic);
    println!("[info] Mode:   {}", mode_str);

    // ── 2. Fetch all positions ──────────────────────────────────────────

    let data = DataClient::default();

    let addr: polymarket_client_sdk::types::Address = wallet_address
        .parse()
        .map_err(|e| anyhow::anyhow!("Bad wallet address: {e}"))?;

    let positions = data
        .positions(
            &PositionsRequest::builder()
                .user(addr)
                .limit(500)?
                .build(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to fetch positions: {e}"))?;

    if positions.is_empty() {
        println!("\nNo positions found for {wallet_address}");
        return Ok(());
    }

    // ── 3. Filter settled (redeemable) positions ────────────────────────

    println!("\n{:=<150}", "= POSITIONS ");
    println!(
        "  {:<3} {:<40} {:<6} {:<10} {:<8} {:<66} {}",
        "#", "Market", "Side", "Shares", "Status", "Condition ID", "Value"
    );

    let mut redeemable = Vec::new();
    let mut expected_usdc = Decimal::ZERO;

    for (i, pos) in positions.iter().enumerate() {
        let title = truncate(&pos.title, 38);
        let won = pos.cur_price >= Decimal::new(95, 2);

        let (status, value) = if pos.redeemable {
            if won {
                expected_usdc += pos.size;
                ("WON", format!("${:.2}", pos.size))
            } else {
                ("LOST", "$0.00".into())
            }
        } else {
            ("ACTIVE", format!("~${:.2}", pos.current_value))
        };

        let cid_hex = format!("0x{}", hex::encode(pos.condition_id));

        println!(
            "  {:<3} {:<40} {:<6} {:<10} {:<8} {:<66} {}",
            i + 1, title, pos.outcome, pos.size, status, cid_hex, value,
        );

        if pos.redeemable {
            redeemable.push(pos);
        }
    }

    println!(
        "\n  Redeemable: {} | Expected USDC: ~${:.2}\n",
        redeemable.len(), expected_usdc,
    );

    if redeemable.is_empty() {
        println!("Nothing to redeem.");
        return Ok(());
    }

    if !execute {
        println!("=== DRY RUN — pass --execute to actually redeem ===");
        return Ok(());
    }

    // ── 4. Build redeem transactions (deduplicate by condition_id) ──────

    let mut seen_conditions = HashSet::new();
    let mut redeem_txs: Vec<(String, String, Transaction)> = Vec::new(); // (title, kind, tx)

    for pos in &redeemable {
        let cid_hex = format!("0x{}", hex::encode(pos.condition_id));
        if !seen_conditions.insert(cid_hex.clone()) {
            continue;
        }

        let title = truncate(&pos.title, 38);
        let kind = if pos.negative_risk { "NegRisk" } else { "CTF" };
        let cid_bytes: [u8; 32] = *pos.condition_id;
        let tx = if pos.negative_risk {
            operations::redeem_neg_risk_positions(cid_bytes, &[1, 2])
        } else {
            operations::redeem_regular(cid_bytes, &[1, 2])
        };
        redeem_txs.push((title, kind.to_string(), tx));
    }

    let total = redeem_txs.len();
    println!("{:=<70}", format!("= REDEEMING {} [{}] ", total, if batch_mode { "batch" } else { "sequential" }));

    // ── 5a. Batch mode: single relay request ───────────────────────────

    if batch_mode {
        let txs: Vec<Transaction> = redeem_txs.iter().map(|(_, _, tx)| tx.clone()).collect();

        match client.execute_batch(txs, "Batch redeem all").await {
            Ok(result) => {
                let hash = result.tx_hash.as_deref().unwrap_or("unknown");
                println!("  [OK]   Batch redeemed {} condition(s) | tx: {}", total, short_hash(hash));
                for (t, k, _) in &redeem_txs {
                    println!("         - {} ({})", t, k);
                }
                println!("\n{:=<70}", "= SUMMARY ");
                println!("  Redeemed: {total}/{total} condition(s)");
            }
            Err(e) => {
                println!("  [FAIL] Batch failed: {}", e);
                println!("         Try without --batch (sequential mode) instead.");
                println!("\n{:=<70}", "= SUMMARY ");
                println!("  Redeemed: 0/{total} condition(s)");
                println!("  Failed:   1");
            }
        }
        println!("  Expected USDC: ~${:.2}", expected_usdc);
        return Ok(());
    }

    // ── 5b. Sequential mode: one-at-a-time with wait and fallback ──────

    let mut successful = 0;
    let mut failed = 0;

    // Proxy wallets can only use gasless — direct fallback is not available
    // (proxy contract requires msg.sender == factory, not EOA)
    let can_direct = tx_type == RelayerTxType::Safe;

    for (i, (title, kind, tx)) in redeem_txs.iter().enumerate() {
        println!("\n[{} / {}] Redeeming: {} ({})", i + 1, total, title, kind);

        let txs = vec![tx.clone()];
        let gasless_result = match client.execute(txs, title).await {
            Ok(handle) => match handle.wait().await {
                Ok(result) => {
                    let hash = result.tx_hash.as_deref().unwrap_or("unknown");
                    println!("  [OK]   gasless | tx: {}", short_hash(hash));
                    successful += 1;
                    true
                }
                Err(e) => {
                    println!("  [WARN] Relayer tx failed: {}", e);
                    false
                }
            },
            Err(e) => {
                println!("  [WARN] Relayer API error: {}", e);
                false
            }
        };

        // Direct fallback (Safe only — Proxy wallets cannot be called directly)
        if !gasless_result {
            if can_direct {
                println!("  ... falling back to direct on-chain (MATIC gas)");
                match direct.execute(tx).await {
                    Ok(res) => {
                        println!("  [OK]   direct | gas: {:.5} MATIC | tx: {}", res.gas_cost_matic, short_hash(&res.tx_hash));
                        successful += 1;
                    }
                    Err(err) => {
                        println!("  [FAIL] Direct also failed: {}", err);
                        failed += 1;
                    }
                }
            } else {
                println!("  [FAIL] No fallback for {} wallets — gasless is the only path", tx_type.as_str());
                failed += 1;
            }
        }

        if i + 1 < total {
            println!("  Waiting {}s before next...", delay_secs);
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
    }

    println!("\n{:=<70}", "= SUMMARY ");
    println!("  Redeemed: {}/{} condition(s)", successful, total);
    if failed > 0 {
        println!("  Failed:   {}", failed);
    }
    println!("  Expected USDC: ~${:.2}", expected_usdc);

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max - 3).collect();
        format!("{cut}...")
    }
}

fn short_hash(h: &str) -> String {
    if h.len() > 14 {
        format!("{}...{}", &h[..8], &h[h.len() - 4..])
    } else {
        h.to_string()
    }
}
