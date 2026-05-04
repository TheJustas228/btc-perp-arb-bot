use axum::{extract::State, response::IntoResponse, routing::{get, post}, Json, Router};
use chrono::Utc;
use futures::future::join_all;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashSet, VecDeque},
    net::SocketAddr,
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::{fs, net::TcpListener, sync::RwLock, time::sleep};
use tower_http::services::ServeDir;

const PAIR: &str = "BTC/USDT PERP";
const POLL_MS_MIN: u64 = 750;
const REQUEST_TIMEOUT_MS_MIN: u64 = 500;
const MAX_RECORDED_OPPORTUNITIES: usize = 100;
const ABSOLUTE_MAX_SIZE_BTC: f64 = 0.5;
const DEFAULT_ACCOUNT_CAPITAL_USD: f64 = 587.0;
const ACCOUNT_LEVERAGE: f64 = 10.0;
const DEFAULT_MIN_EXECUTABLE_NET: f64 = 1.0;
const PAPER_CHECK_DELAYS_MS: [u64; 4] = [250, 500, 1000, 2000];
const MAX_PAPER_CHECKS: usize = 100;
const MAX_FAKE_TRADES: usize = 20;
const MAX_OPEN_POSITIONS: usize = 50;
const MAX_CLOSED_TRADES: usize = 100;
const POSITION_CLOSE_TRIGGER: f64 = 0.0;
const POSITION_TIMEOUT_TICKS: u32 = 600;
const RECORDS_PATH: &str = "data/interesting-opportunities.json";

