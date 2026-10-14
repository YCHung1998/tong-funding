# Bybit ticker WebSocket feed — troubleshooting and roll-back

## What it is

```
Bybit REST tickers (every 10 s) ──► bybit_rest map ─┐   base: listing, rules, interval, volume
                                                     ├─► overlay_bybit (every 1 s) ─► MarketPartial ─► scanner / sim prices
wss://stream.bybit.com/v5/public/linear ─► TickerCache ┘   newer rate, mark price, next settlement only
```

- The REST poll is unchanged and is always the base and the fallback.
- The WebSocket (`tickers.<SYMBOL>`, 801+ USDT perpetuals, 100 topics per subscribe request) only
  overwrites a REST observation when its message time is newer.
- The pre-trade REST re-fetch, orders and accounts never use this feed or its cache.
- First message of a topic is a `snapshot`, later ones are `delta`s with only the changed fields; the
  cache merges them. After a reconnect the cache is cleared and everything is subscribed again.

## Where to look

| What | Name |
|---|---|
| Health source (system log, `FETCH_ERROR` / `FEED_RECOVERED` events) | `bybit_ws_ticker` |
| Scanner header chip | `Bybit WebSocket` (ONLINE / RECONNECTING / OFFLINE) |
| Notice (badge drawer, not the banner) | `Bybit WebSocket 已斷線，行情退回 REST 輪詢（約每 10 秒更新）` |
| Event `error_kind` | `Stale` (no message for the stale threshold), `Disconnected`, `Network`/`Timeout` (connect), `Exchange` (a subscribe was rejected) |

The notice means: the WebSocket is not delivering (offline, silent, or never connected). Bybit data is
then as fresh as the REST poll (about 10 s). Nothing is lost; the scanner keeps working.

Liveness is connection-level: the stream carries ~800 messages per second, so no message at all for
`max(stale_data_threshold_ms, 3 s)` means a dead connection and triggers a reconnect (back-off 1 s →
30 s). A single quiet symbol is *not* stale (silence means unchanged).

## Roll back

```
TONG_FUNDING_BYBIT_FEED=rest   # then restart the app
```

Only the word `rest` (any case) turns the WebSocket off; anything else, or unset, is on. When off, no
connection is opened, no notice is raised, and the scanner header shows
`已停用（TONG_FUNDING_BYBIT_FEED=rest）`.

## Known limits

- Rollover: after a settlement a delta normally carries the new `nextFundingTime` (observed on the real
  stream for an hourly symbol). If a symbol's settlement time stays more than 15 s in the past, the loop
  unsubscribes and subscribes that topic again (at most 100 symbols per 5 s pass, each retried no
  sooner than 30 s) to get a fresh snapshot.
- New listings: symbols that appear in the REST listing are subscribed on the next 5 s pass.
- Dated futures (`…-09OCT26`) are not subscribed.

## Checking it against the real exchange

```
cargo test -p tong-funding live_bybit_ws_vs_rest -- --ignored --nocapture
```

Public, read-only, 60 s. Prints how many symbols have a complete WebSocket entry, how many mark prices
differ from a REST snapshot by more than 0.5 %, funding-rate and next-settlement differences, and the
final health.
