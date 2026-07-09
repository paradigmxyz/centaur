# This file is auto-generated from the current state of the database. Instead
# of editing this file, please use the migrations feature of Active Record to
# incrementally modify your database, and then regenerate this schema definition.
#
# This file is the source Rails uses to define your schema when running `bin/rails
# db:schema:load`. When creating a new database, `bin/rails db:schema:load` tends to
# be faster and is potentially less error prone than running all of your
# migrations from scratch. Old migrations may fail to apply correctly if those
# migrations use external dependencies or application code.
#
# It's strongly recommended that you check this file into your version control system.

ActiveRecord::Schema[8.1].define(version: 2026_07_09_223000) do
  # These are extensions that must be enabled in order to support this database
  enable_extension "pg_catalog.plpgsql"

  create_table "api_keys", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.datetime "deleted_at"
    t.string "name", null: false
    t.string "token_hash", null: false
    t.datetime "updated_at", null: false
    t.bigint "user_id", null: false
    t.index ["deleted_at"], name: "index_api_keys_on_deleted_at"
    t.index ["token_hash"], name: "index_api_keys_on_token_hash", unique: true
    t.index ["user_id"], name: "index_api_keys_on_user_id"
  end

  create_table "aws_auth_secrets", force: :cascade do |t|
    t.jsonb "allowed_regions", default: [], null: false
    t.jsonb "allowed_services", default: [], null: false
    t.datetime "created_at", null: false
    t.bigint "created_by_id", null: false
    t.string "description"
    t.string "foreign_id"
    t.jsonb "labels", default: {}, null: false
    t.string "name"
    t.string "namespace", default: "default", null: false
    t.datetime "updated_at", null: false
    t.index ["created_by_id"], name: "index_aws_auth_secrets_on_created_by_id"
    t.index ["labels"], name: "index_aws_auth_secrets_on_labels", using: :gin
    t.index ["namespace", "foreign_id"], name: "index_aws_auth_secrets_on_namespace_and_foreign_id", unique: true
  end

  create_table "broker_credentials", force: :cascade do |t|
    t.text "access_token"
    t.text "api_key"
    t.string "client_id"
    t.text "client_secret"
    t.datetime "created_at", null: false
    t.bigint "created_by_id"
    t.boolean "dead", default: false, null: false
    t.string "dead_reason"
    t.string "description"
    t.float "early_refresh_fraction", default: 0.2, null: false
    t.integer "early_refresh_slack_seconds", default: 300, null: false
    t.datetime "expires_at"
    t.string "external_user_key"
    t.integer "failure_count", default: 0, null: false
    t.string "foreign_id"
    t.string "grant", default: "refresh_token", null: false
    t.jsonb "labels", default: {}, null: false
    t.datetime "last_refresh"
    t.integer "max_refresh_interval_seconds", default: 86400, null: false
    t.string "name"
    t.string "namespace", default: "default", null: false
    t.datetime "next_attempt_at"
    t.bigint "oauth_app_id"
    t.text "password"
    t.string "provider_email"
    t.string "provider_subject"
    t.integer "refresh_timeout_seconds", default: 30, null: false
    t.text "refresh_token"
    t.jsonb "scopes", default: [], null: false
    t.string "token_endpoint", null: false
    t.text "token_endpoint_headers"
    t.datetime "updated_at", null: false
    t.text "username"
    t.index ["created_by_id"], name: "index_broker_credentials_on_created_by_id"
    t.index ["labels"], name: "index_broker_credentials_on_labels", using: :gin
    t.index ["namespace", "foreign_id"], name: "index_broker_credentials_on_namespace_and_foreign_id", unique: true
    t.index ["next_attempt_at"], name: "index_broker_credentials_on_next_attempt_at"
    t.index ["oauth_app_id", "provider_subject"], name: "index_broker_credentials_on_oauth_app_id_and_provider_subject", unique: true, where: "(provider_subject IS NOT NULL)"
    t.index ["oauth_app_id"], name: "index_broker_credentials_on_oauth_app_id"
  end

  create_table "gcp_auth_secrets", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.bigint "created_by_id", null: false
    t.jsonb "credentials_provider"
    t.string "description"
    t.string "foreign_id"
    t.jsonb "labels", default: {}, null: false
    t.string "name"
    t.string "namespace", default: "default", null: false
    t.jsonb "scopes", default: [], null: false
    t.string "subject"
    t.datetime "updated_at", null: false
    t.index ["created_by_id"], name: "index_gcp_auth_secrets_on_created_by_id"
    t.index ["labels"], name: "index_gcp_auth_secrets_on_labels", using: :gin
    t.index ["namespace", "foreign_id"], name: "index_gcp_auth_secrets_on_namespace_and_foreign_id", unique: true
  end

  create_table "gcp_id_token_secrets", force: :cascade do |t|
    t.string "audience", null: false
    t.datetime "created_at", null: false
    t.bigint "created_by_id", null: false
    t.string "description"
    t.string "foreign_id"
    t.string "header"
    t.jsonb "labels", default: {}, null: false
    t.string "name"
    t.string "namespace", default: "default", null: false
    t.datetime "updated_at", null: false
    t.index ["created_by_id"], name: "index_gcp_id_token_secrets_on_created_by_id"
    t.index ["labels"], name: "index_gcp_id_token_secrets_on_labels", using: :gin
    t.index ["namespace", "foreign_id"], name: "index_gcp_id_token_secrets_on_namespace_and_foreign_id", unique: true
  end

  create_table "grants", force: :cascade do |t|
    t.bigint "aws_auth_secret_id"
    t.datetime "created_at", null: false
    t.bigint "created_by_id", null: false
    t.bigint "gcp_auth_secret_id"
    t.bigint "gcp_id_token_secret_id"
    t.bigint "hmac_secret_id"
    t.bigint "oauth_token_secret_id"
    t.bigint "pg_dsn_secret_id"
    t.bigint "principal_id"
    t.integer "priority", null: false
    t.bigint "role_id"
    t.bigint "static_secret_id"
    t.datetime "updated_at", null: false
    t.index ["aws_auth_secret_id"], name: "index_grants_on_aws_auth_secret_id"
    t.index ["created_by_id"], name: "index_grants_on_created_by_id"
    t.index ["gcp_auth_secret_id"], name: "index_grants_on_gcp_auth_secret_id"
    t.index ["gcp_id_token_secret_id"], name: "index_grants_on_gcp_id_token_secret_id"
    t.index ["hmac_secret_id"], name: "index_grants_on_hmac_secret_id"
    t.index ["oauth_token_secret_id"], name: "index_grants_on_oauth_token_secret_id"
    t.index ["pg_dsn_secret_id"], name: "index_grants_on_pg_dsn_secret_id"
    t.index ["principal_id", "aws_auth_secret_id"], name: "index_grants_uniq_principal_aws_auth_secret", unique: true, where: "((principal_id IS NOT NULL) AND (aws_auth_secret_id IS NOT NULL))"
    t.index ["principal_id", "gcp_auth_secret_id"], name: "index_grants_uniq_principal_gcp_auth_secret", unique: true, where: "((principal_id IS NOT NULL) AND (gcp_auth_secret_id IS NOT NULL))"
    t.index ["principal_id", "hmac_secret_id"], name: "index_grants_uniq_principal_hmac_secret", unique: true, where: "((principal_id IS NOT NULL) AND (hmac_secret_id IS NOT NULL))"
    t.index ["principal_id", "oauth_token_secret_id"], name: "index_grants_uniq_principal_oauth_token_secret", unique: true, where: "((principal_id IS NOT NULL) AND (oauth_token_secret_id IS NOT NULL))"
    t.index ["principal_id", "pg_dsn_secret_id"], name: "index_grants_uniq_principal_pg_dsn_secret", unique: true, where: "((principal_id IS NOT NULL) AND (pg_dsn_secret_id IS NOT NULL))"
    t.index ["principal_id", "static_secret_id"], name: "index_grants_uniq_principal_static_secret", unique: true, where: "((principal_id IS NOT NULL) AND (static_secret_id IS NOT NULL))"
    t.index ["principal_id"], name: "index_grants_on_principal_id"
    t.index ["role_id", "aws_auth_secret_id"], name: "index_grants_uniq_role_aws_auth_secret", unique: true, where: "((role_id IS NOT NULL) AND (aws_auth_secret_id IS NOT NULL))"
    t.index ["role_id", "gcp_auth_secret_id"], name: "index_grants_uniq_role_gcp_auth_secret", unique: true, where: "((role_id IS NOT NULL) AND (gcp_auth_secret_id IS NOT NULL))"
    t.index ["role_id", "hmac_secret_id"], name: "index_grants_uniq_role_hmac_secret", unique: true, where: "((role_id IS NOT NULL) AND (hmac_secret_id IS NOT NULL))"
    t.index ["role_id", "oauth_token_secret_id"], name: "index_grants_uniq_role_oauth_token_secret", unique: true, where: "((role_id IS NOT NULL) AND (oauth_token_secret_id IS NOT NULL))"
    t.index ["role_id", "pg_dsn_secret_id"], name: "index_grants_uniq_role_pg_dsn_secret", unique: true, where: "((role_id IS NOT NULL) AND (pg_dsn_secret_id IS NOT NULL))"
    t.index ["role_id", "static_secret_id"], name: "index_grants_uniq_role_static_secret", unique: true, where: "((role_id IS NOT NULL) AND (static_secret_id IS NOT NULL))"
    t.index ["role_id"], name: "index_grants_on_role_id"
    t.index ["static_secret_id"], name: "index_grants_on_static_secret_id"
  end

  create_table "hmac_secrets", force: :cascade do |t|
    t.boolean "allow_chunked_body", default: false, null: false
    t.datetime "created_at", null: false
    t.bigint "created_by_id", null: false
    t.string "description"
    t.string "foreign_id"
    t.jsonb "headers", default: [], null: false
    t.jsonb "labels", default: {}, null: false
    t.string "name"
    t.string "namespace", default: "default", null: false
    t.string "signature_algorithm"
    t.string "signature_key_encoding"
    t.text "signature_message"
    t.string "signature_output_encoding"
    t.string "timestamp_format"
    t.datetime "updated_at", null: false
    t.index ["created_by_id"], name: "index_hmac_secrets_on_created_by_id"
    t.index ["labels"], name: "index_hmac_secrets_on_labels", using: :gin
    t.index ["namespace", "foreign_id"], name: "index_hmac_secrets_on_namespace_and_foreign_id", unique: true
  end

  create_table "mcp_oauth_authorization_codes", force: :cascade do |t|
    t.string "code_challenge", null: false
    t.string "code_hash", null: false
    t.datetime "consumed_at"
    t.datetime "created_at", null: false
    t.datetime "expires_at", null: false
    t.bigint "mcp_oauth_client_id", null: false
    t.bigint "principal_id", null: false
    t.string "redirect_uri", null: false
    t.string "resource", null: false
    t.jsonb "scopes", default: [], null: false
    t.datetime "updated_at", null: false
    t.bigint "user_id", null: false
    t.index ["code_hash"], name: "index_mcp_oauth_authorization_codes_on_code_hash", unique: true
    t.index ["expires_at"], name: "index_mcp_oauth_authorization_codes_on_expires_at"
    t.index ["mcp_oauth_client_id"], name: "index_mcp_oauth_authorization_codes_on_mcp_oauth_client_id"
    t.index ["principal_id"], name: "index_mcp_oauth_authorization_codes_on_principal_id"
    t.index ["user_id"], name: "index_mcp_oauth_authorization_codes_on_user_id"
  end

  create_table "mcp_oauth_clients", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.jsonb "grant_types", default: [], null: false
    t.datetime "last_used_at"
    t.jsonb "metadata", default: {}, null: false
    t.string "name"
    t.jsonb "redirect_uris", default: [], null: false
    t.jsonb "response_types", default: [], null: false
    t.jsonb "scopes", default: [], null: false
    t.datetime "updated_at", null: false
  end

  create_table "mcp_oauth_refresh_tokens", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.datetime "expires_at", null: false
    t.datetime "last_used_at"
    t.bigint "mcp_oauth_client_id", null: false
    t.bigint "principal_id", null: false
    t.string "resource", null: false
    t.datetime "revoked_at"
    t.jsonb "scopes", default: [], null: false
    t.string "token_hash", null: false
    t.datetime "updated_at", null: false
    t.bigint "user_id", null: false
    t.index ["expires_at"], name: "index_mcp_oauth_refresh_tokens_on_expires_at"
    t.index ["mcp_oauth_client_id"], name: "index_mcp_oauth_refresh_tokens_on_mcp_oauth_client_id"
    t.index ["principal_id"], name: "index_mcp_oauth_refresh_tokens_on_principal_id"
    t.index ["token_hash"], name: "index_mcp_oauth_refresh_tokens_on_token_hash", unique: true
    t.index ["user_id"], name: "index_mcp_oauth_refresh_tokens_on_user_id"
  end

  create_table "oauth_apps", force: :cascade do |t|
    t.jsonb "allowed_scopes", default: [], null: false
    t.string "client_id", null: false
    t.text "client_secret"
    t.datetime "created_at", null: false
    t.bigint "created_by_id", null: false
    t.string "credential_namespace", default: "default", null: false
    t.string "description"
    t.boolean "enabled", default: true, null: false
    t.jsonb "labels", default: {}, null: false
    t.string "provider", null: false
    t.string "slug", null: false
    t.datetime "updated_at", null: false
    t.index ["created_by_id"], name: "index_oauth_apps_on_created_by_id"
    t.index ["labels"], name: "index_oauth_apps_on_labels", using: :gin
    t.index ["slug"], name: "index_oauth_apps_on_slug", unique: true
  end

  create_table "oauth_token_secrets", force: :cascade do |t|
    t.string "audience"
    t.datetime "created_at", null: false
    t.bigint "created_by_id", null: false
    t.string "description"
    t.string "foreign_id"
    t.string "grant"
    t.string "header"
    t.jsonb "labels", default: {}, null: false
    t.string "name"
    t.string "namespace", default: "default", null: false
    t.jsonb "scopes", default: [], null: false
    t.string "token_endpoint"
    t.datetime "updated_at", null: false
    t.string "value_prefix"
    t.index ["created_by_id"], name: "index_oauth_token_secrets_on_created_by_id"
    t.index ["labels"], name: "index_oauth_token_secrets_on_labels", using: :gin
    t.index ["namespace", "foreign_id"], name: "index_oauth_token_secrets_on_namespace_and_foreign_id", unique: true
  end

  create_table "pg_dsn_secrets", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.bigint "created_by_id", null: false
    t.string "database", null: false
    t.string "description"
    t.string "foreign_id"
    t.jsonb "labels", default: {}, null: false
    t.string "name"
    t.string "namespace", default: "default", null: false
    t.string "role"
    t.jsonb "settings", default: [], null: false
    t.datetime "updated_at", null: false
    t.index ["created_by_id"], name: "index_pg_dsn_secrets_on_created_by_id"
    t.index ["labels"], name: "index_pg_dsn_secrets_on_labels", using: :gin
    t.index ["namespace", "foreign_id"], name: "index_pg_dsn_secrets_on_namespace_and_foreign_id", unique: true
  end

  create_table "principal_roles", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.bigint "principal_id", null: false
    t.bigint "role_id", null: false
    t.datetime "updated_at", null: false
    t.index ["principal_id", "role_id"], name: "index_principal_roles_on_principal_id_and_role_id", unique: true
    t.index ["principal_id"], name: "index_principal_roles_on_principal_id"
    t.index ["role_id"], name: "index_principal_roles_on_role_id"
  end

  create_table "slack_channel_permissions", force: :cascade do |t|
    t.string "channel_id", null: false
    t.string "channel_name"
    t.datetime "created_at", null: false
    t.boolean "download_enabled", default: false, null: false
    t.boolean "history_enabled", default: false, null: false
    t.bigint "principal_id", null: false
    t.datetime "updated_at", null: false
    t.boolean "upload_enabled", default: false, null: false
    t.index ["principal_id", "channel_id"], name: "idx_on_principal_id_channel_id_53e63fc3fb", unique: true
    t.index ["principal_id"], name: "index_slack_channel_permissions_on_principal_id"
  end

  create_table "principal_sync_config_snapshots", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.text "payload", null: false
    t.bigint "principal_cache_version", null: false
    t.bigint "principal_id", null: false
    t.datetime "updated_at", null: false
    t.index ["principal_id", "principal_cache_version"], name: "idx_principal_sync_snapshots_on_principal_version", unique: true
    t.index ["principal_id"], name: "index_principal_sync_config_snapshots_on_principal_id"
    t.index ["updated_at"], name: "index_principal_sync_config_snapshots_on_updated_at"
  end

  create_table "principals", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.bigint "created_by_id", null: false
    t.string "foreign_id"
    t.jsonb "labels", default: {}, null: false
    t.string "name"
    t.string "namespace", default: "default", null: false
    t.boolean "sandbox_api_server_enabled", default: true, null: false
    t.boolean "sandbox_observability_enabled", default: true, null: false
    t.boolean "sandbox_repo_cache_enabled", default: true, null: false
    t.bigint "sync_config_cache_version", default: 0, null: false
    t.datetime "updated_at", null: false
    t.index ["created_by_id"], name: "index_principals_on_created_by_id"
    t.index ["labels"], name: "index_principals_on_labels", using: :gin
    t.index ["namespace", "foreign_id"], name: "index_principals_on_namespace_and_foreign_id", unique: true
  end

  create_table "proxies", force: :cascade do |t|
    t.string "bearer_token_hash", null: false
    t.datetime "created_at", null: false
    t.string "name", null: false
    t.datetime "principal_assigned_at"
    t.bigint "principal_id"
    t.datetime "updated_at", null: false
    t.index ["principal_id"], name: "index_proxies_on_principal_id"
  end

  create_table "request_rules", force: :cascade do |t|
    t.bigint "aws_auth_secret_id"
    t.string "cidr"
    t.datetime "created_at", null: false
    t.bigint "gcp_auth_secret_id"
    t.bigint "gcp_id_token_secret_id"
    t.bigint "hmac_secret_id"
    t.string "host"
    t.jsonb "http_methods", default: [], null: false
    t.bigint "oauth_token_secret_id"
    t.jsonb "paths", default: [], null: false
    t.integer "position", default: 0, null: false
    t.bigint "static_secret_id"
    t.datetime "updated_at", null: false
    t.index ["aws_auth_secret_id"], name: "index_request_rules_on_aws_auth_secret_id"
    t.index ["gcp_auth_secret_id"], name: "index_request_rules_on_gcp_auth_secret_id"
    t.index ["gcp_id_token_secret_id"], name: "index_request_rules_on_gcp_id_token_secret_id"
    t.index ["hmac_secret_id"], name: "index_request_rules_on_hmac_secret_id"
    t.index ["host"], name: "index_request_rules_on_host"
    t.index ["oauth_token_secret_id"], name: "index_request_rules_on_oauth_token_secret_id"
    t.index ["position"], name: "index_request_rules_on_position"
    t.index ["static_secret_id"], name: "index_request_rules_on_static_secret_id"
  end

  create_table "roles", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.bigint "created_by_id", null: false
    t.string "foreign_id"
    t.jsonb "labels", default: {}, null: false
    t.string "name"
    t.string "namespace", default: "default", null: false
    t.datetime "updated_at", null: false
    t.index ["created_by_id"], name: "index_roles_on_created_by_id"
    t.index ["labels"], name: "index_roles_on_labels", using: :gin
    t.index ["namespace", "foreign_id"], name: "index_roles_on_namespace_and_foreign_id", unique: true
  end

  create_table "secret_sources", force: :cascade do |t|
    t.bigint "aws_auth_secret_id"
    t.jsonb "config", default: {}, null: false
    t.datetime "created_at", null: false
    t.bigint "gcp_auth_secret_id"
    t.bigint "gcp_id_token_secret_id"
    t.bigint "hmac_secret_id"
    t.bigint "oauth_token_secret_id"
    t.bigint "pg_dsn_secret_id"
    t.string "role"
    t.string "role_kind"
    t.text "secret"
    t.string "source_type", null: false
    t.bigint "static_secret_id"
    t.datetime "updated_at", null: false
    t.index ["aws_auth_secret_id", "role", "role_kind"], name: "index_secret_sources_on_aws_owner_and_role", unique: true
    t.index ["aws_auth_secret_id"], name: "index_secret_sources_on_aws_auth_secret_id"
    t.index ["gcp_auth_secret_id"], name: "index_secret_sources_on_gcp_auth_secret_id", unique: true
    t.index ["gcp_id_token_secret_id"], name: "index_secret_sources_on_gcp_id_token_secret_id", unique: true
    t.index ["hmac_secret_id", "role", "role_kind"], name: "index_secret_sources_on_hmac_owner_and_role", unique: true
    t.index ["hmac_secret_id"], name: "index_secret_sources_on_hmac_secret_id"
    t.index ["oauth_token_secret_id", "role", "role_kind"], name: "index_secret_sources_on_oauth_owner_and_role", unique: true
    t.index ["oauth_token_secret_id"], name: "index_secret_sources_on_oauth_token_secret_id"
    t.index ["pg_dsn_secret_id"], name: "index_secret_sources_on_pg_dsn_secret_id", unique: true
    t.index ["source_type"], name: "index_secret_sources_on_source_type"
    t.index ["static_secret_id"], name: "index_secret_sources_on_static_secret_id", unique: true
  end

  create_table "static_secrets", force: :cascade do |t|
    t.bigint "broker_credential_id"
    t.datetime "created_at", null: false
    t.bigint "created_by_id"
    t.string "description"
    t.string "foreign_id"
    t.jsonb "inject_config"
    t.jsonb "labels", default: {}, null: false
    t.string "name"
    t.string "namespace", default: "default", null: false
    t.jsonb "replace_config"
    t.datetime "updated_at", null: false
    t.index ["broker_credential_id"], name: "index_static_secrets_on_broker_credential_id", unique: true, where: "(broker_credential_id IS NOT NULL)"
    t.index ["created_by_id"], name: "index_static_secrets_on_created_by_id"
    t.index ["labels"], name: "index_static_secrets_on_labels", using: :gin
    t.index ["namespace", "foreign_id"], name: "index_static_secrets_on_namespace_and_foreign_id", unique: true
  end

  create_table "user_identities", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.string "email"
    t.boolean "email_verified", default: false, null: false
    t.string "provider", null: false
    t.string "subject", null: false
    t.string "team_id"
    t.datetime "updated_at", null: false
    t.bigint "user_id", null: false
    t.index ["provider", "subject"], name: "index_user_identities_on_provider_and_subject", unique: true
    t.index ["user_id"], name: "index_user_identities_on_user_id"
  end

  create_table "users", force: :cascade do |t|
    t.boolean "admin", default: false, null: false
    t.datetime "approved_at"
    t.bigint "approved_by_id"
    t.datetime "created_at", null: false
    t.string "email", null: false
    t.string "name"
    t.string "password_digest"
    t.string "status", default: "pending", null: false
    t.datetime "updated_at", null: false
    t.index ["approved_by_id"], name: "index_users_on_approved_by_id"
    t.index ["email"], name: "index_users_on_email", unique: true
  end

  add_foreign_key "api_keys", "users"
  add_foreign_key "aws_auth_secrets", "users", column: "created_by_id"
  add_foreign_key "broker_credentials", "oauth_apps"
  add_foreign_key "broker_credentials", "users", column: "created_by_id"
  add_foreign_key "gcp_auth_secrets", "users", column: "created_by_id"
  add_foreign_key "gcp_id_token_secrets", "users", column: "created_by_id"
  add_foreign_key "grants", "aws_auth_secrets"
  add_foreign_key "grants", "gcp_auth_secrets"
  add_foreign_key "grants", "gcp_id_token_secrets"
  add_foreign_key "grants", "hmac_secrets"
  add_foreign_key "grants", "oauth_token_secrets"
  add_foreign_key "grants", "pg_dsn_secrets"
  add_foreign_key "grants", "principals"
  add_foreign_key "grants", "roles"
  add_foreign_key "grants", "static_secrets"
  add_foreign_key "grants", "users", column: "created_by_id"
  add_foreign_key "hmac_secrets", "users", column: "created_by_id"
  add_foreign_key "mcp_oauth_authorization_codes", "mcp_oauth_clients"
  add_foreign_key "mcp_oauth_authorization_codes", "principals"
  add_foreign_key "mcp_oauth_authorization_codes", "users"
  add_foreign_key "mcp_oauth_refresh_tokens", "mcp_oauth_clients"
  add_foreign_key "mcp_oauth_refresh_tokens", "principals"
  add_foreign_key "mcp_oauth_refresh_tokens", "users"
  add_foreign_key "oauth_apps", "users", column: "created_by_id"
  add_foreign_key "oauth_token_secrets", "users", column: "created_by_id"
  add_foreign_key "pg_dsn_secrets", "users", column: "created_by_id"
  add_foreign_key "principal_roles", "principals"
  add_foreign_key "principal_roles", "roles"
  add_foreign_key "slack_channel_permissions", "principals"
  add_foreign_key "principal_sync_config_snapshots", "principals"
  add_foreign_key "principals", "users", column: "created_by_id"
  add_foreign_key "proxies", "principals", on_delete: :nullify
  add_foreign_key "request_rules", "aws_auth_secrets"
  add_foreign_key "request_rules", "gcp_auth_secrets"
  add_foreign_key "request_rules", "gcp_id_token_secrets"
  add_foreign_key "request_rules", "hmac_secrets"
  add_foreign_key "request_rules", "oauth_token_secrets"
  add_foreign_key "request_rules", "static_secrets"
  add_foreign_key "roles", "users", column: "created_by_id"
  add_foreign_key "secret_sources", "aws_auth_secrets"
  add_foreign_key "secret_sources", "gcp_auth_secrets"
  add_foreign_key "secret_sources", "gcp_id_token_secrets"
  add_foreign_key "secret_sources", "hmac_secrets"
  add_foreign_key "secret_sources", "oauth_token_secrets"
  add_foreign_key "secret_sources", "pg_dsn_secrets"
  add_foreign_key "secret_sources", "static_secrets"
  add_foreign_key "static_secrets", "broker_credentials"
  add_foreign_key "static_secrets", "users", column: "created_by_id"
  add_foreign_key "user_identities", "users"
  add_foreign_key "users", "users", column: "approved_by_id"
end
