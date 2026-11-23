-- Console schema as of the last Rails migration (20261112120000), dumped from a
-- database built by `bin/rails db:schema:load` (identical to one built by
-- running every Rails migration). api-rs owns the schema from here on: add new
-- changes as later migrations in this directory, never as Rails migrations.
--
-- A database Rails already migrated to this point records this migration as
-- applied without running it (see `centaur_console_db::migrate`). The Rails
-- bookkeeping tables are kept, and the Rails versions recorded, so a rollback
-- to a chart that still runs `bin/rails db:prepare` finds nothing to do instead
-- of loading `schema.rb` over existing tables.

CREATE TABLE public.api_client_principals (
    id bigint NOT NULL,
    api_client_id bigint NOT NULL,
    principal_id bigint NOT NULL,
    "default" boolean DEFAULT false NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.api_client_principals_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.api_client_principals_id_seq OWNED BY public.api_client_principals.id;

CREATE TABLE public.api_clients (
    id bigint NOT NULL,
    name character varying NOT NULL,
    secret_digest character varying NOT NULL,
    scopes jsonb DEFAULT '[]'::jsonb NOT NULL,
    disabled_at timestamp(6) without time zone,
    created_by_id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.api_clients_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.api_clients_id_seq OWNED BY public.api_clients.id;

CREATE TABLE public.api_keys (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    deleted_at timestamp(6) without time zone,
    name character varying NOT NULL,
    token_hash character varying NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL,
    user_id bigint NOT NULL
);

CREATE SEQUENCE public.api_keys_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.api_keys_id_seq OWNED BY public.api_keys.id;

CREATE TABLE public.ar_internal_metadata (
    key character varying NOT NULL,
    value character varying,
    created_at timestamp(6) without time zone NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE TABLE public.aws_auth_secrets (
    id bigint NOT NULL,
    allowed_regions jsonb DEFAULT '[]'::jsonb NOT NULL,
    allowed_services jsonb DEFAULT '[]'::jsonb NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    description character varying,
    foreign_id character varying,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.aws_auth_secrets_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.aws_auth_secrets_id_seq OWNED BY public.aws_auth_secrets.id;

CREATE TABLE public.broker_credentials (
    id bigint NOT NULL,
    access_token text,
    api_key text,
    client_id character varying,
    client_secret text,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint,
    dead boolean DEFAULT false NOT NULL,
    dead_reason character varying,
    description character varying,
    early_refresh_fraction double precision DEFAULT 0.2 NOT NULL,
    early_refresh_slack_seconds integer DEFAULT 300 NOT NULL,
    expires_at timestamp(6) without time zone,
    external_user_key character varying,
    failure_count integer DEFAULT 0 NOT NULL,
    foreign_id character varying,
    "grant" character varying DEFAULT 'refresh_token'::character varying NOT NULL,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    last_refresh timestamp(6) without time zone,
    max_refresh_interval_seconds integer DEFAULT 86400 NOT NULL,
    name character varying,
    next_attempt_at timestamp(6) without time zone,
    oauth_app_id bigint,
    password text,
    provider_email character varying,
    provider_subject character varying,
    refresh_timeout_seconds integer DEFAULT 30 NOT NULL,
    refresh_token text,
    scopes jsonb DEFAULT '[]'::jsonb NOT NULL,
    token_endpoint character varying NOT NULL,
    token_endpoint_headers text,
    updated_at timestamp(6) without time zone NOT NULL,
    username text
);

CREATE SEQUENCE public.broker_credentials_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.broker_credentials_id_seq OWNED BY public.broker_credentials.id;

CREATE TABLE public.gcp_auth_secrets (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    credentials_provider jsonb,
    description character varying,
    foreign_id character varying,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying,
    scopes jsonb DEFAULT '[]'::jsonb NOT NULL,
    subject character varying,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.gcp_auth_secrets_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.gcp_auth_secrets_id_seq OWNED BY public.gcp_auth_secrets.id;

CREATE TABLE public.gcp_id_token_secrets (
    id bigint NOT NULL,
    audience character varying NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    description character varying,
    foreign_id character varying,
    header character varying,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.gcp_id_token_secrets_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.gcp_id_token_secrets_id_seq OWNED BY public.gcp_id_token_secrets.id;

CREATE TABLE public.grants (
    id bigint NOT NULL,
    aws_auth_secret_id bigint,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    gcp_auth_secret_id bigint,
    gcp_id_token_secret_id bigint,
    hmac_secret_id bigint,
    oauth_token_secret_id bigint,
    pg_dsn_secret_id bigint,
    principal_id bigint,
    priority integer NOT NULL,
    role_id bigint,
    static_secret_id bigint,
    updated_at timestamp(6) without time zone NOT NULL,
    ssh_key_secret_id bigint
);

CREATE SEQUENCE public.grants_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.grants_id_seq OWNED BY public.grants.id;

CREATE TABLE public.hmac_secrets (
    id bigint NOT NULL,
    allow_chunked_body boolean DEFAULT false NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    description character varying,
    foreign_id character varying,
    headers jsonb DEFAULT '[]'::jsonb NOT NULL,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying,
    signature_algorithm character varying,
    signature_key_encoding character varying,
    signature_message text,
    signature_output_encoding character varying,
    timestamp_format character varying,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.hmac_secrets_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.hmac_secrets_id_seq OWNED BY public.hmac_secrets.id;

CREATE TABLE public.mcp_oauth_authorization_codes (
    id bigint NOT NULL,
    code_challenge character varying NOT NULL,
    code_hash character varying NOT NULL,
    consumed_at timestamp(6) without time zone,
    created_at timestamp(6) without time zone NOT NULL,
    expires_at timestamp(6) without time zone NOT NULL,
    mcp_oauth_client_id bigint NOT NULL,
    principal_id bigint NOT NULL,
    redirect_uri character varying NOT NULL,
    resource character varying NOT NULL,
    scopes jsonb DEFAULT '[]'::jsonb NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL,
    user_id bigint NOT NULL
);

CREATE SEQUENCE public.mcp_oauth_authorization_codes_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.mcp_oauth_authorization_codes_id_seq OWNED BY public.mcp_oauth_authorization_codes.id;

CREATE TABLE public.mcp_oauth_clients (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    grant_types jsonb DEFAULT '[]'::jsonb NOT NULL,
    last_used_at timestamp(6) without time zone,
    metadata jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying,
    redirect_uris jsonb DEFAULT '[]'::jsonb NOT NULL,
    response_types jsonb DEFAULT '[]'::jsonb NOT NULL,
    scopes jsonb DEFAULT '[]'::jsonb NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.mcp_oauth_clients_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.mcp_oauth_clients_id_seq OWNED BY public.mcp_oauth_clients.id;

CREATE TABLE public.mcp_oauth_refresh_tokens (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    expires_at timestamp(6) without time zone NOT NULL,
    last_used_at timestamp(6) without time zone,
    mcp_oauth_client_id bigint NOT NULL,
    principal_id bigint NOT NULL,
    resource character varying NOT NULL,
    revoked_at timestamp(6) without time zone,
    scopes jsonb DEFAULT '[]'::jsonb NOT NULL,
    token_hash character varying NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL,
    user_id bigint NOT NULL
);

CREATE SEQUENCE public.mcp_oauth_refresh_tokens_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.mcp_oauth_refresh_tokens_id_seq OWNED BY public.mcp_oauth_refresh_tokens.id;

CREATE TABLE public.oauth_apps (
    id bigint NOT NULL,
    allowed_scopes jsonb DEFAULT '[]'::jsonb NOT NULL,
    always_available boolean DEFAULT false NOT NULL,
    client_id character varying NOT NULL,
    client_secret text,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    description character varying,
    enabled boolean DEFAULT true NOT NULL,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    provider character varying NOT NULL,
    shared boolean DEFAULT false NOT NULL,
    slug character varying NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL,
    provider_config jsonb DEFAULT '{}'::jsonb NOT NULL,
    owner_only boolean DEFAULT false NOT NULL
);

CREATE SEQUENCE public.oauth_apps_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.oauth_apps_id_seq OWNED BY public.oauth_apps.id;

CREATE TABLE public.oauth_token_secrets (
    id bigint NOT NULL,
    audience character varying,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    description character varying,
    foreign_id character varying,
    "grant" character varying,
    header character varying,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying,
    scopes jsonb DEFAULT '[]'::jsonb NOT NULL,
    token_endpoint character varying,
    updated_at timestamp(6) without time zone NOT NULL,
    value_prefix character varying
);

CREATE SEQUENCE public.oauth_token_secrets_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.oauth_token_secrets_id_seq OWNED BY public.oauth_token_secrets.id;

CREATE TABLE public.pg_dsn_secrets (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    database character varying NOT NULL,
    description character varying,
    foreign_id character varying,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying,
    role character varying,
    settings jsonb DEFAULT '[]'::jsonb NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.pg_dsn_secrets_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.pg_dsn_secrets_id_seq OWNED BY public.pg_dsn_secrets.id;

CREATE TABLE public.principal_roles (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    principal_id bigint NOT NULL,
    role_id bigint NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.principal_roles_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.principal_roles_id_seq OWNED BY public.principal_roles.id;

CREATE TABLE public.principal_sync_config_snapshots (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    payload text NOT NULL,
    principal_cache_version bigint NOT NULL,
    principal_id bigint NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.principal_sync_config_snapshots_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.principal_sync_config_snapshots_id_seq OWNED BY public.principal_sync_config_snapshots.id;

CREATE TABLE public.principals (
    id bigint NOT NULL,
    console_user_id bigint,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    foreign_id character varying,
    kind character varying DEFAULT 'unknown'::character varying NOT NULL,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying,
    sandbox_api_server_enabled boolean DEFAULT true NOT NULL,
    sandbox_observability_enabled boolean DEFAULT true NOT NULL,
    sandbox_repo_cache character varying DEFAULT 'all'::character varying NOT NULL,
    sandbox_sessions_read_enabled boolean DEFAULT false NOT NULL,
    sandbox_workflows_read_enabled boolean DEFAULT false NOT NULL,
    sandbox_workflows_write_enabled boolean DEFAULT false NOT NULL,
    slack_channel_id character varying,
    slack_email character varying,
    slack_team_id character varying,
    slack_user_id character varying,
    sync_config_cache_version bigint DEFAULT 0 NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.principals_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.principals_id_seq OWNED BY public.principals.id;

CREATE TABLE public.proxies (
    id bigint NOT NULL,
    bearer_token_hash character varying NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying NOT NULL,
    principal_assigned_at timestamp(6) without time zone,
    principal_id bigint,
    requester_principal_assigned_at timestamp(6) without time zone,
    requester_principal_id bigint,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.proxies_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.proxies_id_seq OWNED BY public.proxies.id;

CREATE TABLE public.request_rules (
    id bigint NOT NULL,
    aws_auth_secret_id bigint,
    cidr character varying,
    created_at timestamp(6) without time zone NOT NULL,
    gcp_auth_secret_id bigint,
    gcp_id_token_secret_id bigint,
    hmac_secret_id bigint,
    host character varying,
    http_methods jsonb DEFAULT '[]'::jsonb NOT NULL,
    oauth_token_secret_id bigint,
    paths jsonb DEFAULT '[]'::jsonb NOT NULL,
    "position" integer DEFAULT 0 NOT NULL,
    static_secret_id bigint,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.request_rules_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.request_rules_id_seq OWNED BY public.request_rules.id;

CREATE TABLE public.roles (
    id bigint NOT NULL,
    assign_by_default boolean DEFAULT false NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    foreign_id character varying,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.roles_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.roles_id_seq OWNED BY public.roles.id;

CREATE TABLE public.scheduled_tasks (
    id bigint NOT NULL,
    author_id bigint,
    created_at timestamp(6) without time zone NOT NULL,
    cron_expression character varying,
    delivery_channel character varying NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    last_enqueued_at timestamp(6) without time zone,
    last_error text,
    last_run_at timestamp(6) without time zone,
    last_run_id character varying,
    name character varying NOT NULL,
    next_run_at timestamp(6) without time zone,
    prompt text NOT NULL,
    timezone character varying DEFAULT 'America/Los_Angeles'::character varying NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL,
    principal_id bigint,
    CONSTRAINT scheduled_tasks_exactly_one_owner CHECK (((author_id IS NULL) <> (principal_id IS NULL)))
);

CREATE SEQUENCE public.scheduled_tasks_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.scheduled_tasks_id_seq OWNED BY public.scheduled_tasks.id;

CREATE TABLE public.schema_migrations (
    version character varying NOT NULL
);

CREATE TABLE public.secret_sources (
    id bigint NOT NULL,
    aws_auth_secret_id bigint,
    broker_credential_id bigint,
    config jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    gcp_auth_secret_id bigint,
    gcp_id_token_secret_id bigint,
    hmac_secret_id bigint,
    oauth_token_secret_id bigint,
    pg_dsn_secret_id bigint,
    role character varying,
    role_kind character varying,
    secret text,
    source_type character varying NOT NULL,
    static_secret_id bigint,
    updated_at timestamp(6) without time zone NOT NULL,
    ssh_key_secret_id bigint,
    CONSTRAINT secret_sources_broker_credential_requires_token_broker CHECK (((broker_credential_id IS NULL) OR ((source_type)::text = 'token_broker'::text)))
);

CREATE SEQUENCE public.secret_sources_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.secret_sources_id_seq OWNED BY public.secret_sources.id;

CREATE TABLE public.skill_editors (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    skill_id bigint NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL,
    user_id bigint NOT NULL
);

CREATE SEQUENCE public.skill_editors_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.skill_editors_id_seq OWNED BY public.skill_editors.id;

CREATE TABLE public.skills (
    id bigint NOT NULL,
    archived_at timestamp(6) without time zone,
    content text NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    description text NOT NULL,
    lock_version integer DEFAULT 0 NOT NULL,
    name character varying NOT NULL,
    search_vector tsvector GENERATED ALWAYS AS (((setweight(to_tsvector('english'::regconfig, (name)::text), 'A'::"char") || setweight(to_tsvector('english'::regconfig, description), 'B'::"char")) || setweight(to_tsvector('english'::regconfig, content), 'C'::"char"))) STORED,
    shared_at timestamp(6) without time zone,
    updated_at timestamp(6) without time zone NOT NULL,
    user_id bigint NOT NULL,
    visibility character varying DEFAULT 'shared'::character varying NOT NULL
);

CREATE SEQUENCE public.skills_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.skills_id_seq OWNED BY public.skills.id;

CREATE TABLE public.slack_bot_channels (
    id bigint NOT NULL,
    active boolean DEFAULT true NOT NULL,
    archived boolean DEFAULT false NOT NULL,
    bot_user_id character varying NOT NULL,
    channel_id character varying NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    last_seen_at timestamp(6) without time zone,
    member_user_ids text[] DEFAULT '{}'::text[] NOT NULL,
    membership_error text,
    membership_last_attempted_at timestamp(6) without time zone,
    membership_refreshed_at timestamp(6) without time zone,
    name character varying NOT NULL,
    private boolean DEFAULT false NOT NULL,
    team_id character varying NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.slack_bot_channels_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.slack_bot_channels_id_seq OWNED BY public.slack_bot_channels.id;

CREATE TABLE public.slack_channel_permissions (
    id bigint NOT NULL,
    channel_id character varying NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    download_enabled boolean DEFAULT false NOT NULL,
    history_enabled boolean DEFAULT false NOT NULL,
    principal_id bigint,
    role_id bigint,
    updated_at timestamp(6) without time zone NOT NULL,
    upload_enabled boolean DEFAULT false NOT NULL,
    CONSTRAINT slack_channel_permissions_exactly_one_grantee CHECK (((principal_id IS NOT NULL) <> (role_id IS NOT NULL)))
);

CREATE SEQUENCE public.slack_channel_permissions_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.slack_channel_permissions_id_seq OWNED BY public.slack_channel_permissions.id;

CREATE TABLE public.slack_dm_sync_cursors (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    next_conversation_id character varying,
    next_credential_id bigint,
    not_before timestamp(6) without time zone,
    oauth_app_slug character varying NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.slack_dm_sync_cursors_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.slack_dm_sync_cursors_id_seq OWNED BY public.slack_dm_sync_cursors.id;

CREATE TABLE public.ssh_key_secrets (
    id bigint NOT NULL,
    foreign_id character varying,
    name character varying NOT NULL,
    description character varying,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    public_key text NOT NULL,
    owner_id bigint,
    created_by_id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.ssh_key_secrets_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.ssh_key_secrets_id_seq OWNED BY public.ssh_key_secrets.id;

CREATE TABLE public.static_secrets (
    id bigint NOT NULL,
    broker_credential_id bigint,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint,
    description character varying,
    foreign_id character varying,
    inject_config jsonb,
    kind character varying DEFAULT 'custom'::character varying NOT NULL,
    labels jsonb DEFAULT '{}'::jsonb NOT NULL,
    name character varying,
    replace_config jsonb,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.static_secrets_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.static_secrets_id_seq OWNED BY public.static_secrets.id;

CREATE TABLE public.system_settings (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    default_sandbox_api_server_enabled boolean DEFAULT true NOT NULL,
    default_sandbox_observability_enabled boolean DEFAULT true NOT NULL,
    default_sandbox_repo_cache character varying DEFAULT 'all'::character varying NOT NULL,
    default_sandbox_sessions_read_enabled boolean DEFAULT false NOT NULL,
    default_sandbox_workflows_read_enabled boolean DEFAULT false NOT NULL,
    default_sandbox_workflows_write_enabled boolean DEFAULT false NOT NULL,
    singleton boolean DEFAULT true NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.system_settings_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.system_settings_id_seq OWNED BY public.system_settings.id;

CREATE TABLE public.thread_shares (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    created_by_id bigint NOT NULL,
    thread_key character varying(512) NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.thread_shares_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.thread_shares_id_seq OWNED BY public.thread_shares.id;

CREATE TABLE public.user_identities (
    id bigint NOT NULL,
    created_at timestamp(6) without time zone NOT NULL,
    email character varying,
    email_verified boolean DEFAULT false NOT NULL,
    provider character varying NOT NULL,
    subject character varying NOT NULL,
    team_id character varying,
    updated_at timestamp(6) without time zone NOT NULL,
    user_id bigint NOT NULL
);

CREATE SEQUENCE public.user_identities_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.user_identities_id_seq OWNED BY public.user_identities.id;

CREATE TABLE public.users (
    id bigint NOT NULL,
    admin boolean DEFAULT false NOT NULL,
    approved_at timestamp(6) without time zone,
    approved_by_id bigint,
    created_at timestamp(6) without time zone NOT NULL,
    email character varying NOT NULL,
    name character varying,
    password_digest character varying,
    status character varying DEFAULT 'pending'::character varying NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE SEQUENCE public.users_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.users_id_seq OWNED BY public.users.id;

ALTER TABLE ONLY public.api_client_principals ALTER COLUMN id SET DEFAULT nextval('public.api_client_principals_id_seq'::regclass);

ALTER TABLE ONLY public.api_clients ALTER COLUMN id SET DEFAULT nextval('public.api_clients_id_seq'::regclass);

ALTER TABLE ONLY public.api_keys ALTER COLUMN id SET DEFAULT nextval('public.api_keys_id_seq'::regclass);

ALTER TABLE ONLY public.aws_auth_secrets ALTER COLUMN id SET DEFAULT nextval('public.aws_auth_secrets_id_seq'::regclass);

ALTER TABLE ONLY public.broker_credentials ALTER COLUMN id SET DEFAULT nextval('public.broker_credentials_id_seq'::regclass);

ALTER TABLE ONLY public.gcp_auth_secrets ALTER COLUMN id SET DEFAULT nextval('public.gcp_auth_secrets_id_seq'::regclass);

ALTER TABLE ONLY public.gcp_id_token_secrets ALTER COLUMN id SET DEFAULT nextval('public.gcp_id_token_secrets_id_seq'::regclass);

ALTER TABLE ONLY public.grants ALTER COLUMN id SET DEFAULT nextval('public.grants_id_seq'::regclass);

ALTER TABLE ONLY public.hmac_secrets ALTER COLUMN id SET DEFAULT nextval('public.hmac_secrets_id_seq'::regclass);

ALTER TABLE ONLY public.mcp_oauth_authorization_codes ALTER COLUMN id SET DEFAULT nextval('public.mcp_oauth_authorization_codes_id_seq'::regclass);

ALTER TABLE ONLY public.mcp_oauth_clients ALTER COLUMN id SET DEFAULT nextval('public.mcp_oauth_clients_id_seq'::regclass);

ALTER TABLE ONLY public.mcp_oauth_refresh_tokens ALTER COLUMN id SET DEFAULT nextval('public.mcp_oauth_refresh_tokens_id_seq'::regclass);

ALTER TABLE ONLY public.oauth_apps ALTER COLUMN id SET DEFAULT nextval('public.oauth_apps_id_seq'::regclass);

ALTER TABLE ONLY public.oauth_token_secrets ALTER COLUMN id SET DEFAULT nextval('public.oauth_token_secrets_id_seq'::regclass);

ALTER TABLE ONLY public.pg_dsn_secrets ALTER COLUMN id SET DEFAULT nextval('public.pg_dsn_secrets_id_seq'::regclass);

ALTER TABLE ONLY public.principal_roles ALTER COLUMN id SET DEFAULT nextval('public.principal_roles_id_seq'::regclass);

ALTER TABLE ONLY public.principal_sync_config_snapshots ALTER COLUMN id SET DEFAULT nextval('public.principal_sync_config_snapshots_id_seq'::regclass);

ALTER TABLE ONLY public.principals ALTER COLUMN id SET DEFAULT nextval('public.principals_id_seq'::regclass);

ALTER TABLE ONLY public.proxies ALTER COLUMN id SET DEFAULT nextval('public.proxies_id_seq'::regclass);

ALTER TABLE ONLY public.request_rules ALTER COLUMN id SET DEFAULT nextval('public.request_rules_id_seq'::regclass);

ALTER TABLE ONLY public.roles ALTER COLUMN id SET DEFAULT nextval('public.roles_id_seq'::regclass);

ALTER TABLE ONLY public.scheduled_tasks ALTER COLUMN id SET DEFAULT nextval('public.scheduled_tasks_id_seq'::regclass);

ALTER TABLE ONLY public.secret_sources ALTER COLUMN id SET DEFAULT nextval('public.secret_sources_id_seq'::regclass);

ALTER TABLE ONLY public.skill_editors ALTER COLUMN id SET DEFAULT nextval('public.skill_editors_id_seq'::regclass);

ALTER TABLE ONLY public.skills ALTER COLUMN id SET DEFAULT nextval('public.skills_id_seq'::regclass);

ALTER TABLE ONLY public.slack_bot_channels ALTER COLUMN id SET DEFAULT nextval('public.slack_bot_channels_id_seq'::regclass);

ALTER TABLE ONLY public.slack_channel_permissions ALTER COLUMN id SET DEFAULT nextval('public.slack_channel_permissions_id_seq'::regclass);

ALTER TABLE ONLY public.slack_dm_sync_cursors ALTER COLUMN id SET DEFAULT nextval('public.slack_dm_sync_cursors_id_seq'::regclass);

ALTER TABLE ONLY public.ssh_key_secrets ALTER COLUMN id SET DEFAULT nextval('public.ssh_key_secrets_id_seq'::regclass);

ALTER TABLE ONLY public.static_secrets ALTER COLUMN id SET DEFAULT nextval('public.static_secrets_id_seq'::regclass);

ALTER TABLE ONLY public.system_settings ALTER COLUMN id SET DEFAULT nextval('public.system_settings_id_seq'::regclass);

ALTER TABLE ONLY public.thread_shares ALTER COLUMN id SET DEFAULT nextval('public.thread_shares_id_seq'::regclass);

ALTER TABLE ONLY public.user_identities ALTER COLUMN id SET DEFAULT nextval('public.user_identities_id_seq'::regclass);

ALTER TABLE ONLY public.users ALTER COLUMN id SET DEFAULT nextval('public.users_id_seq'::regclass);

ALTER TABLE ONLY public.api_client_principals
    ADD CONSTRAINT api_client_principals_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.api_clients
    ADD CONSTRAINT api_clients_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.api_keys
    ADD CONSTRAINT api_keys_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.ar_internal_metadata
    ADD CONSTRAINT ar_internal_metadata_pkey PRIMARY KEY (key);

ALTER TABLE ONLY public.aws_auth_secrets
    ADD CONSTRAINT aws_auth_secrets_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.broker_credentials
    ADD CONSTRAINT broker_credentials_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.gcp_auth_secrets
    ADD CONSTRAINT gcp_auth_secrets_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.gcp_id_token_secrets
    ADD CONSTRAINT gcp_id_token_secrets_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT grants_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.hmac_secrets
    ADD CONSTRAINT hmac_secrets_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.mcp_oauth_authorization_codes
    ADD CONSTRAINT mcp_oauth_authorization_codes_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.mcp_oauth_clients
    ADD CONSTRAINT mcp_oauth_clients_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.mcp_oauth_refresh_tokens
    ADD CONSTRAINT mcp_oauth_refresh_tokens_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.oauth_apps
    ADD CONSTRAINT oauth_apps_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.oauth_token_secrets
    ADD CONSTRAINT oauth_token_secrets_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.pg_dsn_secrets
    ADD CONSTRAINT pg_dsn_secrets_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.principal_roles
    ADD CONSTRAINT principal_roles_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.principal_sync_config_snapshots
    ADD CONSTRAINT principal_sync_config_snapshots_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.principals
    ADD CONSTRAINT principals_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.proxies
    ADD CONSTRAINT proxies_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.request_rules
    ADD CONSTRAINT request_rules_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.roles
    ADD CONSTRAINT roles_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.scheduled_tasks
    ADD CONSTRAINT scheduled_tasks_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.schema_migrations
    ADD CONSTRAINT schema_migrations_pkey PRIMARY KEY (version);

ALTER TABLE ONLY public.secret_sources
    ADD CONSTRAINT secret_sources_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.skill_editors
    ADD CONSTRAINT skill_editors_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.skills
    ADD CONSTRAINT skills_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.slack_bot_channels
    ADD CONSTRAINT slack_bot_channels_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.slack_channel_permissions
    ADD CONSTRAINT slack_channel_permissions_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.slack_dm_sync_cursors
    ADD CONSTRAINT slack_dm_sync_cursors_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.ssh_key_secrets
    ADD CONSTRAINT ssh_key_secrets_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.static_secrets
    ADD CONSTRAINT static_secrets_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.system_settings
    ADD CONSTRAINT system_settings_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.thread_shares
    ADD CONSTRAINT thread_shares_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.user_identities
    ADD CONSTRAINT user_identities_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_pkey PRIMARY KEY (id);

CREATE UNIQUE INDEX idx_principal_sync_snapshots_on_principal_version ON public.principal_sync_config_snapshots USING btree (principal_id, principal_cache_version);

CREATE UNIQUE INDEX idx_slack_permissions_unique_principal_channel ON public.slack_channel_permissions USING btree (principal_id, channel_id) WHERE (principal_id IS NOT NULL);

CREATE UNIQUE INDEX idx_slack_permissions_unique_role_channel ON public.slack_channel_permissions USING btree (role_id, channel_id) WHERE (role_id IS NOT NULL);

CREATE INDEX index_active_skills_for_catalog ON public.skills USING btree (visibility, updated_at) WHERE (archived_at IS NULL);

CREATE UNIQUE INDEX index_active_skills_on_name ON public.skills USING btree (name) WHERE (archived_at IS NULL);

CREATE INDEX index_api_client_principals_on_api_client_id ON public.api_client_principals USING btree (api_client_id);

CREATE UNIQUE INDEX index_api_client_principals_on_api_client_id_and_principal_id ON public.api_client_principals USING btree (api_client_id, principal_id);

CREATE UNIQUE INDEX index_api_client_principals_on_default ON public.api_client_principals USING btree (api_client_id) WHERE "default";

CREATE INDEX index_api_client_principals_on_principal_id ON public.api_client_principals USING btree (principal_id);

CREATE INDEX index_api_clients_on_created_by_id ON public.api_clients USING btree (created_by_id);

CREATE UNIQUE INDEX index_api_clients_on_secret_digest ON public.api_clients USING btree (secret_digest);

CREATE INDEX index_api_keys_on_deleted_at ON public.api_keys USING btree (deleted_at);

CREATE UNIQUE INDEX index_api_keys_on_token_hash ON public.api_keys USING btree (token_hash);

CREATE INDEX index_api_keys_on_user_id ON public.api_keys USING btree (user_id);

CREATE INDEX index_aws_auth_secrets_on_created_by_id ON public.aws_auth_secrets USING btree (created_by_id);

CREATE UNIQUE INDEX index_aws_auth_secrets_on_foreign_id ON public.aws_auth_secrets USING btree (foreign_id);

CREATE INDEX index_aws_auth_secrets_on_labels ON public.aws_auth_secrets USING gin (labels);

CREATE INDEX index_broker_credentials_on_created_by_id ON public.broker_credentials USING btree (created_by_id);

CREATE UNIQUE INDEX index_broker_credentials_on_foreign_id ON public.broker_credentials USING btree (foreign_id);

CREATE INDEX index_broker_credentials_on_labels ON public.broker_credentials USING gin (labels);

CREATE INDEX index_broker_credentials_on_next_attempt_at ON public.broker_credentials USING btree (next_attempt_at);

CREATE INDEX index_broker_credentials_on_oauth_app_id ON public.broker_credentials USING btree (oauth_app_id);

CREATE UNIQUE INDEX index_broker_credentials_on_oauth_app_id_and_provider_subject ON public.broker_credentials USING btree (oauth_app_id, provider_subject) WHERE (provider_subject IS NOT NULL);

CREATE INDEX index_gcp_auth_secrets_on_created_by_id ON public.gcp_auth_secrets USING btree (created_by_id);

CREATE UNIQUE INDEX index_gcp_auth_secrets_on_foreign_id ON public.gcp_auth_secrets USING btree (foreign_id);

CREATE INDEX index_gcp_auth_secrets_on_labels ON public.gcp_auth_secrets USING gin (labels);

CREATE INDEX index_gcp_id_token_secrets_on_created_by_id ON public.gcp_id_token_secrets USING btree (created_by_id);

CREATE UNIQUE INDEX index_gcp_id_token_secrets_on_foreign_id ON public.gcp_id_token_secrets USING btree (foreign_id);

CREATE INDEX index_gcp_id_token_secrets_on_labels ON public.gcp_id_token_secrets USING gin (labels);

CREATE INDEX index_grants_on_aws_auth_secret_id ON public.grants USING btree (aws_auth_secret_id);

CREATE INDEX index_grants_on_created_by_id ON public.grants USING btree (created_by_id);

CREATE INDEX index_grants_on_gcp_auth_secret_id ON public.grants USING btree (gcp_auth_secret_id);

CREATE INDEX index_grants_on_gcp_id_token_secret_id ON public.grants USING btree (gcp_id_token_secret_id);

CREATE INDEX index_grants_on_hmac_secret_id ON public.grants USING btree (hmac_secret_id);

CREATE INDEX index_grants_on_oauth_token_secret_id ON public.grants USING btree (oauth_token_secret_id);

CREATE INDEX index_grants_on_pg_dsn_secret_id ON public.grants USING btree (pg_dsn_secret_id);

CREATE INDEX index_grants_on_principal_id ON public.grants USING btree (principal_id);

CREATE INDEX index_grants_on_role_id ON public.grants USING btree (role_id);

CREATE INDEX index_grants_on_ssh_key_secret_id ON public.grants USING btree (ssh_key_secret_id);

CREATE INDEX index_grants_on_static_secret_id ON public.grants USING btree (static_secret_id);

CREATE UNIQUE INDEX index_grants_uniq_principal_aws_auth_secret ON public.grants USING btree (principal_id, aws_auth_secret_id) WHERE ((principal_id IS NOT NULL) AND (aws_auth_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_principal_gcp_auth_secret ON public.grants USING btree (principal_id, gcp_auth_secret_id) WHERE ((principal_id IS NOT NULL) AND (gcp_auth_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_principal_hmac_secret ON public.grants USING btree (principal_id, hmac_secret_id) WHERE ((principal_id IS NOT NULL) AND (hmac_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_principal_oauth_token_secret ON public.grants USING btree (principal_id, oauth_token_secret_id) WHERE ((principal_id IS NOT NULL) AND (oauth_token_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_principal_pg_dsn_secret ON public.grants USING btree (principal_id, pg_dsn_secret_id) WHERE ((principal_id IS NOT NULL) AND (pg_dsn_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_principal_ssh_key_secret ON public.grants USING btree (principal_id, ssh_key_secret_id) WHERE ((principal_id IS NOT NULL) AND (ssh_key_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_principal_static_secret ON public.grants USING btree (principal_id, static_secret_id) WHERE ((principal_id IS NOT NULL) AND (static_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_role_aws_auth_secret ON public.grants USING btree (role_id, aws_auth_secret_id) WHERE ((role_id IS NOT NULL) AND (aws_auth_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_role_gcp_auth_secret ON public.grants USING btree (role_id, gcp_auth_secret_id) WHERE ((role_id IS NOT NULL) AND (gcp_auth_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_role_hmac_secret ON public.grants USING btree (role_id, hmac_secret_id) WHERE ((role_id IS NOT NULL) AND (hmac_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_role_oauth_token_secret ON public.grants USING btree (role_id, oauth_token_secret_id) WHERE ((role_id IS NOT NULL) AND (oauth_token_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_role_pg_dsn_secret ON public.grants USING btree (role_id, pg_dsn_secret_id) WHERE ((role_id IS NOT NULL) AND (pg_dsn_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_role_ssh_key_secret ON public.grants USING btree (role_id, ssh_key_secret_id) WHERE ((role_id IS NOT NULL) AND (ssh_key_secret_id IS NOT NULL));

CREATE UNIQUE INDEX index_grants_uniq_role_static_secret ON public.grants USING btree (role_id, static_secret_id) WHERE ((role_id IS NOT NULL) AND (static_secret_id IS NOT NULL));

CREATE INDEX index_hmac_secrets_on_created_by_id ON public.hmac_secrets USING btree (created_by_id);

CREATE UNIQUE INDEX index_hmac_secrets_on_foreign_id ON public.hmac_secrets USING btree (foreign_id);

CREATE INDEX index_hmac_secrets_on_labels ON public.hmac_secrets USING gin (labels);

CREATE UNIQUE INDEX index_mcp_oauth_authorization_codes_on_code_hash ON public.mcp_oauth_authorization_codes USING btree (code_hash);

CREATE INDEX index_mcp_oauth_authorization_codes_on_expires_at ON public.mcp_oauth_authorization_codes USING btree (expires_at);

CREATE INDEX index_mcp_oauth_authorization_codes_on_mcp_oauth_client_id ON public.mcp_oauth_authorization_codes USING btree (mcp_oauth_client_id);

CREATE INDEX index_mcp_oauth_authorization_codes_on_principal_id ON public.mcp_oauth_authorization_codes USING btree (principal_id);

CREATE INDEX index_mcp_oauth_authorization_codes_on_user_id ON public.mcp_oauth_authorization_codes USING btree (user_id);

CREATE INDEX index_mcp_oauth_refresh_tokens_on_expires_at ON public.mcp_oauth_refresh_tokens USING btree (expires_at);

CREATE INDEX index_mcp_oauth_refresh_tokens_on_mcp_oauth_client_id ON public.mcp_oauth_refresh_tokens USING btree (mcp_oauth_client_id);

CREATE INDEX index_mcp_oauth_refresh_tokens_on_principal_id ON public.mcp_oauth_refresh_tokens USING btree (principal_id);

CREATE UNIQUE INDEX index_mcp_oauth_refresh_tokens_on_token_hash ON public.mcp_oauth_refresh_tokens USING btree (token_hash);

CREATE INDEX index_mcp_oauth_refresh_tokens_on_user_id ON public.mcp_oauth_refresh_tokens USING btree (user_id);

CREATE INDEX index_oauth_apps_on_created_by_id ON public.oauth_apps USING btree (created_by_id);

CREATE INDEX index_oauth_apps_on_labels ON public.oauth_apps USING gin (labels);

CREATE UNIQUE INDEX index_oauth_apps_on_slug ON public.oauth_apps USING btree (slug);

CREATE INDEX index_oauth_token_secrets_on_created_by_id ON public.oauth_token_secrets USING btree (created_by_id);

CREATE UNIQUE INDEX index_oauth_token_secrets_on_foreign_id ON public.oauth_token_secrets USING btree (foreign_id);

CREATE INDEX index_oauth_token_secrets_on_labels ON public.oauth_token_secrets USING gin (labels);

CREATE INDEX index_pg_dsn_secrets_on_created_by_id ON public.pg_dsn_secrets USING btree (created_by_id);

CREATE UNIQUE INDEX index_pg_dsn_secrets_on_foreign_id ON public.pg_dsn_secrets USING btree (foreign_id);

CREATE INDEX index_pg_dsn_secrets_on_labels ON public.pg_dsn_secrets USING gin (labels);

CREATE INDEX index_principal_roles_on_principal_id ON public.principal_roles USING btree (principal_id);

CREATE UNIQUE INDEX index_principal_roles_on_principal_id_and_role_id ON public.principal_roles USING btree (principal_id, role_id);

CREATE INDEX index_principal_roles_on_role_id ON public.principal_roles USING btree (role_id);

CREATE INDEX index_principal_sync_config_snapshots_on_principal_id ON public.principal_sync_config_snapshots USING btree (principal_id);

CREATE INDEX index_principal_sync_config_snapshots_on_updated_at ON public.principal_sync_config_snapshots USING btree (updated_at);

CREATE INDEX index_principals_on_console_user_id ON public.principals USING btree (console_user_id);

CREATE INDEX index_principals_on_created_by_id ON public.principals USING btree (created_by_id);

CREATE UNIQUE INDEX index_principals_on_foreign_id ON public.principals USING btree (foreign_id);

CREATE INDEX index_principals_on_kind ON public.principals USING btree (kind);

CREATE INDEX index_principals_on_labels ON public.principals USING gin (labels);

CREATE INDEX index_principals_on_slack_channel_id ON public.principals USING btree (slack_channel_id);

CREATE INDEX index_principals_on_slack_email ON public.principals USING btree (slack_email);

CREATE INDEX index_principals_on_slack_team_id ON public.principals USING btree (slack_team_id);

CREATE INDEX index_principals_on_slack_user_id ON public.principals USING btree (slack_user_id);

CREATE INDEX index_proxies_on_labels ON public.proxies USING gin (labels);

CREATE INDEX index_proxies_on_principal_id ON public.proxies USING btree (principal_id);

CREATE INDEX index_proxies_on_requester_principal_id ON public.proxies USING btree (requester_principal_id);

CREATE INDEX index_request_rules_on_aws_auth_secret_id ON public.request_rules USING btree (aws_auth_secret_id);

CREATE INDEX index_request_rules_on_gcp_auth_secret_id ON public.request_rules USING btree (gcp_auth_secret_id);

CREATE INDEX index_request_rules_on_gcp_id_token_secret_id ON public.request_rules USING btree (gcp_id_token_secret_id);

CREATE INDEX index_request_rules_on_hmac_secret_id ON public.request_rules USING btree (hmac_secret_id);

CREATE INDEX index_request_rules_on_host ON public.request_rules USING btree (host);

CREATE INDEX index_request_rules_on_oauth_token_secret_id ON public.request_rules USING btree (oauth_token_secret_id);

CREATE INDEX index_request_rules_on_position ON public.request_rules USING btree ("position");

CREATE INDEX index_request_rules_on_static_secret_id ON public.request_rules USING btree (static_secret_id);

CREATE INDEX index_roles_on_created_by_id ON public.roles USING btree (created_by_id);

CREATE UNIQUE INDEX index_roles_on_foreign_id ON public.roles USING btree (foreign_id);

CREATE INDEX index_roles_on_labels ON public.roles USING gin (labels);

CREATE INDEX index_scheduled_tasks_on_author_id ON public.scheduled_tasks USING btree (author_id);

CREATE UNIQUE INDEX index_scheduled_tasks_on_author_id_and_name ON public.scheduled_tasks USING btree (author_id, name);

CREATE INDEX index_scheduled_tasks_on_enabled_and_next_run_at ON public.scheduled_tasks USING btree (enabled, next_run_at);

CREATE UNIQUE INDEX index_scheduled_tasks_on_principal_id_and_name ON public.scheduled_tasks USING btree (principal_id, name);

CREATE INDEX index_secret_sources_on_aws_auth_secret_id ON public.secret_sources USING btree (aws_auth_secret_id);

CREATE UNIQUE INDEX index_secret_sources_on_aws_owner_and_role ON public.secret_sources USING btree (aws_auth_secret_id, role, role_kind);

CREATE INDEX index_secret_sources_on_broker_credential_id ON public.secret_sources USING btree (broker_credential_id);

CREATE UNIQUE INDEX index_secret_sources_on_gcp_auth_secret_id ON public.secret_sources USING btree (gcp_auth_secret_id);

CREATE UNIQUE INDEX index_secret_sources_on_gcp_id_token_secret_id ON public.secret_sources USING btree (gcp_id_token_secret_id);

CREATE UNIQUE INDEX index_secret_sources_on_hmac_owner_and_role ON public.secret_sources USING btree (hmac_secret_id, role, role_kind);

CREATE INDEX index_secret_sources_on_hmac_secret_id ON public.secret_sources USING btree (hmac_secret_id);

CREATE UNIQUE INDEX index_secret_sources_on_oauth_owner_and_role ON public.secret_sources USING btree (oauth_token_secret_id, role, role_kind);

CREATE INDEX index_secret_sources_on_oauth_token_secret_id ON public.secret_sources USING btree (oauth_token_secret_id);

CREATE UNIQUE INDEX index_secret_sources_on_pg_dsn_secret_id ON public.secret_sources USING btree (pg_dsn_secret_id);

CREATE INDEX index_secret_sources_on_source_type ON public.secret_sources USING btree (source_type);

CREATE UNIQUE INDEX index_secret_sources_on_ssh_key_secret_id ON public.secret_sources USING btree (ssh_key_secret_id);

CREATE UNIQUE INDEX index_secret_sources_on_static_secret_id ON public.secret_sources USING btree (static_secret_id);

CREATE INDEX index_skill_editors_on_skill_id ON public.skill_editors USING btree (skill_id);

CREATE UNIQUE INDEX index_skill_editors_on_skill_id_and_user_id ON public.skill_editors USING btree (skill_id, user_id);

CREATE INDEX index_skill_editors_on_user_id ON public.skill_editors USING btree (user_id);

CREATE INDEX index_skills_on_search_vector ON public.skills USING gin (search_vector);

CREATE INDEX index_skills_on_user_id ON public.skills USING btree (user_id);

CREATE INDEX index_slack_bot_channels_for_catalog_search ON public.slack_bot_channels USING btree (team_id, active, name);

CREATE INDEX index_slack_bot_channels_on_member_user_ids ON public.slack_bot_channels USING gin (member_user_ids);

CREATE UNIQUE INDEX index_slack_bot_channels_on_team_id_and_channel_id ON public.slack_bot_channels USING btree (team_id, channel_id);

CREATE INDEX index_slack_channel_permissions_on_principal_id ON public.slack_channel_permissions USING btree (principal_id);

CREATE UNIQUE INDEX index_slack_dm_sync_cursors_on_oauth_app_slug ON public.slack_dm_sync_cursors USING btree (oauth_app_slug);

CREATE INDEX index_ssh_key_secrets_on_created_by_id ON public.ssh_key_secrets USING btree (created_by_id);

CREATE UNIQUE INDEX index_ssh_key_secrets_on_foreign_id ON public.ssh_key_secrets USING btree (foreign_id);

CREATE INDEX index_ssh_key_secrets_on_labels ON public.ssh_key_secrets USING gin (labels);

CREATE UNIQUE INDEX index_ssh_key_secrets_on_owner_id ON public.ssh_key_secrets USING btree (owner_id);

CREATE UNIQUE INDEX index_static_secrets_on_broker_credential_id ON public.static_secrets USING btree (broker_credential_id) WHERE (broker_credential_id IS NOT NULL);

CREATE INDEX index_static_secrets_on_created_by_id ON public.static_secrets USING btree (created_by_id);

CREATE UNIQUE INDEX index_static_secrets_on_foreign_id ON public.static_secrets USING btree (foreign_id);

CREATE INDEX index_static_secrets_on_labels ON public.static_secrets USING gin (labels);

CREATE UNIQUE INDEX index_system_settings_on_singleton ON public.system_settings USING btree (singleton);

CREATE INDEX index_thread_shares_on_created_by_id ON public.thread_shares USING btree (created_by_id);

CREATE UNIQUE INDEX index_thread_shares_on_thread_key ON public.thread_shares USING btree (thread_key);

CREATE UNIQUE INDEX index_user_identities_on_provider_and_subject ON public.user_identities USING btree (provider, subject);

CREATE INDEX index_user_identities_on_user_id ON public.user_identities USING btree (user_id);

CREATE INDEX index_users_on_approved_by_id ON public.users USING btree (approved_by_id);

CREATE UNIQUE INDEX index_users_on_email ON public.users USING btree (email);

ALTER TABLE ONLY public.proxies
    ADD CONSTRAINT fk_rails_0249dfcf26 FOREIGN KEY (principal_id) REFERENCES public.principals(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.oauth_apps
    ADD CONSTRAINT fk_rails_0c732e29b9 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_121a65684e FOREIGN KEY (pg_dsn_secret_id) REFERENCES public.pg_dsn_secrets(id);

ALTER TABLE ONLY public.principal_sync_config_snapshots
    ADD CONSTRAINT fk_rails_24cd97e7f7 FOREIGN KEY (principal_id) REFERENCES public.principals(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.proxies
    ADD CONSTRAINT fk_rails_2687c74a2d FOREIGN KEY (requester_principal_id) REFERENCES public.principals(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.skill_editors
    ADD CONSTRAINT fk_rails_29394caa42 FOREIGN KEY (skill_id) REFERENCES public.skills(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.principal_roles
    ADD CONSTRAINT fk_rails_2b32784326 FOREIGN KEY (principal_id) REFERENCES public.principals(id);

ALTER TABLE ONLY public.scheduled_tasks
    ADD CONSTRAINT fk_rails_2e60f66b0e FOREIGN KEY (principal_id) REFERENCES public.principals(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.principal_roles
    ADD CONSTRAINT fk_rails_311fdbffc1 FOREIGN KEY (role_id) REFERENCES public.roles(id);

ALTER TABLE ONLY public.hmac_secrets
    ADD CONSTRAINT fk_rails_315f80fdf9 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.api_keys
    ADD CONSTRAINT fk_rails_32c28d0dc2 FOREIGN KEY (user_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.mcp_oauth_authorization_codes
    ADD CONSTRAINT fk_rails_35c88d1d71 FOREIGN KEY (user_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.ssh_key_secrets
    ADD CONSTRAINT fk_rails_35e6b63ea0 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_40e2a762e1 FOREIGN KEY (gcp_auth_secret_id) REFERENCES public.gcp_auth_secrets(id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_431f3f478c FOREIGN KEY (aws_auth_secret_id) REFERENCES public.aws_auth_secrets(id);

ALTER TABLE ONLY public.mcp_oauth_refresh_tokens
    ADD CONSTRAINT fk_rails_4e98ce39f4 FOREIGN KEY (principal_id) REFERENCES public.principals(id);

ALTER TABLE ONLY public.static_secrets
    ADD CONSTRAINT fk_rails_5b9195e659 FOREIGN KEY (broker_credential_id) REFERENCES public.broker_credentials(id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_6040cb3833 FOREIGN KEY (ssh_key_secret_id) REFERENCES public.ssh_key_secrets(id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_65c1988a7d FOREIGN KEY (hmac_secret_id) REFERENCES public.hmac_secrets(id);

ALTER TABLE ONLY public.secret_sources
    ADD CONSTRAINT fk_rails_668a6af06d FOREIGN KEY (hmac_secret_id) REFERENCES public.hmac_secrets(id);

ALTER TABLE ONLY public.user_identities
    ADD CONSTRAINT fk_rails_684b0e1ce0 FOREIGN KEY (user_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.request_rules
    ADD CONSTRAINT fk_rails_69446ec607 FOREIGN KEY (oauth_token_secret_id) REFERENCES public.oauth_token_secrets(id);

ALTER TABLE ONLY public.oauth_token_secrets
    ADD CONSTRAINT fk_rails_72dd4117e4 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.pg_dsn_secrets
    ADD CONSTRAINT fk_rails_7904547f12 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.mcp_oauth_authorization_codes
    ADD CONSTRAINT fk_rails_79417c451c FOREIGN KEY (principal_id) REFERENCES public.principals(id);

ALTER TABLE ONLY public.secret_sources
    ADD CONSTRAINT fk_rails_7a590ca964 FOREIGN KEY (broker_credential_id) REFERENCES public.broker_credentials(id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_7bfa0edf5a FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.principals
    ADD CONSTRAINT fk_rails_7e008279c6 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.secret_sources
    ADD CONSTRAINT fk_rails_7e69f0a486 FOREIGN KEY (ssh_key_secret_id) REFERENCES public.ssh_key_secrets(id);

ALTER TABLE ONLY public.request_rules
    ADD CONSTRAINT fk_rails_7fc7100131 FOREIGN KEY (gcp_id_token_secret_id) REFERENCES public.gcp_id_token_secrets(id);

ALTER TABLE ONLY public.request_rules
    ADD CONSTRAINT fk_rails_7ffd24332a FOREIGN KEY (aws_auth_secret_id) REFERENCES public.aws_auth_secrets(id);

ALTER TABLE ONLY public.gcp_id_token_secrets
    ADD CONSTRAINT fk_rails_82cbf74a22 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.skill_editors
    ADD CONSTRAINT fk_rails_877359093c FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.principals
    ADD CONSTRAINT fk_rails_8906815923 FOREIGN KEY (console_user_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.api_clients
    ADD CONSTRAINT fk_rails_8d552db7a9 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.ssh_key_secrets
    ADD CONSTRAINT fk_rails_8daf98de8d FOREIGN KEY (owner_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.request_rules
    ADD CONSTRAINT fk_rails_8f1359f70b FOREIGN KEY (static_secret_id) REFERENCES public.static_secrets(id);

ALTER TABLE ONLY public.thread_shares
    ADD CONSTRAINT fk_rails_95b08f75a8 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.api_client_principals
    ADD CONSTRAINT fk_rails_9aeeaf3eac FOREIGN KEY (principal_id) REFERENCES public.principals(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.users
    ADD CONSTRAINT fk_rails_9c2d7e0c9d FOREIGN KEY (approved_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.secret_sources
    ADD CONSTRAINT fk_rails_9e8ad7acb6 FOREIGN KEY (pg_dsn_secret_id) REFERENCES public.pg_dsn_secrets(id);

ALTER TABLE ONLY public.broker_credentials
    ADD CONSTRAINT fk_rails_a262ab2994 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_a6619a068a FOREIGN KEY (oauth_token_secret_id) REFERENCES public.oauth_token_secrets(id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_a6a4a8fae5 FOREIGN KEY (gcp_id_token_secret_id) REFERENCES public.gcp_id_token_secrets(id);

ALTER TABLE ONLY public.broker_credentials
    ADD CONSTRAINT fk_rails_aa788a2818 FOREIGN KEY (oauth_app_id) REFERENCES public.oauth_apps(id);

ALTER TABLE ONLY public.scheduled_tasks
    ADD CONSTRAINT fk_rails_b1ee27eb77 FOREIGN KEY (author_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.secret_sources
    ADD CONSTRAINT fk_rails_b3affc42c6 FOREIGN KEY (oauth_token_secret_id) REFERENCES public.oauth_token_secrets(id);

ALTER TABLE ONLY public.roles
    ADD CONSTRAINT fk_rails_b41292c88f FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.slack_channel_permissions
    ADD CONSTRAINT fk_rails_b4f21843d1 FOREIGN KEY (principal_id) REFERENCES public.principals(id);

ALTER TABLE ONLY public.request_rules
    ADD CONSTRAINT fk_rails_b6a1124766 FOREIGN KEY (gcp_auth_secret_id) REFERENCES public.gcp_auth_secrets(id);

ALTER TABLE ONLY public.static_secrets
    ADD CONSTRAINT fk_rails_bc4c3626b5 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_d0310fc055 FOREIGN KEY (principal_id) REFERENCES public.principals(id);

ALTER TABLE ONLY public.secret_sources
    ADD CONSTRAINT fk_rails_d394328dc5 FOREIGN KEY (gcp_auth_secret_id) REFERENCES public.gcp_auth_secrets(id);

ALTER TABLE ONLY public.api_client_principals
    ADD CONSTRAINT fk_rails_dbf3ab030a FOREIGN KEY (api_client_id) REFERENCES public.api_clients(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.slack_channel_permissions
    ADD CONSTRAINT fk_rails_deca4f5e0e FOREIGN KEY (role_id) REFERENCES public.roles(id);

ALTER TABLE ONLY public.mcp_oauth_refresh_tokens
    ADD CONSTRAINT fk_rails_e12f95ca21 FOREIGN KEY (mcp_oauth_client_id) REFERENCES public.mcp_oauth_clients(id);

ALTER TABLE ONLY public.secret_sources
    ADD CONSTRAINT fk_rails_e20a7e8e88 FOREIGN KEY (static_secret_id) REFERENCES public.static_secrets(id);

ALTER TABLE ONLY public.secret_sources
    ADD CONSTRAINT fk_rails_ea35bb0efc FOREIGN KEY (aws_auth_secret_id) REFERENCES public.aws_auth_secrets(id);

ALTER TABLE ONLY public.skills
    ADD CONSTRAINT fk_rails_ee47b87d76 FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_f20ac87bff FOREIGN KEY (static_secret_id) REFERENCES public.static_secrets(id);

ALTER TABLE ONLY public.mcp_oauth_refresh_tokens
    ADD CONSTRAINT fk_rails_f2f9692a8e FOREIGN KEY (user_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.gcp_auth_secrets
    ADD CONSTRAINT fk_rails_f52493e07c FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.aws_auth_secrets
    ADD CONSTRAINT fk_rails_f59f8ceaa7 FOREIGN KEY (created_by_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.request_rules
    ADD CONSTRAINT fk_rails_f6134f4c3c FOREIGN KEY (hmac_secret_id) REFERENCES public.hmac_secrets(id);

ALTER TABLE ONLY public.grants
    ADD CONSTRAINT fk_rails_f76a299f8d FOREIGN KEY (role_id) REFERENCES public.roles(id);

ALTER TABLE ONLY public.mcp_oauth_authorization_codes
    ADD CONSTRAINT fk_rails_fa3183ab35 FOREIGN KEY (mcp_oauth_client_id) REFERENCES public.mcp_oauth_clients(id);

ALTER TABLE ONLY public.secret_sources
    ADD CONSTRAINT fk_rails_fec4e94bcd FOREIGN KEY (gcp_id_token_secret_id) REFERENCES public.gcp_id_token_secrets(id);

INSERT INTO public.schema_migrations (version) VALUES
    ('20260527035805'),
    ('20260527044342'),
    ('20260527044343'),
    ('20260527171520'),
    ('20260527171532'),
    ('20260527171533'),
    ('20260527182421'),
    ('20260527194107'),
    ('20260527195243'),
    ('20260527195244'),
    ('20260527210955'),
    ('20260527211535'),
    ('20260527220000'),
    ('20260527220001'),
    ('20260528111433'),
    ('20260601174019'),
    ('20260601174020'),
    ('20260601174023'),
    ('20260601174024'),
    ('20260601174025'),
    ('20260602041000'),
    ('20260602041005'),
    ('20260602041006'),
    ('20260602050000'),
    ('20260602060000'),
    ('20260602060001'),
    ('20260602060002'),
    ('20260603044111'),
    ('20260603120000'),
    ('20260603120001'),
    ('20260603130000'),
    ('20260604120000'),
    ('20260605155736'),
    ('20260608210001'),
    ('20260608210002'),
    ('20260611044016'),
    ('20260611120000'),
    ('20260611120100'),
    ('20260611130000'),
    ('20260612100000'),
    ('20260612100100'),
    ('20260615224547'),
    ('20260616170000'),
    ('20260623231029'),
    ('20260623231036'),
    ('20260624000100'),
    ('20260625002334'),
    ('20260625030000'),
    ('20260630090000'),
    ('20260630090001'),
    ('20260630090002'),
    ('20260702000000'),
    ('20260707190000'),
    ('20260709160000'),
    ('20260709174916'),
    ('20260709223000'),
    ('20260711182055'),
    ('20260711190035'),
    ('20260714175437'),
    ('20260721213228'),
    ('20260727183832'),
    ('20260727233739'),
    ('20260731120000'),
    ('20260802022347'),
    ('20260802170149'),
    ('20260806190000'),
    ('20260806190100'),
    ('20260806190200'),
    ('20260807091441'),
    ('20260810120000'),
    ('20260810194508'),
    ('20260811120000'),
    ('20260811200000'),
    ('20260812042212'),
    ('20260813181200'),
    ('20260813182623'),
    ('20260813212224'),
    ('20260814180000'),
    ('20260820174601'),
    ('20260820191500'),
    ('20260823054500'),
    ('20260916152611'),
    ('20260924155807'),
    ('20260929224308'),
    ('20261008004411'),
    ('20261010025431'),
    ('20261104232304'),
    ('20261106204641'),
    ('20261106212027'),
    ('20261109021725'),
    ('20261110220120'),
    ('20261112120000');
