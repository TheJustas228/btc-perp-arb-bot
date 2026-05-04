//! Read-only smoke test for MEXC + Bitfinex API keys.
//!
//! Loads keys from `.env`, makes one authenticated read-only call per venue,
//! prints a redacted summary, and exits non-zero on any failure.
//!
//! It never logs the secrets, never sends an order, never withdraws.
//! Run with: `cargo run --bin verify_keys`

use anyhow::{anyhow, bail, Context, Result};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::Value;
use sha2::{Sha256, Sha384};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;
type HmacSha384 = Hmac<Sha384>;

const MEXC_LIVE_HOST: &str = "https://contract.mexc.com";
const MEXC_TESTNET_HOST: &str = "https://futures.testnet.mexc.com";
const BITFINEX_HOST: &str = "https://api.bitfinex.com";

#[derive(Clone, Copy)]
enum Venue {
    MexcLive,
    MexcPaper,
    BitfinexLive,
    BitfinexPaper,
}

impl Venue {
    fn label(&self) -> &'static str {
        match self {
            Venue::MexcLive => "MEXC live",
            Venue::MexcPaper => "MEXC paper (testnet)",
            Venue::BitfinexLive => "Bitfinex live",
            Venue::BitfinexPaper => "Bitfinex paper subaccount",
        }
    }

    fn host(&self) -> &'static str {
        match self {
            Venue::MexcLive => MEXC_LIVE_HOST,
            Venue::MexcPaper => MEXC_TESTNET_HOST,
            Venue::BitfinexLive | Venue::BitfinexPaper => BITFINEX_HOST,
        }
    }

    fn key_var(&self) -> &'static str {
        match self {
            Venue::MexcLive => "MEXC_API_KEY",
            Venue::MexcPaper => "MEXC_API_KEY_PAPER",
            Venue::BitfinexLive => "BITFINEX_API_KEY",
            Venue::BitfinexPaper => "BITFINEX_API_KEY_PAPER",
        }
    }

    fn secret_var(&self) -> &'static str {
        match self {
            Venue::MexcLive => "MEXC_API_SECRET",
            Venue::MexcPaper => "MEXC_API_SECRET_PAPER",
            Venue::BitfinexLive => "BITFINEX_API_SECRET",
            Venue::BitfinexPaper => "BITFINEX_API_SECRET_PAPER",
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let live_flag = std::env::var("ARB_LIVE_TRADING")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "false".to_string());

    println!("Loaded credentials from .env (values not displayed)");
    println!("ARB_LIVE_TRADING = {live_flag}");
    if live_flag != "false" {
        println!("WARNING: ARB_LIVE_TRADING is not 'false' — this smoke test still won't trade,");
        println!("         but you should keep this off until live execution is wired up.");
    }
    println!();

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("btc-usdt-arb-verify/1.0")
        .build()?;

    let venues = [
        Venue::MexcLive,
        Venue::MexcPaper,
        Venue::BitfinexLive,
        Venue::BitfinexPaper,
    ];

    let mut checked = 0usize;
    let mut failed = 0usize;
    let mut skipped = Vec::new();

    for venue in venues {
        let creds = match optional_creds(venue) {
            Some(creds) => creds,
            None => {
                skipped.push(venue.label());
                continue;
            }
        };

        if matches!(venue, Venue::MexcPaper) {
            println!("=== {} ===", venue.label());
            println!(
                "    MEXC does not expose a public API for its futures testnet."
            );
            println!(
                "    Their demo UI at futures.testnet.mexc.com is web-only; the backend"
            );
            println!(
                "    rejects third-party API keys (per MEXC support: \"MEXC API connects"
            );
            println!(
                "    directly to the live trading environment. We don't currently offer"
            );
            println!("    a sandbox or test environment.\")");
            println!(
                "    Recommended: remove MEXC_API_KEY_PAPER / MEXC_API_SECRET_PAPER from .env,"
            );
            println!(
                "    or replace with Binance Futures Testnet keys (testnet.binancefuture.com)"
            );
            println!("    if you want USDT-M perp execution practice with real sandbox auth.");
            println!("MEXC paper (testnet): SKIPPED (no API testnet)\n");
            skipped.push(venue.label());
            continue;
        }

        checked += 1;
        println!("=== {} ({}) ===", venue.label(), venue.host());
        println!(
            "    {} length = {}, {} length = {}",
            venue.key_var(),
            creds.0.len(),
            venue.secret_var(),
            creds.1.len(),
        );

        let result = match venue {
            Venue::MexcLive | Venue::MexcPaper => {
                check_mexc(&client, venue.host(), &creds.0, &creds.1).await
            }
            Venue::BitfinexLive | Venue::BitfinexPaper => {
                check_bitfinex(&client, venue.host(), &creds.0, &creds.1, venue).await
            }
        };

        match result {
            Ok(()) => println!("{}: OK\n", venue.label()),
            Err(e) => {
                failed += 1;
                println!("{}: FAILED — {e:#}\n", venue.label());
            }
        }
    }

    if !skipped.is_empty() {
        println!("Skipped (no credentials configured):");
        for label in &skipped {
            println!("  - {label}");
        }
        println!();
    }

    if checked == 0 {
        bail!("no credentials configured — set at least one pair in .env");
    }
    if failed > 0 {
        bail!("{failed} of {checked} checks failed (see output above)");
    }
    println!("All {checked} configured credential set(s) verified.");
    Ok(())
}

