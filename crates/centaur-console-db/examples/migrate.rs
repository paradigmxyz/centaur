//! Apply Console's migrations to one database, for tests and local setup:
//!
//! ```text
//! cargo run -p centaur-console-db --example migrate -- postgresql://...
//! ```
//!
//! Deployments migrate through api-rs, which applies them on every start.

use sqlx::postgres::PgPoolOptions;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .ok_or("usage: migrate <console-database-url>")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    centaur_console_db::migrate(&pool).await?;
    println!("Console database migrations applied");
    Ok(())
}
