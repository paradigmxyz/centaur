# Absurd SDK for Rust

Rust SDK for [Absurd](https://github.com/earendil-works/absurd), a PostgreSQL-
based durable task execution system.

This SDK is async-only and uses [`sqlx`](https://github.com/launchbadge/sqlx)
with `PgPool`.

## Installation

```toml
[dependencies]
absurd-sdk = "0.4.0"
```

## Quick Start

Before using the SDK, initialize Absurd in your PostgreSQL database:

```bash
uvx absurdctl init -d your-database-name
uvx absurdctl create-queue -d your-database-name default
```

Then register tasks and run a worker:

```rust
use absurd::{Client, ClientOptions, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct OrderParams {
    order_id: String,
}

#[derive(Debug, Serialize)]
struct OrderResult {
    order_id: String,
    charged: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let app = Client::connect(ClientOptions::default()).await?;
    app.create_queue(None, Default::default()).await?;

    app.register_task("order-fulfillment", |params: OrderParams, ctx| async move {
        let charged: bool = ctx.step("charge-card", || async { Ok(true) }).await?;
        Ok(OrderResult {
            order_id: params.order_id,
            charged,
        })
    })?;

    app.run_worker(Default::default()).await
}
```

If `database_url` is omitted, the client uses `ABSURD_DATABASE_URL`, then
`PGDATABASE`, then `postgresql://localhost/absurd`.
