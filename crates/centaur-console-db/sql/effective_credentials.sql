WITH candidate_grants AS (
    SELECT g.*
    FROM grants AS g
    WHERE g.principal_id = $1

    UNION ALL

    SELECT g.*
    FROM principal_roles AS pr
    JOIN grants AS g ON g.role_id = pr.role_id
    WHERE pr.principal_id = $1

    UNION ALL

    SELECT g.*
    FROM grants AS g
    JOIN static_secrets AS ss ON ss.id = g.static_secret_id
    JOIN broker_credentials AS bc ON bc.id = ss.broker_credential_id
    JOIN oauth_apps AS oa ON oa.id = bc.oauth_app_id AND oa.always_available
    JOIN secret_sources AS requester_source
      ON requester_source.static_secret_id = ss.id
     AND requester_source.broker_credential_id = bc.id
     AND requester_source.source_type = 'token_broker'
    WHERE $2::bigint IS NOT NULL
      AND g.principal_id = $2
      AND g.role_id IS NULL
),
effective_grants AS (
    SELECT g.* FROM candidate_grants g
    LEFT JOIN static_secrets ss ON ss.id = g.static_secret_id
    LEFT JOIN broker_credentials bc ON bc.id = ss.broker_credential_id
    LEFT JOIN oauth_apps oa ON oa.id = bc.oauth_app_id
    LEFT JOIN principals actor ON actor.id = COALESCE($2, $1)
    WHERE oa.provider IS DISTINCT FROM 'runlayer' OR (
        NOT oa.shared AND g.role_id IS NULL AND g.principal_id = actor.id AND (
            (actor.kind = 'console_user' AND actor.console_user_id = bc.created_by_id) OR
            (actor.kind IS DISTINCT FROM 'console_user' AND EXISTS (
                SELECT 1 FROM user_identities ui
                WHERE ui.user_id = bc.created_by_id AND ui.provider = 'slack'
                  AND ui.subject ~ '^([UW][A-Z0-9]{8,}|USLACK)$'
                  AND ui.team_id ~ '^[TE][A-Z0-9]{8,}$'
                  AND NULLIF(trim(actor.slack_user_id), '') IS NOT NULL
                  AND NULLIF(trim(actor.slack_team_id), '') IS NOT NULL
                  AND lower(trim(ui.subject)) = lower(trim(actor.slack_user_id))
                  AND lower(trim(ui.team_id)) = lower(trim(actor.slack_team_id))
                  AND NOT EXISTS (
                      SELECT 1 FROM user_identities other
                      WHERE other.user_id = ui.user_id AND other.provider = 'slack'
                        AND (lower(trim(other.subject)) IS DISTINCT FROM lower(trim(ui.subject))
                          OR lower(trim(other.team_id)) IS DISTINCT FROM lower(trim(ui.team_id)))
                  )
            ))
        )
    )
),
credential_refs AS (
    SELECT
        ref.kind,
        ref.credential_id,
        MAX(g.priority) AS effective_priority
    FROM effective_grants AS g
    CROSS JOIN LATERAL (
        VALUES
            ('static',       g.static_secret_id),
            ('gcp_auth',     g.gcp_auth_secret_id),
            ('gcp_id_token', g.gcp_id_token_secret_id),
            ('aws_auth',     g.aws_auth_secret_id),
            ('oauth_token',  g.oauth_token_secret_id),
            ('pg_dsn',       g.pg_dsn_secret_id),
            ('hmac',         g.hmac_secret_id)
    ) AS ref(kind, credential_id)
    WHERE ref.credential_id IS NOT NULL
    GROUP BY ref.kind, ref.credential_id
),
credentials AS (
    SELECT r.*, to_jsonb(s) AS credential
    FROM credential_refs AS r
    JOIN static_secrets AS s ON s.id = r.credential_id
    WHERE r.kind = 'static'
    UNION ALL
    SELECT r.*, to_jsonb(s) FROM credential_refs AS r
    JOIN gcp_auth_secrets AS s ON s.id = r.credential_id WHERE r.kind = 'gcp_auth'
    UNION ALL
    SELECT r.*, to_jsonb(s) FROM credential_refs AS r
    JOIN gcp_id_token_secrets AS s ON s.id = r.credential_id WHERE r.kind = 'gcp_id_token'
    UNION ALL
    SELECT r.*, to_jsonb(s) FROM credential_refs AS r
    JOIN aws_auth_secrets AS s ON s.id = r.credential_id WHERE r.kind = 'aws_auth'
    UNION ALL
    SELECT r.*, to_jsonb(s) FROM credential_refs AS r
    JOIN oauth_token_secrets AS s ON s.id = r.credential_id WHERE r.kind = 'oauth_token'
    UNION ALL
    SELECT r.*, to_jsonb(s) FROM credential_refs AS r
    JOIN pg_dsn_secrets AS s ON s.id = r.credential_id WHERE r.kind = 'pg_dsn'
    UNION ALL
    SELECT r.*, to_jsonb(s) FROM credential_refs AS r
    JOIN hmac_secrets AS s ON s.id = r.credential_id WHERE r.kind = 'hmac'
),
source_rows AS (
    SELECT c.kind, c.credential_id, s.id AS source_id, s.broker_credential_id, to_jsonb(s) AS source
    FROM credentials AS c JOIN secret_sources AS s ON s.static_secret_id = c.credential_id WHERE c.kind = 'static'
    UNION ALL
    SELECT c.kind, c.credential_id, s.id, s.broker_credential_id, to_jsonb(s)
    FROM credentials AS c JOIN secret_sources AS s ON s.gcp_auth_secret_id = c.credential_id WHERE c.kind = 'gcp_auth'
    UNION ALL
    SELECT c.kind, c.credential_id, s.id, s.broker_credential_id, to_jsonb(s)
    FROM credentials AS c JOIN secret_sources AS s ON s.gcp_id_token_secret_id = c.credential_id WHERE c.kind = 'gcp_id_token'
    UNION ALL
    SELECT c.kind, c.credential_id, s.id, s.broker_credential_id, to_jsonb(s)
    FROM credentials AS c JOIN secret_sources AS s ON s.aws_auth_secret_id = c.credential_id WHERE c.kind = 'aws_auth'
    UNION ALL
    SELECT c.kind, c.credential_id, s.id, s.broker_credential_id, to_jsonb(s)
    FROM credentials AS c JOIN secret_sources AS s ON s.oauth_token_secret_id = c.credential_id WHERE c.kind = 'oauth_token'
    UNION ALL
    SELECT c.kind, c.credential_id, s.id, s.broker_credential_id, to_jsonb(s)
    FROM credentials AS c JOIN secret_sources AS s ON s.pg_dsn_secret_id = c.credential_id WHERE c.kind = 'pg_dsn'
    UNION ALL
    SELECT c.kind, c.credential_id, s.id, s.broker_credential_id, to_jsonb(s)
    FROM credentials AS c JOIN secret_sources AS s ON s.hmac_secret_id = c.credential_id WHERE c.kind = 'hmac'
),
sources AS (
    SELECT
        sr.kind,
        sr.credential_id,
        jsonb_agg(
            sr.source || jsonb_strip_nulls(jsonb_build_object('broker_access_token',
                CASE WHEN NOT bc.dead AND (bc.expires_at IS NULL OR bc.expires_at > CURRENT_TIMESTAMP)
                     AND (oa.provider IS DISTINCT FROM 'runlayer' OR
                          (oa.enabled AND credential_owner.status = 'active' AND $3::text = bc.labels->>'runlayer_resource'
                           AND sr.kind = 'static' AND EXISTS (
                               SELECT 1 FROM static_secrets wrapper
                               WHERE wrapper.id = sr.credential_id AND wrapper.broker_credential_id = bc.id
                           )))
                THEN bc.access_token END))
            ORDER BY sr.source_id
        ) AS sources
    FROM source_rows AS sr
    LEFT JOIN broker_credentials AS bc ON bc.id = sr.broker_credential_id
    LEFT JOIN oauth_apps AS oa ON oa.id = bc.oauth_app_id
    LEFT JOIN users credential_owner ON credential_owner.id = bc.created_by_id
    GROUP BY sr.kind, sr.credential_id
),
rule_rows AS (
    SELECT c.kind, c.credential_id, r.id AS rule_id, r.position, to_jsonb(r) AS rule
    FROM credentials AS c JOIN request_rules AS r ON r.static_secret_id = c.credential_id WHERE c.kind = 'static'
    UNION ALL
    SELECT c.kind, c.credential_id, r.id, r.position, to_jsonb(r)
    FROM credentials AS c JOIN request_rules AS r ON r.gcp_auth_secret_id = c.credential_id WHERE c.kind = 'gcp_auth'
    UNION ALL
    SELECT c.kind, c.credential_id, r.id, r.position, to_jsonb(r)
    FROM credentials AS c JOIN request_rules AS r ON r.gcp_id_token_secret_id = c.credential_id WHERE c.kind = 'gcp_id_token'
    UNION ALL
    SELECT c.kind, c.credential_id, r.id, r.position, to_jsonb(r)
    FROM credentials AS c JOIN request_rules AS r ON r.aws_auth_secret_id = c.credential_id WHERE c.kind = 'aws_auth'
    UNION ALL
    SELECT c.kind, c.credential_id, r.id, r.position, to_jsonb(r)
    FROM credentials AS c JOIN request_rules AS r ON r.oauth_token_secret_id = c.credential_id WHERE c.kind = 'oauth_token'
    UNION ALL
    SELECT c.kind, c.credential_id, r.id, r.position, to_jsonb(r)
    FROM credentials AS c JOIN request_rules AS r ON r.hmac_secret_id = c.credential_id WHERE c.kind = 'hmac'
),
rules AS (
    SELECT kind, credential_id, jsonb_agg(rule ORDER BY position, rule_id) AS rules
    FROM rule_rows GROUP BY kind, credential_id
)
SELECT
    c.kind,
    c.credential_id,
    c.effective_priority,
    c.credential,
    COALESCE(s.sources, '[]'::jsonb) AS sources,
    COALESCE(r.rules, '[]'::jsonb) AS rules
FROM credentials AS c
LEFT JOIN sources AS s ON s.kind = c.kind AND s.credential_id = c.credential_id
LEFT JOIN rules AS r ON r.kind = c.kind AND r.credential_id = c.credential_id
ORDER BY c.effective_priority, c.kind, c.credential_id
