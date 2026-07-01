# Test Inputs by Tool Category

Default test inputs to use only when triaging a failed tool `health` check or when the user explicitly requests deeper per-method coverage. Do not use these as the first-pass smoke test. First run:

```bash
<tool> health
```

Use the inputs below only after recording the health result and only for safe read-only follow-up checks.

## Database / Structured-Data Tools

Tools: database or warehouse-backed tools

| Method pattern | Test input |
|---------------|------------|
| `*_tables`, `*_list` | `{}` or `{"limit": 3}` |
| `*_describe` | `{"table_name": "Fund"}` |
| `*_query` | `{"query": "SELECT 1", "limit": 1}` |
| `*_by_symbol` | `{"symbol": "ETH"}` or `{"symbol": "BTC"}` |
| `*_search` | `{"query": "bitcoin", "limit": 3}` |
| `*_organizations` | `{"search": "bitcoin", "limit": 3}` |
| `*_people` | `{"search": "fred", "limit": 3}` |
| `*_positions` | `{"limit": 3}` |
| `*_balances` | `{"limit": 3}` |

## Blockchain / DeFi Tools

Tools: `debank`, `defillama`, `dune`, `nansen`, `arkham`, `snapshot`

| Method pattern | Test input |
|---------------|------------|
| Wallet lookups | `{"address": "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"}` (vitalik.eth) |
| Protocol queries | `{"protocol": "aave"}` or `{"protocol": "uniswap"}` |
| Chain queries | `{"chain": "ethereum"}` |
| Token lookups | `{"symbol": "ETH"}` or `{"contract": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"}` (WETH) |

## Market Data Tools

Tools: `coinmetrics`, `tardis`, `bloomberg`

| Method pattern | Test input |
|---------------|------------|
| Price queries | `{"asset": "btc"}` or `{"symbol": "BTC"}` |
| Time ranges | `{"start_date": "2026-03-01", "end_date": "2026-03-03"}` |
| Metrics | `{"metric": "PriceUSD", "asset": "btc"}` |

## News / Search Tools

Tools: `websearch`, `googlenews`, `newsapi`, `coindesk`, `theblock`

| Method pattern | Test input |
|---------------|------------|
| Search | `{"query": "bitcoin", "limit": 3}` |
| Headlines | `{"limit": 3}` |

## Social Tools

Tools: `twitter`, `slack`, `social-monitor`

| Method pattern | Test input |
|---------------|------------|
| User lookup | `{"username": "VitalikButerin"}` |
| Search | `{"query": "ethereum", "limit": 3}` |
| Channel messages | `{"channel": "general", "limit": 3}` |

## Custody / Exchange Tools

Tools: `coinbase`, `anchorage`, `bitgo`, `unit410`, `falconx`

| Method pattern | Test input |
|---------------|------------|
| List wallets | `{"limit": 3}` |
| Balances | `{"limit": 3}` |

## Analytics / BI Tools

Tools: `similarweb`, `sensortower`, `harmonic`, `standard-metrics`, `posthog`

| Method pattern | Test input |
|---------------|------------|
| Company lookup | `{"domain": "example.com"}` |
| Search | `{"query": "bitcoin", "limit": 3}` |

## Government / Regulatory Tools

Tools: `congress`, `fedreg`, `legistorm`, `openfec`

| Method pattern | Test input |
|---------------|------------|
| Search | `{"query": "cryptocurrency", "limit": 3}` |
| Bills | `{"limit": 3}` |

## Infrastructure / Monitoring Tools

Tools: `grafana`, `loki`, `reth`, `confmonitor`

| Method pattern | Test input |
|---------------|------------|
| Health check | `{}` |
| Query | `{"query": "up", "limit": 3}` |
| Logs | `{"query": "{job=\"api\"}", "limit": 3}` |

## Google / Productivity Tools

Tools: `gsuite`

| Method pattern | Test input |
|---------------|------------|
| Calendar | `{"limit": 3}` |
| Contacts | `{"query": "alex", "limit": 3}` |
| Gmail | `{"query": "from:me", "limit": 3}` |

## General Rules

1. **Run `<tool> health` first** and keep the health output as the primary smoke result.
2. **Always use `limit: 2` or `3`** to keep responses small.
3. **Use well-known addresses** for blockchain lookups (vitalik.eth, WETH contract).
4. **Use broad search terms** that are likely to return results ("bitcoin", "ethereum", "open source").
5. **Never use real credentials** as test inputs.
6. **Prefer read-only methods**. Skip anything that creates, updates, or deletes.
