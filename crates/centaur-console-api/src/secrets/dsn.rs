//! The database a Postgres connection string names, for checking an inline
//! DSN against the secret's `database`. Understands the two libpq forms:
//! `postgres://user:pass@host:port/dbname?...` and `host=... dbname=...`.
//!
//! `sqlx`'s `PgConnectOptions` is not used: parsing starts from the process
//! environment (`PGDATABASE`, ...) and `~/.pgpass`, neither of which belongs
//! in a validation of pasted input.

use percent_encoding::percent_decode_str;

/// The database the DSN names: `None` when the string is not a connection
/// string, `Some(None)` when it names no database.
pub fn dbname(dsn: &str) -> Option<Option<String>> {
    let dsn = dsn.trim();
    if dsn.starts_with("postgres://") || dsn.starts_with("postgresql://") {
        from_url(dsn)
    } else {
        from_keywords(dsn)
    }
}

fn from_url(dsn: &str) -> Option<Option<String>> {
    let url = url::Url::parse(dsn).ok()?;
    // libpq lets a `dbname` query parameter stand in for the path.
    let from_query = url
        .query_pairs()
        .find(|(key, _)| key == "dbname")
        .map(|(_, value)| value.into_owned());
    let path = url.path().trim_start_matches('/');
    let from_path =
        (!path.is_empty()).then(|| percent_decode_str(path).decode_utf8_lossy().into_owned());
    Some(from_path.or(from_query).filter(|name| !name.is_empty()))
}

/// `key = value` pairs separated by whitespace; values may be single
/// quoted, with `\'` and `\\` escapes.
fn from_keywords(dsn: &str) -> Option<Option<String>> {
    let mut chars = dsn.chars().peekable();
    let mut dbname = None;
    let mut any = false;
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| *c != '=' && !c.is_whitespace()) {
            key.push(c);
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        chars.next_if_eq(&'=')?;
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut value = String::new();
        if chars.next_if_eq(&'\'').is_some() {
            loop {
                match chars.next()? {
                    '\'' => break,
                    '\\' => value.push(chars.next()?),
                    c => value.push(c),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                if c == '\\' {
                    value.push(chars.next()?);
                } else {
                    value.push(c);
                }
            }
        }
        if key.is_empty() {
            return None;
        }
        any = true;
        if key == "dbname" {
            dbname = Some(value).filter(|name| !name.is_empty());
        }
    }
    any.then_some(dbname)
}

#[cfg(test)]
mod tests {
    use super::dbname;

    #[test]
    fn urls() {
        let name = |dsn| dbname(dsn).unwrap();
        assert_eq!(
            name("postgres://u:p@db.example:5432/shop"),
            Some("shop".into())
        );
        assert_eq!(
            name("postgresql://u@h/my%20db?sslmode=require"),
            Some("my db".into())
        );
        assert_eq!(name("postgres://u@h/?dbname=shop"), Some("shop".into()));
        assert_eq!(name("postgres://u@h"), None);
        assert_eq!(name("postgres://u@h/"), None);
    }

    #[test]
    fn keywords() {
        let name = |dsn| dbname(dsn).unwrap();
        assert_eq!(name("host=h user=u dbname=shop"), Some("shop".into()));
        assert_eq!(name("dbname = 'my db' host=h"), Some("my db".into()));
        assert_eq!(name(r"dbname='it\'s'"), Some("it's".into()));
        assert_eq!(name("host=h"), None);
    }

    #[test]
    fn junk_is_not_a_connection_string() {
        for junk in ["", "not a dsn", "host", "host='unterminated"] {
            assert_eq!(dbname(junk), None, "{junk:?}");
        }
    }
}
