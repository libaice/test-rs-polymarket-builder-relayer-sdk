//! Test script: query all settled Polymarket positions and batch redeem them.
//!
//! Usage:
//!   cargo run                  # dry-run (list positions only)
//!   cargo run -- --execute     # actually redeem

use ethers::signers::LocalWallet;
use polymarket_client_sdk::data::Client as DataClient;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_relayer::{
    operations, AuthMethod, DirectExecutor, RelayClient, RelayerError, RelayerTxType, Transaction,
};
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::env;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let _ = dotenvy::dotenv();

    let execute = env::args().any(|a| a == "--execute");

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
    let client = RelayClient::new(137, wallet.clone(), auth, RelayerTxType::Safe).await?;
    let direct = DirectExecutor::new(&rpc_url, wallet, 137)?;
    let matic = direct.get_matic_balance().await.unwrap_or(0.0);

    println!("[info] EOA:   {:?}", client.signer_address());
    println!("[info] Safe:  {:?}", client.wallet_address()?);
    println!("[info] MATIC: {:.4}", matic);

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
            i + 1,
            title,
            pos.outcome,
            pos.size,
            status,
            cid_hex,
            value,
        );

        if pos.redeemable {
            redeemable.push(pos);
        }
    }

    println!(
        "\n  Redeemable: {} | Expected USDC: ~${:.2}\n",
        redeemable.len(),
        expected_usdc,
    );

    if redeemable.is_empty() {
        println!("Nothing to redeem.");
        return Ok(());
    }

    if !execute {
        println!("=== DRY RUN — pass --execute to actually redeem ===");
        return Ok(());
    }

    // ── 4. Batch redeem all settled positions ───────────────────────────

    println!("{:=<70}", "= REDEEMING ");

    let mut seen_conditions = HashSet::new();
    let mut success = 0u32;
    let mut failed = 0u32;
    let mut gas_spent = 0.0f64;

    for pos in &redeemable {
        let cid_hex = format!("0x{}", hex::encode(pos.condition_id));

        // Each condition_id only needs to be redeemed once (covers both outcomes)
        if !seen_conditions.insert(cid_hex.clone()) {
            continue;
        }

        let title = truncate(&pos.title, 38);
        let cid_bytes: [u8; 32] = *pos.condition_id;
        let won = pos.cur_price >= Decimal::new(95, 2);
        let kind = if pos.negative_risk { "NegRisk" } else { "CTF" };

        let tx = if pos.negative_risk {
            operations::redeem_neg_risk_positions(cid_bytes, &[1, 2])
        } else {
            operations::redeem_regular(cid_bytes, &[1, 2])
        };

        let usdc_label = if won {
            format!("+${:.2}", pos.size)
        } else {
            "$0.00".into()
        };

        // Try gasless relayer first
        match try_relayer(&client, &tx, &pos.title).await {
            Ok(hash) => {
                println!(
                    "  [OK]   {:<40} | {} | {} | gasless | tx: {}",
                    title, usdc_label, kind, short_hash(&hash),
                );
                success += 1;
                continue;
            }
            Err(RelayerError::QuotaExhausted) => {
                println!("  [429]  {} — relayer quota hit, falling back to direct", title);
            }
            Err(e) => {
                println!("  [WARN] {} — relayer error: {}, trying direct", title, e);
            }
        }

        // Fallback: direct on-chain
        match direct.execute(&tx).await {
            Ok(r) if r.success => {
                gas_spent += r.gas_cost_matic;
                println!(
                    "  [OK]   {:<40} | {} | {} | direct | gas: {:.5} MATIC | tx: {}",
                    title, usdc_label, kind, r.gas_cost_matic, short_hash(&r.tx_hash),
                );
                success += 1;
            }
            Ok(r) => {
                println!("  [FAIL] {} | reverted | tx: {}", title, short_hash(&r.tx_hash));
                failed += 1;
            }
            Err(e) => {
                println!("  [FAIL] {} | {}", title, e);
                failed += 1;
            }
        }
    }

    // ── 5. Summary ──────────────────────────────────────────────────────

    println!("\n{:=<70}", "= SUMMARY ");
    println!("  Redeemed: {success}/{} condition(s)", seen_conditions.len());
    if failed > 0 {
        println!("  Failed:   {failed}");
    }
    println!("  Expected USDC: ~${:.2}", expected_usdc);
    if gas_spent > 0.0 {
        println!("  Gas (direct):  ~{:.6} MATIC", gas_spent);
    }

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────

async fn try_relayer(
    client: &RelayClient,
    tx: &Transaction,
    description: &str,
) -> polymarket_relayer::Result<String> {
    let handle = client
        .execute(vec![tx.clone()], &format!("Redeem: {}", description))
        .await?;
    let tx_id = handle.id().to_string();
    let result = handle.wait().await?;
    Ok(result.tx_hash.unwrap_or(tx_id))
}

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