#[derive(Clone)]
struct Exchange {
    id: &'static str,
    name: &'static str,
    fee_rate: f64,
    ticker_url: &'static str,
    depth_url: Option<&'static str>,
    /// Settlement-asset tier the venue belongs to.
    ///
    /// Only [`VenueTier::UsdtLinearPerp`] venues are eligible to be the
    /// buy or sell leg of an executable arb. Other tiers (different
    /// settlement asset, persistent basis, or wide internal book) stay
    /// in the price matrix as a reference but never feed into
    /// `find_executable_scenarios_for_opportunities`.
    tier: VenueTier,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
enum VenueTier {
    /// Linear perpetuals quoted and margined in USDT (or near-USDT
    /// equivalents like UST). All routes between two venues in this
    /// tier are tradeable in principle.
    UsdtLinearPerp,
    /// Settlement asset differs from USDT (USTF0, USD, USDC, etc.) so
    /// the cross-venue spread carries a structural basis that doesn't
    /// reliably mean-revert. Useful as a price reference, never as an
    /// executable leg.
    DifferentSettlement,
    /// Internal book is structurally wide or contract semantics differ
    /// enough (contract multipliers, illiquid top-of-book, etc.) that
    /// the displayed mid is misleading for execution. Reference only.
    UnreliableBook,
}

#[derive(Clone, Serialize, Deserialize)]
struct PriceRow {
    id: String,
    name: String,
    price: Option<f64>,
    bid: Option<f64>,
    ask: Option<f64>,
    #[serde(rename = "feeRate")]
    fee_rate: f64,
    /// Settlement-asset tier of the venue. Only `UsdtLinearPerp` rows
    /// are considered when generating executable arb candidates;
    /// other tiers stay in the price matrix as a reference.
    #[serde(default = "default_tier")]
    tier: VenueTier,
    ok: bool,
    error: Option<String>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
    #[serde(rename = "latencyMs")]
    latency_ms: Option<u128>,
}

fn default_tier() -> VenueTier {
    VenueTier::UsdtLinearPerp
}

#[derive(Clone, Serialize, Deserialize)]
struct Opportunity {
    id: String,
    #[serde(rename = "buyExchange")]
    buy_exchange: Option<String>,
    #[serde(rename = "sellExchange")]
    sell_exchange: Option<String>,
    #[serde(rename = "sizeBtc", skip_serializing_if = "Option::is_none")]
    size_btc: Option<f64>,
    #[serde(rename = "buyPrice")]
    buy_price: Option<f64>,
    #[serde(rename = "sellPrice")]
    sell_price: Option<f64>,
    #[serde(rename = "grossDifference")]
    gross_difference: Option<f64>,
    #[serde(rename = "grossPercentageDifference")]
    gross_percentage_difference: Option<f64>,
    #[serde(rename = "estimatedFees")]
    estimated_fees: Option<f64>,
    #[serde(rename = "estimatedNetProfit")]
    estimated_net_profit: Option<f64>,
    status: String,
    #[serde(rename = "detectedAt")]
    detected_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct DepthSimulation {
    #[serde(rename = "maxSizeBtc")]
    max_size_btc: Option<f64>,
    #[serde(rename = "sizeBtc")]
    size_btc: f64,
    #[serde(rename = "buyExchange")]
    buy_exchange: Option<String>,
    #[serde(rename = "sellExchange")]
    sell_exchange: Option<String>,
    #[serde(rename = "buyAveragePrice")]
    buy_average_price: Option<f64>,
    #[serde(rename = "sellAveragePrice")]
    sell_average_price: Option<f64>,
    #[serde(rename = "buyNotional")]
    buy_notional: Option<f64>,
    #[serde(rename = "sellNotional")]
    sell_notional: Option<f64>,
    #[serde(rename = "grossDifference")]
    gross_difference: Option<f64>,
    #[serde(rename = "grossProfit")]
    gross_profit: Option<f64>,
    #[serde(rename = "estimatedFees")]
    estimated_fees: Option<f64>,
    #[serde(rename = "estimatedNetProfit")]
    estimated_net_profit: Option<f64>,
    status: String,
    #[serde(rename = "simulatedAt")]
    simulated_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct AccountSizing {
    #[serde(rename = "capitalUsd")]
    capital_usd: f64,
    leverage: f64,
    #[serde(rename = "maxNotionalPerLeg")]
    max_notional_per_leg: f64,
    #[serde(rename = "maxSizeBtc")]
    max_size_btc: f64,
}

#[derive(Clone, Serialize, Deserialize)]
struct PaperCheck {
    id: String,
    #[serde(rename = "buyExchange")]
    buy_exchange: String,
    #[serde(rename = "sellExchange")]
    sell_exchange: String,
    #[serde(rename = "sizeBtc")]
    size_btc: f64,
    #[serde(rename = "initialNet")]
    initial_net: f64,
    #[serde(rename = "minExecutableNet")]
    min_executable_net: f64,
    #[serde(rename = "delayMs")]
    delay_ms: u64,
    #[serde(rename = "recheckNet")]
    recheck_net: Option<f64>,
    #[serde(rename = "netChange")]
    net_change: Option<f64>,
    survived: bool,
    status: String,
    #[serde(rename = "detectedAt")]
    detected_at: String,
    #[serde(rename = "checkedAt")]
    checked_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct FakeTrade {
    id: String,
    #[serde(rename = "buyExchange")]
    buy_exchange: String,
    #[serde(rename = "sellExchange")]
    sell_exchange: String,
    #[serde(rename = "sizeBtc")]
    size_btc: f64,
    #[serde(rename = "buyAveragePrice")]
    buy_average_price: f64,
    #[serde(rename = "sellAveragePrice")]
    sell_average_price: f64,
    #[serde(rename = "buyNotional")]
    buy_notional: f64,
    #[serde(rename = "sellNotional")]
    sell_notional: f64,
    #[serde(rename = "estimatedFees")]
    estimated_fees: f64,
    #[serde(rename = "estimatedNet")]
    estimated_net: f64,
    #[serde(rename = "marginUsed")]
    margin_used: f64,
    status: String,
    #[serde(rename = "createdAt")]
    created_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct OpenPosition {
    id: String,
    #[serde(rename = "buyExchange")]
    buy_exchange: String,
    #[serde(rename = "sellExchange")]
    sell_exchange: String,
    #[serde(rename = "sizeBtc")]
    size_btc: f64,
    #[serde(rename = "entryBuyPrice")]
    entry_buy_price: f64,
    #[serde(rename = "entrySellPrice")]
    entry_sell_price: f64,
    #[serde(rename = "entryFees")]
    entry_fees: f64,
    #[serde(rename = "entryEstimatedNet")]
    entry_estimated_net: f64,
    #[serde(rename = "buyFeeRate")]
    buy_fee_rate: f64,
    #[serde(rename = "sellFeeRate")]
    sell_fee_rate: f64,
    #[serde(rename = "ticksHeld")]
    ticks_held: u32,
    #[serde(rename = "currentExitSpread")]
    current_exit_spread: Option<f64>,
    #[serde(rename = "currentTriggerSpread")]
    current_trigger_spread: Option<f64>,
    #[serde(rename = "currentUnrealizedNet")]
    current_unrealized_net: Option<f64>,
    #[serde(rename = "openedAt")]
    opened_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct ClosedTrade {
    id: String,
    #[serde(rename = "buyExchange")]
    buy_exchange: String,
    #[serde(rename = "sellExchange")]
    sell_exchange: String,
    #[serde(rename = "sizeBtc")]
    size_btc: f64,
    #[serde(rename = "entryBuyPrice")]
    entry_buy_price: f64,
    #[serde(rename = "entrySellPrice")]
    entry_sell_price: f64,
    #[serde(rename = "exitBuyPrice")]
    exit_buy_price: f64,
    #[serde(rename = "exitSellPrice")]
    exit_sell_price: f64,
    #[serde(rename = "entryFees")]
    entry_fees: f64,
    #[serde(rename = "exitFees")]
    exit_fees: f64,
    #[serde(rename = "entryEstimatedNet")]
    entry_estimated_net: f64,
    #[serde(rename = "realizedGross")]
    realized_gross: f64,
    #[serde(rename = "realizedNet")]
    realized_net: f64,
    #[serde(rename = "pnlVsEstimated")]
    pnl_vs_estimated: f64,
    #[serde(rename = "ticksHeld")]
    ticks_held: u32,
    #[serde(rename = "closedReason")]
    closed_reason: String,
    #[serde(rename = "openedAt")]
    opened_at: String,
    #[serde(rename = "closedAt")]
    closed_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    pair: String,
    #[serde(rename = "pollMs")]
    poll_ms: u64,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
    prices: Vec<PriceRow>,
    opportunities: Vec<Opportunity>,
    #[serde(rename = "recordedOpportunities")]
    recorded_opportunities: Vec<Opportunity>,
    #[serde(rename = "maxSpreadRecord")]
    max_spread_record: Option<Opportunity>,
    #[serde(rename = "highestNetRecord")]
    highest_net_record: Option<Opportunity>,
    #[serde(rename = "maxProfitableSize")]
    max_profitable_size: DepthSimulation,
    #[serde(rename = "maxProfitableScenarios")]
    max_profitable_scenarios: Vec<DepthSimulation>,
    #[serde(rename = "cumulativeDepthNet")]
    cumulative_depth_net: f64,
    #[serde(rename = "minDepthNetThreshold")]
    min_depth_net_threshold: f64,
    #[serde(rename = "accountSizing")]
    account_sizing: AccountSizing,
    #[serde(rename = "paperChecks")]
    paper_checks: Vec<PaperCheck>,
    #[serde(rename = "fakeTrades")]
    fake_trades: Vec<FakeTrade>,
    #[serde(rename = "fakeCumulativeNet")]
    fake_cumulative_net: f64,
    #[serde(rename = "fakeTradesResetAt")]
    fake_trades_reset_at: String,
    #[serde(rename = "openPositions")]
    open_positions: Vec<OpenPosition>,
    #[serde(rename = "closedTrades")]
    closed_trades: Vec<ClosedTrade>,
    #[serde(rename = "realizedCumulativeNet")]
    realized_cumulative_net: f64,
    #[serde(rename = "realizedTradesResetAt")]
    realized_trades_reset_at: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct StoredRecords {
    #[serde(rename = "maxSpreadRecord")]
    max_spread_record: Option<Opportunity>,
    #[serde(rename = "highestNetRecord")]
    highest_net_record: Option<Opportunity>,
    #[serde(rename = "cumulativeDepthNet")]
    cumulative_depth_net: f64,
    #[serde(rename = "recentDepthScenarioKeys")]
    recent_depth_scenario_keys: Vec<String>,
    #[serde(rename = "minDepthNetThreshold", default)]
    min_depth_net_threshold: Option<f64>,
    #[serde(rename = "accountCapitalUsd", default)]
    account_capital_usd: f64,
}

#[derive(Clone)]
struct Level {
    price: f64,
    amount: f64,
}

#[derive(Clone)]
struct Book {
    name: String,
    fee_rate: f64,
    tier: VenueTier,
    asks: Vec<Level>,
    bids: Vec<Level>,
}

#[derive(Clone)]
struct AppState {
    snapshot: Arc<RwLock<Snapshot>>,
    runtime: Arc<RwLock<RuntimeState>>,
}

struct RuntimeState {
    recorded_opportunities: Vec<Opportunity>,
    max_spread_record: Option<Opportunity>,
    highest_net_record: Option<Opportunity>,
    cumulative_depth_net: f64,
    min_depth_net_threshold: f64,
    account_capital_usd: f64,
    recent_depth_keys: VecDeque<String>,
    recent_depth_key_set: HashSet<String>,
    paper_checks: Vec<PaperCheck>,
    pending_paper_keys: HashSet<String>,
    fake_trades: Vec<FakeTrade>,
    fake_cumulative_net: f64,
    fake_trades_reset_at: String,
    open_positions: Vec<OpenPosition>,
    open_position_keys: HashSet<String>,
    closed_trades: Vec<ClosedTrade>,
    realized_cumulative_net: f64,
    realized_trades_reset_at: String,
}

#[derive(Deserialize)]
struct SettingsPayload {
    #[serde(rename = "minDepthNetThreshold")]
    min_depth_net_threshold: Option<f64>,
    #[serde(rename = "accountCapitalUsd")]
    account_capital_usd: Option<f64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let poll_ms = env_u64("POLL_MS", 1500).max(POLL_MS_MIN);
    let timeout_ms = env_u64("REQUEST_TIMEOUT_MS", 2000).max(REQUEST_TIMEOUT_MS_MIN);
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env_u16("PORT", 3000);
    let exchanges = exchanges();
    let stored = load_stored_records().await.unwrap_or_default();
    let stored_account_capital_usd = stored_account_capital(&stored);
    let initial_threshold = initial_min_depth_net_threshold(&stored);

    let snapshot = Arc::new(RwLock::new(initial_snapshot(
        poll_ms,
        &exchanges,
        &stored,
    )));
    let runtime = Arc::new(RwLock::new(RuntimeState {
        recorded_opportunities: Vec::new(),
        max_spread_record: stored.max_spread_record,
        highest_net_record: stored.highest_net_record,
        cumulative_depth_net: stored.cumulative_depth_net,
        min_depth_net_threshold: initial_threshold,
        account_capital_usd: stored_account_capital_usd,
        recent_depth_key_set: stored.recent_depth_scenario_keys.iter().cloned().collect(),
        recent_depth_keys: stored.recent_depth_scenario_keys.into(),
        paper_checks: Vec::new(),
        pending_paper_keys: HashSet::new(),
        fake_trades: Vec::new(),
        fake_cumulative_net: 0.0,
        fake_trades_reset_at: now(),
        open_positions: Vec::new(),
        open_position_keys: HashSet::new(),
        closed_trades: Vec::new(),
        realized_cumulative_net: 0.0,
        realized_trades_reset_at: now(),
    }));
    let state = AppState {
        snapshot: snapshot.clone(),
        runtime: runtime.clone(),
    };

    tokio::spawn(poll_loop(
        snapshot,
        runtime,
        exchanges.clone(),
        poll_ms,
        timeout_ms,
    ));

    let app = Router::new()
        .route("/api/tickers", get(api_tickers))
        .route("/api/health", get(api_health))
        .route("/api/reset-depth-total", post(api_reset_depth_total))
        .route("/api/reset-fake-trades", post(api_reset_fake_trades))
        .route("/api/reset-realized-trades", post(api_reset_realized_trades))
        .route("/api/settings", post(api_settings))
        .fallback_service(ServeDir::new("public").append_index_html_on_directories(true))
        .with_state(state);

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let listener = TcpListener::bind(addr).await?;
    println!("BTC/USDT spread app running at http://{addr}");
    println!("Polling {} exchanges every {poll_ms}ms", exchanges.len());
    axum::serve(listener, app).await?;
    Ok(())
}

async fn api_tickers(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.snapshot.read().await.clone())
}

async fn api_health(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.snapshot.read().await;
    Json(serde_json::json!({
        "ok": true,
        "pollMs": snapshot.poll_ms,
        "exchanges": snapshot.prices.len()
    }))
}

async fn api_reset_depth_total(State(state): State<AppState>) -> impl IntoResponse {
    {
        let mut runtime = state.runtime.write().await;
        runtime.cumulative_depth_net = 0.0;
        runtime.recent_depth_keys.clear();
        runtime.recent_depth_key_set.clear();
        save_runtime_records(&runtime).await;
    }
    {
        let mut snapshot = state.snapshot.write().await;
        snapshot.cumulative_depth_net = 0.0;
    }
    Json(serde_json::json!({ "ok": true, "cumulativeDepthNet": 0.0 }))
}

async fn api_reset_fake_trades(State(state): State<AppState>) -> impl IntoResponse {
    let reset_at = now();
    {
        let mut runtime = state.runtime.write().await;
        runtime.fake_trades.clear();
        runtime.fake_cumulative_net = 0.0;
        runtime.fake_trades_reset_at = reset_at.clone();
    }
    {
        let mut snapshot = state.snapshot.write().await;
        snapshot.fake_trades.clear();
        snapshot.fake_cumulative_net = 0.0;
        snapshot.fake_trades_reset_at = reset_at.clone();
    }
    Json(serde_json::json!({ "ok": true, "fakeTradesResetAt": reset_at }))
}

async fn api_reset_realized_trades(State(state): State<AppState>) -> impl IntoResponse {
    let reset_at = now();
    {
        let mut runtime = state.runtime.write().await;
        runtime.open_positions.clear();
        runtime.open_position_keys.clear();
        runtime.closed_trades.clear();
        runtime.realized_cumulative_net = 0.0;
        runtime.realized_trades_reset_at = reset_at.clone();
    }
    {
        let mut snapshot = state.snapshot.write().await;
        snapshot.open_positions.clear();
        snapshot.closed_trades.clear();
        snapshot.realized_cumulative_net = 0.0;
        snapshot.realized_trades_reset_at = reset_at.clone();
    }
    Json(serde_json::json!({ "ok": true, "realizedTradesResetAt": reset_at }))
}

async fn api_settings(
    State(state): State<AppState>,
    Json(payload): Json<SettingsPayload>,
) -> impl IntoResponse {
    let current_runtime = state.runtime.read().await;
    let threshold = payload
        .min_depth_net_threshold
        .filter(|value| value.is_finite())
        .unwrap_or(current_runtime.min_depth_net_threshold)
        .max(0.0);
    let account_capital_usd = payload
        .account_capital_usd
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(current_runtime.account_capital_usd)
        .max(1.0);
    drop(current_runtime);
    {
        let mut runtime = state.runtime.write().await;
        runtime.min_depth_net_threshold = threshold;
        runtime.account_capital_usd = account_capital_usd;
        save_runtime_records(&runtime).await;
    }
    {
        let mut snapshot = state.snapshot.write().await;
        snapshot.min_depth_net_threshold = threshold;
        snapshot.account_sizing.capital_usd = account_capital_usd;
    }
    Json(serde_json::json!({
        "ok": true,
        "minDepthNetThreshold": threshold,
        "accountCapitalUsd": account_capital_usd
    }))
}

async fn poll_loop(
    snapshot: Arc<RwLock<Snapshot>>,
    runtime: Arc<RwLock<RuntimeState>>,
    exchanges: Vec<Exchange>,
    poll_ms: u64,
    timeout_ms: u64,
) {
    let client = Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .user_agent("btc-usdt-exchange-spread-rust/1.0")
        .build()
        .expect("reqwest client");

    loop {
        let previous = snapshot.read().await.prices.clone();
        let ticker_futures = exchanges
            .iter()
            .cloned()
            .map(|exchange| fetch_ticker(client.clone(), exchange));
        let depth_futures = exchanges
            .iter()
            .cloned()
            .map(|exchange| fetch_depth(client.clone(), exchange));

        let (ticker_results, depth_results) =
            tokio::join!(join_all(ticker_futures), join_all(depth_futures));

        let prices = merge_prices(ticker_results, previous);
        let books: Vec<Book> = depth_results.into_iter().flatten().collect();
        let opportunities = detect_opportunities(&prices);
        let visible_opportunity = opportunities.first().cloned();
        let (min_depth_net_threshold, account_capital_usd) = {
            let runtime = runtime.read().await;
            (
                runtime.min_depth_net_threshold,
                runtime.account_capital_usd,
            )
        };
        let account_sizing =
            account_sizing(&prices, visible_opportunity.as_ref(), account_capital_usd);
        let max_size_btc = account_sizing.max_size_btc;
        let visible_scenarios =
            find_executable_scenarios_for_opportunities(
                &books,
                &opportunities,
                max_size_btc,
                min_depth_net_threshold,
            );

        let current_price_spread = find_current_price_spread(&prices);
        let current_highest_net = find_highest_estimated_net(&prices);
        let max_profitable_size = visible_scenarios
            .first()
            .cloned()
            .unwrap_or_else(|| empty_max_profitable(max_size_btc));
        let runtime_handle = runtime.clone();

        let (
            recorded_opportunities,
            max_spread_record,
            highest_net_record,
            cumulative_depth_net,
            min_depth_net_threshold,
            paper_checks,
            fake_trades,
            fake_cumulative_net,
            fake_trades_reset_at,
            open_positions,
            closed_trades,
            realized_cumulative_net,
            realized_trades_reset_at,
        ) = {
            let mut runtime = runtime.write().await;
            if let Some(opportunity) = visible_opportunity.clone() {
                let depth_net = max_profitable_size.estimated_net_profit.unwrap_or(0.0);
                if max_profitable_size.size_btc > 0.0
                    && depth_net >= runtime.min_depth_net_threshold
                {
                    record_opportunity(
                        &mut runtime,
                        opportunity,
                        Some(max_profitable_size.size_btc),
                    );
                }
            }
            let min_executable_net = runtime.min_depth_net_threshold;
            maybe_schedule_paper_checks(
                &mut runtime,
                runtime_handle.clone(),
                client.clone(),
                exchanges.clone(),
                visible_opportunity.as_ref(),
                &max_profitable_size,
                min_executable_net,
            );
            if let Some(spread) = current_price_spread.clone() {
                if runtime
                    .max_spread_record
                    .as_ref()
                    .and_then(|r| r.gross_difference)
                    .map_or(true, |current| spread.gross_difference.unwrap_or(0.0) > current)
                {
                    runtime.max_spread_record = Some(spread);
                    save_runtime_records(&runtime).await;
                }
            }
            if let Some(net) = current_highest_net.clone() {
                if runtime
                    .highest_net_record
                    .as_ref()
                    .and_then(|r| r.estimated_net_profit)
                    .map_or(true, |current| net.estimated_net_profit.unwrap_or(f64::MIN) > current)
                {
                    runtime.highest_net_record = Some(net);
                    save_runtime_records(&runtime).await;
                }
            }

            let added = record_visible_depth_net(&mut runtime, &visible_scenarios);
            if added {
                save_runtime_records(&runtime).await;
            }
            update_open_positions(&mut runtime, &books);
            maybe_open_position(
                &mut runtime,
                &max_profitable_size,
                &books,
                min_executable_net,
                ACCOUNT_LEVERAGE,
            );

            (
                runtime.recorded_opportunities.clone(),
                runtime.max_spread_record.clone(),
                runtime.highest_net_record.clone(),
                runtime.cumulative_depth_net,
                runtime.min_depth_net_threshold,
                runtime.paper_checks.clone(),
                runtime.fake_trades.clone(),
                runtime.fake_cumulative_net,
                runtime.fake_trades_reset_at.clone(),
                runtime.open_positions.clone(),
                runtime.closed_trades.clone(),
                runtime.realized_cumulative_net,
                runtime.realized_trades_reset_at.clone(),
            )
        };

        let new_snapshot = Snapshot {
            pair: PAIR.to_string(),
            poll_ms,
            updated_at: Some(now()),
            prices,
            opportunities: visible_opportunity.into_iter().collect(),
            recorded_opportunities,
            max_spread_record,
            highest_net_record,
            max_profitable_size,
            max_profitable_scenarios: visible_scenarios,
            cumulative_depth_net,
            min_depth_net_threshold,
            account_sizing,
            paper_checks,
            fake_trades,
            fake_cumulative_net,
            fake_trades_reset_at,
            open_positions,
            closed_trades,
            realized_cumulative_net,
            realized_trades_reset_at,
        };
        *snapshot.write().await = new_snapshot;
        sleep(Duration::from_millis(poll_ms)).await;
    }
}

async fn fetch_ticker(client: Client, exchange: Exchange) -> PriceRow {
    let started = std::time::Instant::now();
    let result = async {
        let json: Value = client.get(exchange.ticker_url).send().await?.json().await?;
        Ok::<_, reqwest::Error>(parse_ticker(exchange.id, &json))
    }
    .await;

    match result {
        Ok((price, bid, ask)) if price.is_some() => PriceRow {
            id: exchange.id.to_string(),
            name: exchange.name.to_string(),
            price,
            bid,
            ask,
            fee_rate: exchange.fee_rate,
            tier: exchange.tier,
            ok: true,
            error: None,
            updated_at: Some(now()),
            latency_ms: Some(started.elapsed().as_millis()),
        },
        Ok(_) => failed_price(&exchange, started, "Price missing"),
        Err(error) => failed_price(&exchange, started, &error.to_string()),
    }
}

fn failed_price(exchange: &Exchange, started: std::time::Instant, error: &str) -> PriceRow {
    PriceRow {
        id: exchange.id.to_string(),
        name: exchange.name.to_string(),
        price: None,
        bid: None,
        ask: None,
        fee_rate: exchange.fee_rate,
        tier: exchange.tier,
        ok: false,
        error: Some(error.to_string()),
        updated_at: Some(now()),
        latency_ms: Some(started.elapsed().as_millis()),
    }
}

async fn fetch_depth(client: Client, exchange: Exchange) -> Option<Book> {
    let url = exchange.depth_url?;
    let response = match client.get(url).send().await {
        Ok(response) => response,
        Err(_) if exchange.id == "bitfinex" => {
            return fetch_bitfinex_ticker_book(client, exchange).await;
        }
        Err(_) => return None,
    };
    let json: Value = match response.json().await {
        Ok(json) => json,
        Err(_) if exchange.id == "bitfinex" => {
            return fetch_bitfinex_ticker_book(client, exchange).await;
        }
        Err(_) => return None,
    };
    let (asks, bids) = match parse_depth(exchange.id, &json) {
        Some(depth) => depth,
        None if exchange.id == "bitfinex" => {
            return fetch_bitfinex_ticker_book(client, exchange).await;
        }
        None => return None,
    };
    let mut asks = normalize_levels(exchange.id, &asks);
    let mut bids = normalize_levels(exchange.id, &bids);
    asks.sort_by(|a, b| a.price.total_cmp(&b.price));
    bids.sort_by(|a, b| b.price.total_cmp(&a.price));
    if asks.is_empty() || bids.is_empty() {
        return None;
    }
    Some(Book {
        name: exchange.name.to_string(),
        fee_rate: exchange.fee_rate,
        tier: exchange.tier,
        asks,
        bids,
    })
}

async fn fetch_bitfinex_ticker_book(client: Client, exchange: Exchange) -> Option<Book> {
    let json: Value = client
        .get(exchange.ticker_url)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let bid = num(json.get(0))?;
    let bid_size = num(json.get(1))?;
    let ask = num(json.get(2))?;
    let ask_size = num(json.get(3))?;

    Some(Book {
        name: exchange.name.to_string(),
        fee_rate: exchange.fee_rate,
        tier: exchange.tier,
        asks: vec![Level {
            price: ask,
            amount: ask_size,
        }],
        bids: vec![Level {
            price: bid,
            amount: bid_size,
        }],
    })
}

async fn fetch_books_for_route(
    client: Client,
    exchanges: &[Exchange],
    opportunity: &Opportunity,
) -> Option<(Book, Book)> {
    let buy_name = opportunity.buy_exchange.as_deref()?;
    let sell_name = opportunity.sell_exchange.as_deref()?;
    let buy_exchange = exchanges
        .iter()
        .find(|exchange| exchange.name == buy_name)?
        .clone();
    let sell_exchange = exchanges
        .iter()
        .find(|exchange| exchange.name == sell_name)?
        .clone();
    let (buy_book, sell_book) = tokio::join!(
        fetch_depth(client.clone(), buy_exchange),
        fetch_depth(client, sell_exchange)
    );
    Some((buy_book?, sell_book?))
}

fn merge_prices(results: Vec<PriceRow>, previous: Vec<PriceRow>) -> Vec<PriceRow> {
    results
        .into_iter()
        .map(|result| {
            if result.ok {
                return result;
            }
            if let Some(prev) = previous.iter().find(|p| p.id == result.id && p.ok) {
                let mut stale = prev.clone();
                stale.ok = false;
                stale.error = result.error;
                stale.latency_ms = result.latency_ms;
                return stale;
            }
            result
        })
        .collect()
}

fn parse_ticker(id: &str, json: &Value) -> (Option<f64>, Option<f64>, Option<f64>) {
    match id {
        "binance" => {
            let bid = num(json.get("bidPrice"));
            let ask = num(json.get("askPrice"));
            (midpoint(bid, ask), bid, ask)
        }
        "okx" => {
            let t = json.pointer("/data/0");
            (num_path(t, "last"), num_path(t, "bidPx"), num_path(t, "askPx"))
        }
        "bybit" => {
            let t = json.pointer("/result/list/0");
            (
                num_path(t, "lastPrice"),
                num_path(t, "bid1Price"),
                num_path(t, "ask1Price"),
            )
        }
        "kucoin" => {
            let d = json.get("data");
            (num_path(d, "price"), num_path(d, "bestBidPrice"), num_path(d, "bestAskPrice"))
        }
        "gateio" => {
            let t = json.get(0);
            (num_path(t, "last"), num_path(t, "highest_bid"), num_path(t, "lowest_ask"))
        }
        "bitget" => {
            let t = json.pointer("/data/0");
            (num_path(t, "lastPr"), num_path(t, "bidPr"), num_path(t, "askPr"))
        }
        "mexc" => {
            let d = json.get("data");
            (num_path(d, "lastPrice"), num_path(d, "bid1"), num_path(d, "ask1"))
        }
        "kraken" => {
            let t = json.pointer("/tickers/0");
            (num_path(t, "last"), num_path(t, "bid"), num_path(t, "ask"))
        }
        "bitfinex" => (num(json.get(6)), num(json.get(0)), num(json.get(2))),
        "cryptocom" => {
            let t = json.pointer("/result/data/0");
            (num_path(t, "a"), num_path(t, "b"), num_path(t, "k"))
        }
        "phemex" => {
            let book = json.pointer("/result/orderbook_p");
            let bid = book.and_then(|v| num(v.pointer("/bids/0/0")));
            let ask = book.and_then(|v| num(v.pointer("/asks/0/0")));
            (midpoint(bid, ask), bid, ask)
        }
        "whitebit" => {
            let bid = num(json.pointer("/bids/0/0"));
            let ask = num(json.pointer("/asks/0/0"));
            (midpoint(bid, ask), bid, ask)
        }
        "poloniex" => {
            let t = json.pointer("/data/0");
            (num_path(t, "c"), num_path(t, "bPx"), num_path(t, "aPx"))
        }
        "htx" => {
            let t = json.get("tick");
            (num_path(t, "close"), t.and_then(|v| num(v.pointer("/bid/0"))), t.and_then(|v| num(v.pointer("/ask/0"))))
        }
        "bingx" => {
            let t = json.get("data");
            let bid = num_path(t, "bidPrice");
            let ask = num_path(t, "askPrice");
            (num_path(t, "lastPrice").or(midpoint(bid, ask)), bid, ask)
        }
        _ => (None, None, None),
    }
}

fn parse_depth(id: &str, json: &Value) -> Option<(Vec<Value>, Vec<Value>)> {
    let pair = |asks: Option<&Value>, bids: Option<&Value>| {
        Some((asks?.as_array()?.clone(), bids?.as_array()?.clone()))
    };
    match id {
        "binance" => pair(json.get("asks"), json.get("bids")),
        "okx" => pair(json.pointer("/data/0/asks"), json.pointer("/data/0/bids")),
        "bybit" => pair(json.pointer("/result/a"), json.pointer("/result/b")),
        "kucoin" => pair(json.pointer("/data/asks"), json.pointer("/data/bids")),
        "gateio" => Some((
            gate_futures_levels(json.get("asks")?.as_array()?),
            gate_futures_levels(json.get("bids")?.as_array()?),
        )),
        "bitget" => pair(json.pointer("/data/asks"), json.pointer("/data/bids")),
        "mexc" => pair(json.pointer("/data/asks"), json.pointer("/data/bids")),
        "kraken" => pair(json.pointer("/orderBook/asks"), json.pointer("/orderBook/bids")),
        "bitfinex" => {
            let mut asks = Vec::new();
            let mut bids = Vec::new();
            for level in json.as_array()? {
                let amount = num(level.get(2))?;
                let price = level.get(0)?.clone();
                if amount > 0.0 {
                    bids.push(Value::Array(vec![price, Value::from(amount)]));
                } else {
                    asks.push(Value::Array(vec![price, Value::from(amount.abs())]));
                }
            }
            Some((asks, bids))
        }
        "cryptocom" => pair(json.pointer("/result/data/0/asks"), json.pointer("/result/data/0/bids")),
        "phemex" => pair(json.pointer("/result/orderbook_p/asks"), json.pointer("/result/orderbook_p/bids")),
        "whitebit" => pair(json.get("asks"), json.get("bids")),
        "poloniex" => pair(json.pointer("/data/asks"), json.pointer("/data/bids")),
        "htx" => pair(json.pointer("/tick/asks"), json.pointer("/tick/bids")),
        "bingx" => pair(json.pointer("/data/asks"), json.pointer("/data/bids")),
        _ => None,
    }
}

fn normalize_levels(id: &str, values: &[Value]) -> Vec<Level> {
    let multiplier = contract_amount_multiplier(id);
    values
        .iter()
        .filter_map(|level| {
            let arr = level.as_array()?;
            Some(Level {
                price: num(arr.first())?,
                amount: num(arr.get(1))? * multiplier,
            })
        })
        .filter(|level| level.price > 0.0 && level.amount > 0.0)
        .collect()
}

fn gate_futures_levels(values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .filter_map(|level| {
            Some(Value::Array(vec![
                level.get("p")?.clone(),
                Value::from(num(level.get("s")).unwrap_or(0.0).abs()),
            ]))
        })
        .collect()
}

fn contract_amount_multiplier(id: &str) -> f64 {
    match id {
        "okx" => 0.01,
        "kucoin" | "htx" | "poloniex" => 0.001,
        "gateio" | "mexc" => 0.0001,
        _ => 1.0,
    }
}

fn detect_opportunities(prices: &[PriceRow]) -> Vec<Opportunity> {
    let mut opportunities = Vec::new();
    for buy in prices {
        if !buy.ok {
            continue;
        }
        for sell in prices {
            if buy.id == sell.id || !sell.ok {
                continue;
            }
            let (Some(ask), Some(bid)) = (buy.ask, sell.bid) else {
                continue;
            };
            let gross = bid - ask;
            if gross <= 0.0 {
                continue;
            }
            let fees = ask * buy.fee_rate + bid * sell.fee_rate;
            let net = gross - fees;
            if net <= 0.0 {
                continue;
            }
            opportunities.push(opportunity(
                &format!("{}-{}-{}", buy.id, sell.id, now()),
                Some(&buy.name),
                Some(&sell.name),
                Some(ask),
                Some(bid),
                Some(gross),
                Some((gross / ask) * 100.0),
                Some(fees),
                Some(net),
                "Potential opportunity",
            ));
        }
    }
    opportunities.sort_by(|a, b| opt_f64_desc(a.estimated_net_profit, b.estimated_net_profit));
    opportunities
}

fn find_current_price_spread(prices: &[PriceRow]) -> Option<Opportunity> {
    let mut live: Vec<&PriceRow> = prices
        .iter()
        .filter(|p| p.ok && p.price.is_some())
        .collect();
    if live.len() < 2 {
        return None;
    }
    live.sort_by(|a, b| a.price.unwrap().total_cmp(&b.price.unwrap()));
    let low = live.first()?;
    let high = live.last()?;
    let buy = low.price?;
    let sell = high.price?;
    let gross = sell - buy;
    let fees = buy * low.fee_rate + sell * high.fee_rate;
    let net = gross - fees;
    Some(opportunity(
        &format!("price-spread-{}-{}-{}", low.id, high.id, now()),
        Some(&low.name),
        Some(&high.name),
        Some(buy),
        Some(sell),
        Some(gross),
        Some((gross / buy) * 100.0),
        Some(fees),
        Some(net),
        if net > 0.0 { "Potential opportunity" } else { "Not worth it" },
    ))
}

fn find_highest_estimated_net(prices: &[PriceRow]) -> Option<Opportunity> {
    let mut best = None;
    for buy in prices {
        if !buy.ok {
            continue;
        }
        for sell in prices {
            if buy.id == sell.id || !sell.ok {
                continue;
            }
            let (Some(ask), Some(bid)) = (buy.ask, sell.bid) else {
                continue;
            };
            let gross = bid - ask;
            let fees = ask * buy.fee_rate + bid * sell.fee_rate;
            let net = gross - fees;
            let candidate = opportunity(
                &format!("net-{}-{}-{}", buy.id, sell.id, now()),
                Some(&buy.name),
                Some(&sell.name),
                Some(ask),
                Some(bid),
                Some(gross),
                Some((gross / ask) * 100.0),
                Some(fees),
                Some(net),
                if net > 0.0 { "Potential opportunity" } else { "Not worth it" },
            );
            if best
                .as_ref()
                .and_then(|o: &Opportunity| o.estimated_net_profit)
                .map_or(true, |v| net > v)
            {
                best = Some(candidate);
            }
        }
    }
    best
}

fn weighted_fill(levels: &[Level], size_btc: f64) -> Option<(f64, f64)> {
    let mut remaining = size_btc;
    let mut notional = 0.0;
    for level in levels {
        if remaining <= 0.0 {
            break;
        }
        let filled = remaining.min(level.amount);
        notional += filled * level.price;
        remaining -= filled;
    }
    if remaining > 0.00000001 {
        None
    } else {
        Some((notional / size_btc, notional))
    }
}

fn simulate_profitable_size_for_pair(buy: &Book, sell: &Book, max_size: f64) -> Option<DepthSimulation> {
    let mut asks = buy.asks.clone();
    let mut bids = sell.bids.clone();
    asks.sort_by(|a, b| a.price.total_cmp(&b.price));
    bids.sort_by(|a, b| b.price.total_cmp(&a.price));

    let (mut ask_i, mut bid_i) = (0usize, 0usize);
    let (mut size, mut buy_notional, mut sell_notional) = (0.0, 0.0, 0.0);
    let mut best: Option<DepthSimulation> = None;

    while size < max_size && ask_i < asks.len() && bid_i < bids.len() {
        let ask = asks[ask_i].clone();
        let bid = bids[bid_i].clone();
        if bid.price * (1.0 - sell.fee_rate) <= ask.price * (1.0 + buy.fee_rate) {
            break;
        }
        let fill = ask.amount.min(bid.amount).min(max_size - size);
        size += fill;
        buy_notional += fill * ask.price;
        sell_notional += fill * bid.price;

        let buy_avg = buy_notional / size;
        let sell_avg = sell_notional / size;
        let gross_profit = sell_notional - buy_notional;
        let fees = buy_notional * buy.fee_rate + sell_notional * sell.fee_rate;
        let net = gross_profit - fees;
        let candidate = depth_sim(
            Some(max_size),
            size,
            Some(&buy.name),
            Some(&sell.name),
            Some(buy_avg),
            Some(sell_avg),
            Some(buy_notional),
            Some(sell_notional),
            Some(sell_avg - buy_avg),
            Some(gross_profit),
            Some(fees),
            Some(net),
            if net > 0.0 { "Potential opportunity" } else { "Not worth it" },
        );
        if best
            .as_ref()
            .and_then(|scenario| scenario.estimated_net_profit)
            .map_or(true, |current| net > current)
        {
            best = Some(candidate);
        }

        asks[ask_i].amount -= fill;
        bids[bid_i].amount -= fill;
        if asks[ask_i].amount <= 0.00000001 {
            ask_i += 1;
        }
        if bids[bid_i].amount <= 0.00000001 {
            bid_i += 1;
        }
    }
    best
}

fn books_for_opportunity<'a>(
    books: &'a [Book],
    opportunity: &Opportunity,
) -> Option<(&'a Book, &'a Book)> {
    let buy_name = opportunity.buy_exchange.as_deref()?;
    let sell_name = opportunity.sell_exchange.as_deref()?;
    let buy = books.iter().find(|book| book.name == buy_name)?;
    let sell = books.iter().find(|book| book.name == sell_name)?;
    Some((buy, sell))
}

fn find_best_depth_size_for_opportunity(
    books: &[Book],
    opportunity: &Opportunity,
    max_size: f64,
) -> Option<DepthSimulation> {
    let (buy, sell) = books_for_opportunity(books, opportunity)?;
    // Only USDT-margined linear-perp venues are ever an executable leg.
    // Other tiers (different settlement asset, unreliable internal book)
    // stay in the "Arbitrage Opportunities" feed for transparency but
    // never produce a depth scenario, so they can't trigger a position
    // open or contribute to cumulative depth net.
    if buy.tier != VenueTier::UsdtLinearPerp || sell.tier != VenueTier::UsdtLinearPerp {
        return None;
    }
    simulate_profitable_size_for_pair(buy, sell, max_size)
}

fn find_executable_scenarios_for_opportunities(
    books: &[Book],
    opportunities: &[Opportunity],
    max_size: f64,
    min_executable_net: f64,
) -> Vec<DepthSimulation> {
    let mut scenarios: Vec<DepthSimulation> = opportunities
        .iter()
        .filter_map(|opportunity| find_best_depth_size_for_opportunity(books, opportunity, max_size))
        .filter(|scenario| {
            scenario.size_btc > 0.0
                && scenario.estimated_net_profit.unwrap_or(0.0) >= min_executable_net
        })
        .collect();
    scenarios.sort_by(|a, b| opt_f64_desc(a.estimated_net_profit, b.estimated_net_profit));
    scenarios
}

fn simulate_exact_size_for_pair(buy: &Book, sell: &Book, size: f64) -> Option<DepthSimulation> {
    if size <= 0.0 {
        return None;
    }
    let (buy_avg, buy_notional) = weighted_fill(&buy.asks, size)?;
    let (sell_avg, sell_notional) = weighted_fill(&sell.bids, size)?;
    let gross_profit = sell_notional - buy_notional;
    let fees = buy_notional * buy.fee_rate + sell_notional * sell.fee_rate;
    let net = gross_profit - fees;

    Some(depth_sim(
        Some(size),
        size,
        Some(&buy.name),
        Some(&sell.name),
        Some(buy_avg),
        Some(sell_avg),
        Some(buy_notional),
        Some(sell_notional),
        Some(sell_avg - buy_avg),
        Some(gross_profit),
        Some(fees),
        Some(net),
        if net > 0.0 { "Potential opportunity" } else { "Not worth it" },
    ))
}

fn empty_max_profitable(max_size: f64) -> DepthSimulation {
    depth_sim(Some(max_size), 0.0, None, None, None, None, None, None, None, None, None, None, "No profitable depth")
}

fn maybe_schedule_paper_checks(
    runtime: &mut RuntimeState,
    runtime_handle: Arc<RwLock<RuntimeState>>,
    client: Client,
    exchanges: Vec<Exchange>,
    opportunity: Option<&Opportunity>,
    depth: &DepthSimulation,
    min_executable_net: f64,
) {
    let Some(opportunity) = opportunity else {
        return;
    };
    if depth.size_btc <= 0.0 || depth.estimated_net_profit.unwrap_or(0.0) < min_executable_net {
        return;
    }
    let Some(buy_exchange) = opportunity.buy_exchange.clone() else {
        return;
    };
    let Some(sell_exchange) = opportunity.sell_exchange.clone() else {
        return;
    };
    let base_key = format!(
        "{}>{}:{}:{}",
        buy_exchange,
        sell_exchange,
        (depth.size_btc * 100_000_000.0).round() / 100_000_000.0,
        (depth.estimated_net_profit.unwrap_or(0.0) * 100.0).round() / 100.0
    );
    if runtime.pending_paper_keys.contains(&base_key) {
        return;
    }
    runtime.pending_paper_keys.insert(base_key.clone());

    for delay_ms in PAPER_CHECK_DELAYS_MS {
        let state_key = base_key.clone();
        let runtime_handle = runtime_handle.clone();
        let client = client.clone();
        let exchanges = exchanges.clone();
        let opportunity = opportunity.clone();
        let buy_exchange = buy_exchange.clone();
        let sell_exchange = sell_exchange.clone();
        let initial_net = depth.estimated_net_profit.unwrap_or(0.0);
        let size_btc = depth.size_btc;
        tokio::spawn(async move {
            sleep(Duration::from_millis(delay_ms)).await;
            let paper_check = run_paper_check(
                client,
                &exchanges,
                &opportunity,
                buy_exchange,
                sell_exchange,
                size_btc,
                initial_net,
                min_executable_net,
                delay_ms,
            )
            .await;
            println!(
                "paper-check {} {}ms {}",
                state_key, delay_ms, paper_check.status
            );
            let mut runtime = runtime_handle.write().await;
            runtime.paper_checks.insert(0, paper_check);
            runtime.paper_checks.truncate(MAX_PAPER_CHECKS);
            if delay_ms == PAPER_CHECK_DELAYS_MS[PAPER_CHECK_DELAYS_MS.len() - 1] {
                runtime.pending_paper_keys.remove(&state_key);
            }
        });
    }
}

async fn run_paper_check(
    client: Client,
    exchanges: &[Exchange],
    opportunity: &Opportunity,
    buy_exchange: String,
    sell_exchange: String,
    size_btc: f64,
    initial_net: f64,
    min_executable_net: f64,
    delay_ms: u64,
) -> PaperCheck {
    let detected_at = opportunity.detected_at.clone();
    let result = fetch_books_for_route(client, exchanges, opportunity)
        .await
        .and_then(|(buy, sell)| simulate_exact_size_for_pair(&buy, &sell, size_btc));
    let recheck_net = result.as_ref().and_then(|scenario| scenario.estimated_net_profit);
    let net_change = recheck_net.map(|net| net - initial_net);
    let survived = recheck_net.map_or(false, |net| net >= min_executable_net);
    let status = if survived {
        "Survived"
    } else if recheck_net.is_some() {
        "Below threshold"
    } else {
        "No depth"
    };

    PaperCheck {
        id: format!(
            "{}-{}-{}-{}",
            buy_exchange.to_lowercase().replace(' ', "-"),
            sell_exchange.to_lowercase().replace(' ', "-"),
            delay_ms,
            now()
        ),
        buy_exchange,
        sell_exchange,
        size_btc,
        initial_net,
        min_executable_net,
        delay_ms,
        recheck_net,
        net_change,
        survived,
        status: status.to_string(),
        detected_at,
        checked_at: now(),
    }
}

fn record_opportunity(
    runtime: &mut RuntimeState,
    mut opportunity: Opportunity,
    size_btc: Option<f64>,
) {
    opportunity.size_btc = size_btc;
    let duplicate = runtime.recorded_opportunities.first().is_some_and(|last| {
        last.buy_exchange == opportunity.buy_exchange
            && last.sell_exchange == opportunity.sell_exchange
            && (last.estimated_net_profit.unwrap_or(0.0)
                - opportunity.estimated_net_profit.unwrap_or(0.0))
            .abs()
                < 0.01
    });
    if duplicate {
        return;
    }
    runtime.recorded_opportunities.insert(0, opportunity);
    runtime
        .recorded_opportunities
        .truncate(MAX_RECORDED_OPPORTUNITIES);
}

fn record_fake_trade(runtime: &mut RuntimeState, depth: &DepthSimulation, leverage: f64) {
    let (
        Some(buy_exchange),
        Some(sell_exchange),
        Some(buy_average_price),
        Some(sell_average_price),
        Some(buy_notional),
        Some(sell_notional),
        Some(estimated_fees),
        Some(estimated_net),
    ) = (
        depth.buy_exchange.clone(),
        depth.sell_exchange.clone(),
        depth.buy_average_price,
        depth.sell_average_price,
        depth.buy_notional,
        depth.sell_notional,
        depth.estimated_fees,
        depth.estimated_net_profit,
    ) else {
        return;
    };

    let margin_used = (buy_notional + sell_notional) / leverage.max(1.0);
    runtime.fake_trades.insert(
        0,
        FakeTrade {
            id: format!(
                "fake-{}-{}-{}",
                buy_exchange.to_lowercase().replace(' ', "-"),
                sell_exchange.to_lowercase().replace(' ', "-"),
                now()
            ),
            buy_exchange,
            sell_exchange,
            size_btc: depth.size_btc,
            buy_average_price,
            sell_average_price,
            buy_notional,
            sell_notional,
            estimated_fees,
            estimated_net,
            margin_used,
            status: "Simulated fill".to_string(),
            created_at: now(),
        },
    );
    runtime.fake_cumulative_net += estimated_net;
    runtime.fake_trades.truncate(MAX_FAKE_TRADES);
}

struct PositionEvaluation {
    exit_buy_price: f64,
    exit_sell_price: f64,
    exit_fees: f64,
    exit_spread: f64,
    trigger_spread: f64,
    realized_gross: f64,
    realized_net: f64,
}

fn evaluate_position(position: &OpenPosition, books: &[Book]) -> Option<PositionEvaluation> {
    let buy_book = books.iter().find(|b| b.name == position.buy_exchange)?;
    let sell_book = books.iter().find(|b| b.name == position.sell_exchange)?;

    let buy_ask = buy_book.asks.first()?.price;
    let sell_bid = sell_book.bids.first()?.price;
    let trigger_spread = sell_bid - buy_ask;

    let (exit_buy_price, exit_buy_notional) = weighted_fill(&buy_book.bids, position.size_btc)?;
    let (exit_sell_price, exit_sell_notional) = weighted_fill(&sell_book.asks, position.size_btc)?;
    let exit_spread = exit_sell_price - exit_buy_price;

    let realized_gross = position.size_btc
        * ((position.entry_sell_price - position.entry_buy_price) - exit_spread);
    let exit_fees = exit_buy_notional * position.buy_fee_rate
        + exit_sell_notional * position.sell_fee_rate;
    let realized_net = realized_gross - position.entry_fees - exit_fees;

    Some(PositionEvaluation {
        exit_buy_price,
        exit_sell_price,
        exit_fees,
        exit_spread,
        trigger_spread,
        realized_gross,
        realized_net,
    })
}

/// Direction-symmetric pair key.
///
/// Used to gate `RuntimeState::open_position_keys` so that we cannot
/// hold two opposite positions on the same venue pair at once. The
/// previous implementation used `format!("{buy}>{sell}")` which made
/// `A>B` and `B>A` distinct keys, allowing the bot to open a long-A
/// /short-B position alongside a long-B/short-A position on the next
/// tick when the basis flickered. Those two positions are by
/// construction the same exposure with opposite signs and just bleed
/// fees against each other.
fn pair_key(buy_exchange: &str, sell_exchange: &str) -> String {
    if buy_exchange <= sell_exchange {
        format!("{buy_exchange}<>{sell_exchange}")
    } else {
        format!("{sell_exchange}<>{buy_exchange}")
    }
}

fn maybe_open_position(
    runtime: &mut RuntimeState,
    depth: &DepthSimulation,
    books: &[Book],
    min_executable_net: f64,
    leverage: f64,
) {
    if depth.size_btc <= 0.0
        || depth.estimated_net_profit.unwrap_or(0.0) < min_executable_net
    {
        return;
    }
    let (Some(buy_name), Some(sell_name)) =
        (depth.buy_exchange.as_deref(), depth.sell_exchange.as_deref())
    else {
        return;
    };
    let key = pair_key(buy_name, sell_name);
    if runtime.open_position_keys.contains(&key) {
        return;
    }
    if runtime.open_positions.len() >= MAX_OPEN_POSITIONS {
        return;
    }
    let (Some(buy_book), Some(sell_book)) = (
        books.iter().find(|b| b.name == buy_name),
        books.iter().find(|b| b.name == sell_name),
    ) else {
        return;
    };
    let (
        Some(entry_buy_price),
        Some(entry_sell_price),
        Some(entry_fees),
        Some(entry_estimated_net),
    ) = (
        depth.buy_average_price,
        depth.sell_average_price,
        depth.estimated_fees,
        depth.estimated_net_profit,
    ) else {
        return;
    };
    let id = format!(
        "pos-{}-{}-{}",
        buy_name.to_lowercase().replace(' ', "-"),
        sell_name.to_lowercase().replace(' ', "-"),
        now()
    );
    runtime.open_positions.push(OpenPosition {
        id,
        buy_exchange: buy_name.to_string(),
        sell_exchange: sell_name.to_string(),
        size_btc: depth.size_btc,
        entry_buy_price,
        entry_sell_price,
        entry_fees,
        entry_estimated_net,
        buy_fee_rate: buy_book.fee_rate,
        sell_fee_rate: sell_book.fee_rate,
        ticks_held: 0,
        current_exit_spread: None,
        current_trigger_spread: None,
        current_unrealized_net: None,
        opened_at: now(),
    });
    runtime.open_position_keys.insert(key);
    record_fake_trade(runtime, depth, leverage);
}

fn build_closed_trade(
    position: &OpenPosition,
    eval: &PositionEvaluation,
    reason: &str,
) -> ClosedTrade {
    ClosedTrade {
        id: position.id.clone(),
        buy_exchange: position.buy_exchange.clone(),
        sell_exchange: position.sell_exchange.clone(),
        size_btc: position.size_btc,
        entry_buy_price: position.entry_buy_price,
        entry_sell_price: position.entry_sell_price,
        exit_buy_price: eval.exit_buy_price,
        exit_sell_price: eval.exit_sell_price,
        entry_fees: position.entry_fees,
        exit_fees: eval.exit_fees,
        entry_estimated_net: position.entry_estimated_net,
        realized_gross: eval.realized_gross,
        realized_net: eval.realized_net,
        pnl_vs_estimated: eval.realized_net - position.entry_estimated_net,
        ticks_held: position.ticks_held,
        closed_reason: reason.to_string(),
        opened_at: position.opened_at.clone(),
        closed_at: now(),
    }
}

fn update_open_positions(runtime: &mut RuntimeState, books: &[Book]) {
    let positions = std::mem::take(&mut runtime.open_positions);
    let mut still_open: Vec<OpenPosition> = Vec::with_capacity(positions.len());
    let mut newly_closed: Vec<ClosedTrade> = Vec::new();

    for mut position in positions {
        position.ticks_held += 1;
        let eval = evaluate_position(&position, books);

        match eval {
            Some(eval) => {
                position.current_exit_spread = Some(eval.exit_spread);
                position.current_trigger_spread = Some(eval.trigger_spread);
                position.current_unrealized_net = Some(eval.realized_net);

                let converged = eval.trigger_spread <= POSITION_CLOSE_TRIGGER;
                let timed_out = position.ticks_held >= POSITION_TIMEOUT_TICKS;

                if converged || timed_out {
                    let reason = if converged { "Converged" } else { "Timeout" };
                    let trade = build_closed_trade(&position, &eval, reason);
                    runtime.realized_cumulative_net += trade.realized_net;
                    runtime
                        .open_position_keys
                        .remove(&pair_key(&position.buy_exchange, &position.sell_exchange));
                    newly_closed.push(trade);
                } else {
                    still_open.push(position);
                }
            }
            None => {
                if position.ticks_held >= POSITION_TIMEOUT_TICKS {
                    runtime
                        .open_position_keys
                        .remove(&pair_key(&position.buy_exchange, &position.sell_exchange));
                    let stale = ClosedTrade {
                        id: position.id.clone(),
                        buy_exchange: position.buy_exchange.clone(),
                        sell_exchange: position.sell_exchange.clone(),
                        size_btc: position.size_btc,
                        entry_buy_price: position.entry_buy_price,
                        entry_sell_price: position.entry_sell_price,
                        exit_buy_price: position.entry_buy_price,
                        exit_sell_price: position.entry_sell_price,
                        entry_fees: position.entry_fees,
                        exit_fees: 0.0,
                        entry_estimated_net: position.entry_estimated_net,
                        realized_gross: 0.0,
                        realized_net: -position.entry_fees,
                        pnl_vs_estimated: -position.entry_fees - position.entry_estimated_net,
                        ticks_held: position.ticks_held,
                        closed_reason: "Stale".to_string(),
                        opened_at: position.opened_at.clone(),
                        closed_at: now(),
                    };
                    runtime.realized_cumulative_net += stale.realized_net;
                    newly_closed.push(stale);
                } else {
                    still_open.push(position);
                }
            }
        }
    }

    runtime.open_positions = still_open;
    for trade in newly_closed {
        runtime.closed_trades.insert(0, trade);
    }
    runtime.closed_trades.truncate(MAX_CLOSED_TRADES);
}

fn record_visible_depth_net(runtime: &mut RuntimeState, scenarios: &[DepthSimulation]) -> bool {
    let Some(best) = scenarios.iter().find(|scenario| {
        scenario.size_btc > 0.0
            && scenario.estimated_net_profit.unwrap_or(0.0) >= runtime.min_depth_net_threshold
    }) else {
        return false;
    };
    let net = best.estimated_net_profit.unwrap_or(0.0);
    if net <= 0.0 {
        return false;
    }
    let key = format!(
        "{}>{}:{}:{}",
        best.buy_exchange.as_deref().unwrap_or("-"),
        best.sell_exchange.as_deref().unwrap_or("-"),
        (best.size_btc * 1_000_000.0).round() / 1_000_000.0,
        (net * 100.0).round() / 100.0
    );
    if runtime.recent_depth_key_set.contains(&key) {
        return false;
    }
    runtime.cumulative_depth_net += net;
    runtime.recent_depth_keys.push_back(key.clone());
    runtime.recent_depth_key_set.insert(key);
    while runtime.recent_depth_keys.len() > 100 {
        if let Some(old) = runtime.recent_depth_keys.pop_front() {
            runtime.recent_depth_key_set.remove(&old);
        }
    }
    true
}

fn opportunity(
    id: &str,
    buy_exchange: Option<&str>,
    sell_exchange: Option<&str>,
    buy_price: Option<f64>,
    sell_price: Option<f64>,
    gross_difference: Option<f64>,
    gross_percentage_difference: Option<f64>,
    estimated_fees: Option<f64>,
    estimated_net_profit: Option<f64>,
    status: &str,
) -> Opportunity {
    Opportunity {
        id: id.to_string(),
        buy_exchange: buy_exchange.map(str::to_string),
        sell_exchange: sell_exchange.map(str::to_string),
        size_btc: None,
        buy_price,
        sell_price,
        gross_difference,
        gross_percentage_difference,
        estimated_fees,
        estimated_net_profit,
        status: status.to_string(),
        detected_at: now(),
    }
}

#[allow(clippy::too_many_arguments)]
fn depth_sim(
    max_size_btc: Option<f64>,
    size_btc: f64,
    buy_exchange: Option<&str>,
    sell_exchange: Option<&str>,
    buy_average_price: Option<f64>,
    sell_average_price: Option<f64>,
    buy_notional: Option<f64>,
    sell_notional: Option<f64>,
    gross_difference: Option<f64>,
    gross_profit: Option<f64>,
    estimated_fees: Option<f64>,
    estimated_net_profit: Option<f64>,
    status: &str,
) -> DepthSimulation {
    DepthSimulation {
        max_size_btc,
        size_btc,
        buy_exchange: buy_exchange.map(str::to_string),
        sell_exchange: sell_exchange.map(str::to_string),
        buy_average_price,
        sell_average_price,
        buy_notional,
        sell_notional,
        gross_difference,
        gross_profit,
        estimated_fees,
        estimated_net_profit,
        status: status.to_string(),
        simulated_at: now(),
    }
}

async fn load_stored_records() -> anyhow::Result<StoredRecords> {
    if !Path::new(RECORDS_PATH).exists() {
        return Ok(StoredRecords::default());
    }
    let data = fs::read_to_string(RECORDS_PATH).await?;
    Ok(serde_json::from_str(&data)?)
}

async fn save_runtime_records(runtime: &RuntimeState) {
    let records = StoredRecords {
        max_spread_record: runtime.max_spread_record.clone(),
        highest_net_record: runtime.highest_net_record.clone(),
        cumulative_depth_net: runtime.cumulative_depth_net,
        recent_depth_scenario_keys: runtime.recent_depth_keys.iter().cloned().collect(),
        min_depth_net_threshold: Some(runtime.min_depth_net_threshold),
        account_capital_usd: runtime.account_capital_usd,
    };
    if let Some(parent) = Path::new(RECORDS_PATH).parent() {
        let _ = fs::create_dir_all(parent).await;
    }
    if let Ok(data) = serde_json::to_string_pretty(&records) {
        let _ = fs::write(RECORDS_PATH, format!("{data}\n")).await;
    }
}

fn stored_account_capital(stored: &StoredRecords) -> f64 {
    if stored.account_capital_usd.is_finite() && stored.account_capital_usd > 0.0 {
        stored.account_capital_usd
    } else {
        DEFAULT_ACCOUNT_CAPITAL_USD
    }
}

fn initial_min_depth_net_threshold(stored: &StoredRecords) -> f64 {
    stored
        .min_depth_net_threshold
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(DEFAULT_MIN_EXECUTABLE_NET)
}

fn initial_snapshot(poll_ms: u64, exchanges: &[Exchange], stored: &StoredRecords) -> Snapshot {
    let account_capital_usd = stored_account_capital(stored);
    let account_sizing = AccountSizing {
        capital_usd: account_capital_usd,
        leverage: ACCOUNT_LEVERAGE,
        max_notional_per_leg: account_capital_usd * ACCOUNT_LEVERAGE / 2.0,
        max_size_btc: 0.0,
    };
    Snapshot {
        pair: PAIR.to_string(),
        poll_ms,
        updated_at: None,
        prices: exchanges
            .iter()
            .map(|e| PriceRow {
                id: e.id.to_string(),
                name: e.name.to_string(),
                price: None,
                bid: None,
                ask: None,
                fee_rate: e.fee_rate,
                tier: e.tier,
                ok: false,
                error: Some("Waiting for first poll".to_string()),
                updated_at: None,
                latency_ms: None,
            })
            .collect(),
        opportunities: Vec::new(),
        recorded_opportunities: Vec::new(),
        max_spread_record: stored.max_spread_record.clone(),
        highest_net_record: stored.highest_net_record.clone(),
        max_profitable_size: empty_max_profitable(account_sizing.max_size_btc),
        max_profitable_scenarios: Vec::new(),
        cumulative_depth_net: stored.cumulative_depth_net,
        min_depth_net_threshold: initial_min_depth_net_threshold(stored),
        account_sizing,
        paper_checks: Vec::new(),
        fake_trades: Vec::new(),
        fake_cumulative_net: 0.0,
        fake_trades_reset_at: now(),
        open_positions: Vec::new(),
        closed_trades: Vec::new(),
        realized_cumulative_net: 0.0,
        realized_trades_reset_at: now(),
    }
}

fn account_sizing(
    prices: &[PriceRow],
    opportunity: Option<&Opportunity>,
    capital_usd: f64,
) -> AccountSizing {
    let max_notional_per_leg = capital_usd * ACCOUNT_LEVERAGE / 2.0;
    let reference_price = opportunity
        .and_then(|opportunity| opportunity.buy_price)
        .or_else(|| {
            prices
                .iter()
                .filter(|price| price.ok)
                .filter_map(|price| price.ask.or(price.price))
                .filter(|price| price.is_finite() && *price > 0.0)
                .min_by(|a, b| a.total_cmp(b))
        });
    let max_size_btc = reference_price
        .map(|price| (max_notional_per_leg / price).min(ABSOLUTE_MAX_SIZE_BTC))
        .unwrap_or(0.0);

    AccountSizing {
        capital_usd,
        leverage: ACCOUNT_LEVERAGE,
        max_notional_per_leg,
        max_size_btc,
    }
}

fn exchanges() -> Vec<Exchange> {
    use VenueTier::*;
    vec![
        // ---- Tier: USDT-margined linear perpetuals (executable arb candidates) ----
        ex("binance", "Binance Futures", 0.0005, UsdtLinearPerp, "https://fapi.binance.com/fapi/v1/ticker/bookTicker?symbol=BTCUSDT", Some("https://fapi.binance.com/fapi/v1/depth?symbol=BTCUSDT&limit=100")),
        ex("okx", "OKX Swap", 0.0005, UsdtLinearPerp, "https://www.okx.com/api/v5/market/ticker?instId=BTC-USDT-SWAP", Some("https://www.okx.com/api/v5/market/books?instId=BTC-USDT-SWAP&sz=100")),
        ex("bybit", "Bybit Linear", 0.00055, UsdtLinearPerp, "https://api.bybit.com/v5/market/tickers?category=linear&symbol=BTCUSDT", Some("https://api.bybit.com/v5/market/orderbook?category=linear&symbol=BTCUSDT&limit=100")),
        ex("gateio", "Gate.io Futures", 0.0005, UsdtLinearPerp, "https://api.gateio.ws/api/v4/futures/usdt/tickers?contract=BTC_USDT", Some("https://api.gateio.ws/api/v4/futures/usdt/order_book?contract=BTC_USDT&limit=100")),
        ex("bitget", "Bitget Futures", 0.0006, UsdtLinearPerp, "https://api.bitget.com/api/v2/mix/market/ticker?productType=USDT-FUTURES&symbol=BTCUSDT", Some("https://api.bitget.com/api/v2/mix/market/orderbook?symbol=BTCUSDT&productType=USDT-FUTURES&limit=100")),
        // MEXC: API taker fee is 0.05% even though web/app shows 0.01% for BTC futures takers.
        ex("mexc", "MEXC Futures", 0.0005, UsdtLinearPerp, "https://contract.mexc.com/api/v1/contract/ticker?symbol=BTC_USDT", Some("https://contract.mexc.com/api/v1/contract/depth/BTC_USDT?limit=100")),
        ex("phemex", "Phemex Perp", 0.0006, UsdtLinearPerp, "https://api.phemex.com/md/v2/orderbook?symbol=BTCUSDT", Some("https://api.phemex.com/md/v2/orderbook?symbol=BTCUSDT")),
        ex("poloniex", "Poloniex Futures", 0.0006, UsdtLinearPerp, "https://api.poloniex.com/v3/market/tickers?symbol=BTC_USDT_PERP", Some("https://api.poloniex.com/v3/market/orderBook?symbol=BTC_USDT_PERP&limit=50")),
        ex("htx", "HTX Swap", 0.0005, UsdtLinearPerp, "https://api.hbdm.com/linear-swap-ex/market/detail/merged?contract_code=BTC-USDT", Some("https://api.hbdm.com/linear-swap-ex/market/depth?contract_code=BTC-USDT&type=step0")),
        ex("bingx", "BingX Swap", 0.0005, UsdtLinearPerp, "https://open-api.bingx.com/openApi/swap/v2/quote/ticker?symbol=BTC-USDT", Some("https://open-api.bingx.com/openApi/swap/v2/quote/depth?symbol=BTC-USDT&limit=100")),

        // ---- Tier: different settlement asset (price-feed only) ----
        // Bitfinex BTCF0:USTF0 settles in USTF0 (Bitfinex's tether-like internal token).
        // The pair carries a persistent ~$80 basis vs USDT-perps that does not mean-revert.
        ex("bitfinex", "Bitfinex Derivatives", 0.0, DifferentSettlement, "https://api-pub.bitfinex.com/v2/ticker/tBTCF0:USTF0", Some("https://api-pub.bitfinex.com/v2/book/tBTCF0:USTF0/P0?len=25")),
        // Kraken PF_XBTUSD is USD-margined, not USDT-margined.
        ex("kraken", "Kraken Futures", 0.0005, DifferentSettlement, "https://futures.kraken.com/derivatives/api/v3/tickers?symbol=PF_XBTUSD", Some("https://futures.kraken.com/derivatives/api/v3/orderbook?symbol=PF_XBTUSD")),
        // Crypto.com BTCUSD-PERP is USD-quoted on their own settlement.
        ex("cryptocom", "Crypto.com Derivatives", 0.0005, DifferentSettlement, "https://api.crypto.com/exchange/v1/public/get-tickers?instrument_name=BTCUSD-PERP", Some("https://api.crypto.com/exchange/v1/public/get-book?instrument_name=BTCUSD-PERP&depth=50")),

        // ---- Tier: unreliable book (price-feed only) ----
        // KuCoin XBTUSDTM has shown a persistent ~$25 mark premium vs the USDT cluster
        // that we have no model for; treat as reference until that's understood.
        ex("kucoin", "KuCoin Futures", 0.0006, UnreliableBook, "https://api-futures.kucoin.com/api/v1/ticker?symbol=XBTUSDTM", Some("https://api-futures.kucoin.com/api/v1/level2/depth100?symbol=XBTUSDTM")),
        // WhiteBIT BTC_PERP routinely shows a 30+ USD intra-book bid-ask spread,
        // wider than any cross-venue edge we'd ever capture.
        ex("whitebit", "WhiteBIT Futures", 0.00035, UnreliableBook, "https://whitebit.com/api/v4/public/orderbook/BTC_PERP?limit=50", Some("https://whitebit.com/api/v4/public/orderbook/BTC_PERP?limit=50")),
    ]
}

fn ex(
    id: &'static str,
    name: &'static str,
    fee_rate: f64,
    tier: VenueTier,
    ticker_url: &'static str,
    depth_url: Option<&'static str>,
) -> Exchange {
    Exchange {
        id,
        name,
        fee_rate,
        ticker_url,
        depth_url,
        tier,
    }
}

fn num(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    match value {
        Value::Number(n) => n.as_f64().filter(|v| *v > 0.0),
        Value::String(s) => s.parse::<f64>().ok().filter(|v| *v > 0.0),
        _ => None,
    }
}

fn num_path(value: Option<&Value>, key: &str) -> Option<f64> {
    num(value?.get(key))
}

fn midpoint(bid: Option<f64>, ask: Option<f64>) -> Option<f64> {
    Some((bid? + ask?) / 2.0)
}

fn opt_f64_desc(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    b.unwrap_or(f64::MIN).total_cmp(&a.unwrap_or(f64::MIN))
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u16(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
