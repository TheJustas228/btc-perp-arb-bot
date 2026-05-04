# BTC/USDT Perpetual Derivatives Spread

Live local Rust web app that polls free public BTC/USDT perpetual futures/swap endpoints and shows the same price matrix, arbitrage, depth simulation, and recorded opportunity UI as the spot app.

## Run

```bash
npm start
```

Open `http://localhost:3000`.

## Refresh speed

The server polls all derivative venues concurrently every `1500ms` by default and allows each venue up to `2000ms` to respond. You can tune it:

```bash
POLL_MS=1000 npm start
```

The app enforces a `750ms` minimum to avoid hammering free public APIs.

## Venues

Binance Futures, OKX Swap, Bybit Linear, KuCoin Futures, Gate.io Futures, Bitget Futures, MEXC Futures, Kraken Futures, Bitfinex Derivatives, Crypto.com Derivatives, Phemex Perp, WhiteBIT Futures, Poloniex Futures, HTX Swap, and BingX Swap.

Depth quantities are normalized to BTC where the venue returns contract counts.