fn optional_creds(venue: Venue) -> Option<(String, String)> {
    let key = read_env(venue.key_var())?;
    let secret = read_env(venue.secret_var())?;
    Some((key, secret))
}

fn read_env(name: &str) -> Option<String> {
    let raw = std::env::var(name).ok()?;
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'').to_string();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

async fn check_mexc(client: &Client, host: &str, key: &str, secret: &str) -> Result<()> {
    let path = "/api/v1/private/account/assets";
    let timestamp = unix_ms()?.to_string();

    let signature_payload = format!("{key}{timestamp}");
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|e| anyhow!("hmac key: {e}"))?;
    mac.update(signature_payload.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    let response = client
        .get(format!("{host}{path}"))
        .header("ApiKey", key)
        .header("Request-Time", &timestamp)
        .header("Signature", &signature)
        .header("Content-Type", "application/json")
        .send()
        .await
        .context("request to MEXC failed (network or IP whitelist?)")?;

    let status = response.status();
    let text = response.text().await.context("MEXC response read")?;
    let body: Value = serde_json::from_str(&text)
        .unwrap_or_else(|_| Value::String(format!("non-JSON body: {text}")));

    if !status.is_success() {
        bail!("HTTP {status}: {body}");
    }

    let success = body.get("success").and_then(Value::as_bool).unwrap_or(false);
    let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if !success && code != 0 {
        let msg = body
            .get("message")
            .or_else(|| body.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        bail!("API rejected the call: code={code} msg={msg}");
    }

    let assets = body
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if assets.is_empty() {
        println!("  authenticated, but the futures account has no assets reported.");
        println!("  (a USDT-margin futures wallet only shows here once funded)");
    } else {
        let mut shown = 0usize;
        println!("  Authenticated. Futures account assets:");
        for asset in &assets {
            let currency = asset
                .get("currency")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let available = num_field(asset, "availableBalance");
            let equity = num_field(asset, "equity");
            let frozen = num_field(asset, "frozenBalance");
            if available.unwrap_or(0.0) == 0.0
                && equity.unwrap_or(0.0) == 0.0
                && frozen.unwrap_or(0.0) == 0.0
            {
                continue;
            }
            shown += 1;
            println!(
                "    {currency:<6} available={a:>14} equity={e:>14} frozen={f:>14}",
                a = fmt_opt(available),
                e = fmt_opt(equity),
                f = fmt_opt(frozen),
            );
        }
        if shown == 0 {
            println!("    (all asset rows are zero — futures wallet exists but is empty)");
        }
    }

    Ok(())
}

async fn check_bitfinex(
    client: &Client,
    host: &str,
    key: &str,
    secret: &str,
    venue: Venue,
) -> Result<()> {
    let is_paper = matches!(venue, Venue::BitfinexPaper);

    let wallets = bfx_post(client, host, key, secret, "/auth/r/wallets", "{}").await?;
    let wallets_arr = wallets.as_array().cloned().unwrap_or_default();
    if wallets_arr.is_empty() {
        println!("  Authenticated, but no wallets returned (account empty).");
    } else {
        println!("  Authenticated. Wallets:");
        let mut has_real_perp_collateral = false;
        let mut has_test_perp_collateral = false;
        let mut has_usdt = false;
        let mut has_margin = false;
        for w in &wallets_arr {
            let arr = match w.as_array() {
                Some(a) => a,
                None => continue,
            };
            let wtype = arr.first().and_then(Value::as_str).unwrap_or("?");
            let currency = arr.get(1).and_then(Value::as_str).unwrap_or("?");
            let balance = arr.get(2).and_then(Value::as_f64).unwrap_or(0.0);
            let available = arr.get(4).and_then(Value::as_f64).unwrap_or(0.0);
            if currency == "USTF0" {
                has_real_perp_collateral = true;
            }
            if currency == "TESTUSDTF0" {
                has_test_perp_collateral = true;
            }
            if currency == "UST" || currency == "USDT" || currency == "TESTUSDT" {
                has_usdt = true;
            }
            if wtype == "margin" {
                has_margin = true;
            }
            if balance != 0.0 || available != 0.0 {
                println!(
                    "    [{wtype:<8}] {currency:<10} balance={balance:>14.6} available={available:>14.6}"
                );
            }
        }
        println!();
        if is_paper {
            println!(
                "  TESTUSDTF0 wallet present: {}",
                if has_test_perp_collateral { "yes" } else { "NO" }
            );
            println!(
                "  TESTUSDT/UST wallet present: {}",
                if has_usdt { "yes" } else { "no" }
            );
            println!(
                "  margin wallet present: {}",
                if has_margin { "yes" } else { "no" }
            );
            if !has_test_perp_collateral {
                println!();
                println!("  NOTE: this paper subaccount has no TESTUSDTF0 balance.");
                println!("        Top up via Trading -> Refill Paper Balances before placing");
                println!("        orders on tTESTBTCF0:TESTUSDTF0.");
            }
        } else {
            println!(
                "  USTF0 wallet present: {}",
                if has_real_perp_collateral { "yes" } else { "NO" }
            );
            println!(
                "  USDT/UST wallet present: {}",
                if has_usdt { "yes" } else { "no" }
            );
            println!(
                "  margin wallet present: {}",
                if has_margin { "yes" } else { "no" }
            );
            if !has_real_perp_collateral {
                println!();
                println!("  NOTE: BTCF0:USTF0 (Bitfinex BTC perpetual) is settled in USTF0,");
                println!("        not USDT. With no USTF0 wallet you cannot execute the");
                println!("        MEXC<->Bitfinex 'arbitrage' against this account without");
                println!("        first converting funds to USTF0 on Bitfinex.");
            }
        }
    }

    println!();
    let info = bfx_post(client, host, key, secret, "/auth/r/info/user", "{}").await?;
    if let Some(arr) = info.as_array() {
        let user_id = arr.first().and_then(Value::as_i64);
        let verified = arr.get(20).and_then(Value::as_i64).unwrap_or(0);
        let timezone = arr.get(7).and_then(Value::as_str).unwrap_or("?");
        match user_id {
            Some(uid) => println!(
                "  /info/user OK (user_id={uid}, verified={verified}, tz={timezone})"
            ),
            None => println!("  /info/user OK (no user_id field)"),
        }
    } else {
        println!("  /info/user OK (unexpected shape: {info})");
    }

    let summary = bfx_post(client, host, key, secret, "/auth/r/summary", "{}").await;
    match summary {
        Ok(value) => {
            if let Some(arr) = value.as_array() {
                let trade_vol_30d = arr
                    .get(5)
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(Value::as_array)
                    .and_then(|a| a.get(0))
                    .and_then(Value::as_f64);
                let maker_fees = arr
                    .get(4)
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(Value::as_array)
                    .and_then(|a| a.get(0))
                    .and_then(Value::as_f64);
                let taker_fees = arr
                    .get(4)
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(Value::as_array)
                    .and_then(|a| a.get(1))
                    .and_then(Value::as_f64);
                println!(
                    "  /summary OK (30d_vol={vol}, maker_fee={maker}, taker_fee={taker})",
                    vol = fmt_opt(trade_vol_30d),
                    maker = fmt_opt(maker_fees),
                    taker = fmt_opt(taker_fees),
                );
            } else {
                println!("  /summary OK");
            }
        }
        Err(e) => {
            println!("  /summary not available: {e:#}");
        }
    }

    Ok(())
}

async fn bfx_post(
    client: &Client,
    host: &str,
    key: &str,
    secret: &str,
    api_path: &str,
    body: &str,
) -> Result<Value> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before epoch")?
        .as_micros()
        .to_string();
    let signature_payload = format!("/api/v2{api_path}{nonce}{body}");
    let request_url = format!("{host}/v2{api_path}");
    let mut mac =
        HmacSha384::new_from_slice(secret.as_bytes()).map_err(|e| anyhow!("hmac key: {e}"))?;
    mac.update(signature_payload.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    let response = client
        .post(&request_url)
        .header("bfx-apikey", key)
        .header("bfx-nonce", &nonce)
        .header("bfx-signature", &signature)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .with_context(|| format!("request to Bitfinex {api_path} failed"))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .with_context(|| format!("Bitfinex {api_path} response read"))?;
    if !status.is_success() {
        bail!("{api_path}: HTTP {status}: {text}");
    }

    let parsed: Value = serde_json::from_str(&text)
        .with_context(|| format!("Bitfinex {api_path} returned non-JSON: {text}"))?;
    if let Some(arr) = parsed.as_array() {
        if arr.first().and_then(Value::as_str) == Some("error") {
            bail!("{api_path} returned error: {parsed}");
        }
    }
    Ok(parsed)
}

fn unix_ms() -> Result<u128> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before epoch")?
        .as_millis())
}

fn num_field(value: &Value, key: &str) -> Option<f64> {
    let field = value.get(key)?;
    if let Some(n) = field.as_f64() {
        return Some(n);
    }
    field.as_str().and_then(|s| s.parse::<f64>().ok())
}

fn fmt_opt(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{v:.6}"),
        None => "?".to_string(),
    }
}
