//! The brain's own schema migrations, separate from api-rs's.
//!
//! They create the `nanocode` schema and the least-privileged `centaur_brain`
//! role the service runs as; nanocode creates its tables inside that schema
//! at runtime. They run with the database owner's credentials, which the
//! long-running service never receives. When given the brain's own DSN, this
//! also sets the role's login password from it, so that DSN is the only
//! credential an operator provisions.

use sqlx::{Connection, Executor, PgConnection, migrate::Migrator};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Holds the migrations' tracking table. sqlx names that table without a
/// schema, so this keeps it apart from api-rs's in `public`, and outside
/// `nanocode`, where the brain role could change it.
const TRACKING_SCHEMA: &str = "centaur_brain_migrations";
const ROLE: &str = "centaur_brain";

type Error = Box<dyn std::error::Error + Send + Sync>;

pub async fn run(database_url: &str, brain_database_url: Option<&str>) -> Result<(), Error> {
    let password = brain_database_url.map(brain_password).transpose()?;
    let mut connection = PgConnection::connect(database_url).await?;
    connection
        .execute(format!("create schema if not exists {TRACKING_SCHEMA}").as_str())
        .await?;
    connection
        .execute(format!("set search_path to {TRACKING_SCHEMA}").as_str())
        .await?;
    MIGRATOR.run(&mut connection).await?;
    if let Some(password) = password {
        // ALTER ROLE takes no bind parameters, so let Postgres quote it.
        let (statement,): (String,) = sqlx::query_as(&format!(
            "select format('alter role {ROLE} login password %L', $1)"
        ))
        .bind(password)
        .fetch_one(&mut connection)
        .await?;
        connection.execute(statement.as_str()).await?;
    }
    connection.close().await?;
    Ok(())
}

/// The password of the brain's DSN, which must connect as the brain role.
fn brain_password(url: &str) -> Result<String, Error> {
    let url = url::Url::parse(url)?;
    let user = urlencoding::decode(url.username())?;
    if user != ROLE {
        return Err(
            format!("the brain's database URL must connect as {ROLE}, not {user:?}").into(),
        );
    }
    let password = url
        .password()
        .ok_or("the brain's database URL has no password")?;
    Ok(urlencoding::decode(password)?.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brain_dsn_must_name_the_brain_role_and_a_password() {
        assert_eq!(
            brain_password("postgres://centaur_brain:p%40ss%27w@db:5432/app").unwrap(),
            "p@ss'w"
        );
        assert!(brain_password("postgres://postgres:secret@db/app").is_err());
        assert!(brain_password("postgres://centaur_brain@db/app").is_err());
    }
}
