//! The eight grantable secret kinds and what distinguishes them: the table,
//! oid prefix, the column that names the kind in `secret_sources`,
//! `request_rules` and `grants`, and the columns a document writes.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Static,
    GcpAuth,
    GcpIdToken,
    AwsAuth,
    OauthToken,
    PgDsn,
    Hmac,
    SshKey,
}

/// Every kind, in the order the console lists them.
pub const ALL: [Kind; 8] = [
    Kind::Static,
    Kind::GcpAuth,
    Kind::GcpIdToken,
    Kind::AwsAuth,
    Kind::OauthToken,
    Kind::PgDsn,
    Kind::Hmac,
    Kind::SshKey,
];

impl Kind {
    /// The slug the console API uses (`/console/secrets/{slug}/{id}`).
    pub fn slug(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::GcpAuth => "gcp_auth",
            Self::GcpIdToken => "gcp_id_token",
            Self::AwsAuth => "aws_auth",
            Self::OauthToken => "oauth_token",
            Self::PgDsn => "pg_dsn",
            Self::Hmac => "hmac",
            Self::SshKey => "ssh_key",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        ALL.into_iter().find(|kind| kind.slug() == slug)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Static => "Static",
            Self::GcpAuth => "GCP Auth",
            Self::GcpIdToken => "GCP ID Token",
            Self::AwsAuth => "AWS Auth",
            Self::OauthToken => "OAuth Token",
            Self::PgDsn => "Postgres DSN",
            Self::Hmac => "HMAC",
            Self::SshKey => "SSH key",
        }
    }

    pub fn table(self) -> &'static str {
        match self {
            Self::Static => "static_secrets",
            Self::GcpAuth => "gcp_auth_secrets",
            Self::GcpIdToken => "gcp_id_token_secrets",
            Self::AwsAuth => "aws_auth_secrets",
            Self::OauthToken => "oauth_token_secrets",
            Self::PgDsn => "pg_dsn_secrets",
            Self::Hmac => "hmac_secrets",
            Self::SshKey => "ssh_key_secrets",
        }
    }

    pub fn oid_prefix(self) -> &'static str {
        match self {
            Self::Static => "ssr",
            Self::GcpAuth => "gas",
            Self::GcpIdToken => "gid",
            Self::AwsAuth => "aas",
            Self::OauthToken => "ots",
            Self::PgDsn => "pgs",
            Self::Hmac => "hms",
            Self::SshKey => "sks",
        }
    }

    /// The column naming this kind in `secret_sources`, `request_rules`
    /// and `grants`; also the `grant_key` the API reports.
    pub fn owner_column(self) -> &'static str {
        match self {
            Self::Static => "static_secret_id",
            Self::GcpAuth => "gcp_auth_secret_id",
            Self::GcpIdToken => "gcp_id_token_secret_id",
            Self::AwsAuth => "aws_auth_secret_id",
            Self::OauthToken => "oauth_token_secret_id",
            Self::PgDsn => "pg_dsn_secret_id",
            Self::Hmac => "hmac_secret_id",
            Self::SshKey => "ssh_key_secret_id",
        }
    }

    pub fn oid(self, id: i64) -> String {
        centaur_console_db::oid(self.oid_prefix(), id)
    }

    pub fn decode_oid(self, value: &str) -> Option<i64> {
        centaur_console_db::decode_oid(self.oid_prefix(), value)
    }

    pub fn has_rules(self) -> bool {
        !matches!(self, Self::PgDsn | Self::SshKey)
    }

    /// The request key of the kind's single source, for kinds that have
    /// exactly one (`None` for kinds keyed by credential role).
    pub fn source_key(self) -> Option<&'static str> {
        match self {
            Self::Static => Some("source"),
            Self::PgDsn => Some("dsn"),
            Self::GcpAuth | Self::GcpIdToken => Some("keyfile"),
            Self::SshKey => Some("private_key"),
            _ => None,
        }
    }

    /// Columns a document writes (besides `created_by_id`, the owner and
    /// broker links, which no document carries). `foreign_id` is first.
    pub fn document_columns(self) -> &'static [&'static str] {
        match self {
            Self::Static => &[
                "foreign_id",
                "name",
                "description",
                "labels",
                "kind",
                "inject_config",
                "replace_config",
            ],
            Self::GcpAuth => &[
                "foreign_id",
                "name",
                "description",
                "labels",
                "credentials_provider",
                "subject",
                "scopes",
            ],
            Self::GcpIdToken => &[
                "foreign_id",
                "name",
                "description",
                "labels",
                "audience",
                "header",
            ],
            Self::AwsAuth => &[
                "foreign_id",
                "name",
                "description",
                "labels",
                "allowed_regions",
                "allowed_services",
            ],
            Self::OauthToken => &[
                "foreign_id",
                "name",
                "description",
                "labels",
                "grant",
                "token_endpoint",
                "audience",
                "scopes",
                "header",
                "value_prefix",
            ],
            Self::PgDsn => &[
                "foreign_id",
                "name",
                "description",
                "labels",
                "database",
                "role",
                "settings",
            ],
            Self::Hmac => &[
                "foreign_id",
                "name",
                "description",
                "labels",
                "timestamp_format",
                "signature_algorithm",
                "signature_key_encoding",
                "signature_output_encoding",
                "signature_message",
                "allow_chunked_body",
                "headers",
            ],
            Self::SshKey => &["foreign_id", "name", "description", "labels", "public_key"],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_round_trip_and_columns_are_distinct() {
        for kind in ALL {
            assert_eq!(Kind::from_slug(kind.slug()), Some(kind));
            assert!(kind.owner_column().starts_with(kind.slug()));
            assert_eq!(kind.document_columns()[0], "foreign_id");
        }
        assert_eq!(Kind::from_slug("nope"), None);
    }
}
