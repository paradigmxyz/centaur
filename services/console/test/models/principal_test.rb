require "test_helper"

class PrincipalTest < ActiveSupport::TestCase
  def default_attrs(overrides = {})
    { created_by: users(:acme_admin) }.merge(overrides)
  end

  test "is valid with namespace and foreign_id" do
    principal = Principal.new(default_attrs(namespace: "acme", foreign_id: "C-new-1"))
    assert principal.valid?
  end

  test "namespace defaults to 'default'" do
    principal = Principal.new(default_attrs)
    assert_equal "default", principal.namespace
    assert_equal Principal::UNKNOWN_KIND, principal.kind
    assert principal.valid?
  end

  test "identity fields are stored only in columns and synthesized for compatibility" do
    principal = Principal.create!(default_attrs(
      kind: "slack_dm",
      slack_user_id: "U0123456789",
      slack_channel_id: "D0123456789",
      slack_team_id: "T0123456789",
      slack_email: "ada@example.com",
      labels: { "team" => "platform" }
    ))

    principal.reload
    assert_equal "slack_dm", principal.kind
    assert_equal "U0123456789", principal.slack_user_id
    assert_equal "D0123456789", principal.slack_channel_id
    assert_equal "T0123456789", principal.slack_team_id
    assert_equal "ada@example.com", principal.slack_email
    assert_equal(
      { "team" => "platform", Principal::SANDBOX_REPO_CACHE_LABEL => "all" },
      principal.labels
    )
    assert_equal(
      {
        "team" => "platform",
        Principal::SANDBOX_REPO_CACHE_LABEL => "all",
        "kind" => "slack_dm",
        "slack_user_id" => "U0123456789",
        "slack_channel_id" => "D0123456789",
        "slack_team_id" => "T0123456789",
        "slack_email" => "ada@example.com"
      },
      principal.labels_with_sandbox_capabilities
    )
  end

  test "legacy identity labels are promoted into columns" do
    principal = Principal.create!(default_attrs(
      labels: {
        "kind" => "slack_dm",
        "slack_user_id" => "U0123456789",
        "slack_team_id" => "T0123456789",
        "slack_email" => "ada@example.com"
      }
    ))

    principal.reload
    assert_equal "slack_dm", principal.kind
    assert_equal "U0123456789", principal.slack_user_id
    assert_equal "T0123456789", principal.slack_team_id
    assert_equal "ada@example.com", principal.slack_email
    assert_empty principal.labels.slice(*PrincipalIdentityLabels.labels_for(principal.kind))
  end

  test "first-class identity fields must agree with compatibility labels" do
    principal = Principal.new(default_attrs(
      kind: "slack_dm",
      slack_user_id: "U0123456789",
      labels: {
        "kind" => "slack_channel",
        "slack_user_id" => "U9876543210"
      }
    ))

    assert_not principal.valid?
    assert_includes principal.errors[:kind], "does not agree with labels.kind"
    assert_includes principal.errors[:slack_user_id], "does not agree with labels.slack_user_id"
  end

  test "console user identity is stored only in columns and synthesized for compatibility" do
    user = users(:acme_admin)
    principal = Principal.create!(default_attrs(
      kind: "console_user",
      console_user_id: user.id,
      console_user_email: user.email,
      labels: { "managed-by" => "centaur" }
    ))

    principal.reload
    assert_equal user.id, principal.console_user_id
    assert_equal user.email, principal.console_user_email
    assert_empty principal.labels.slice("console-user-id", "email")
    assert_equal user.oid, principal.labels_with_sandbox_capabilities["console-user-id"]
    assert_equal user.email, principal.labels_with_sandbox_capabilities["email"]
  end

  test "legacy console user identity labels are promoted only for console users" do
    user = users(:acme_admin)
    console_user = Principal.create!(default_attrs(
      labels: {
        "kind" => "console_user",
        "console-user-id" => user.oid,
        "email" => user.email
      }
    ))
    ordinary_user = Principal.create!(default_attrs(
      labels: { "kind" => "user", "email" => user.email }
    ))

    assert_equal user.id, console_user.reload.console_user_id
    assert_equal user.email, console_user.console_user_email
    assert_empty console_user.labels.slice("console-user-id", "email")
    assert_nil ordinary_user.reload.console_user_id
    assert_nil ordinary_user.console_user_email
    assert_equal user.email, ordinary_user.labels["email"]
  end

  test "blank legacy Slack identity labels clear columns" do
    principal = Principal.create!(default_attrs(
      kind: "slack_dm",
      slack_user_id: "U0123456789",
      slack_channel_id: "D0123456789",
      slack_team_id: "T0123456789",
      slack_email: "ada@example.com"
    ))

    principal.update!(labels: {
      "kind" => "slack_dm",
      "slack_user_id" => "",
      "slack_channel_id" => "  ",
      "slack_team_id" => nil,
      "slack_email" => "\t"
    })

    principal.reload
    assert_nil principal.slack_user_id
    assert_nil principal.slack_channel_id
    assert_nil principal.slack_team_id
    assert_nil principal.slack_email
  end

  test "kind accepts only known values" do
    Principal::KINDS.each do |kind|
      assert Principal.new(default_attrs(kind: kind)).valid?, "expected #{kind.inspect} to be valid"
    end

    %w[service future_platform].each do |kind|
      principal = Principal.new(default_attrs(kind: kind))
      assert_not principal.valid?
      assert_includes principal.errors[:kind], "must be one of #{Principal::KINDS.join(", ")}"
    end

    principal = Principal.new(default_attrs(kind: "  "))
    assert_not principal.valid?
    assert_includes principal.errors[:kind], "can't be blank"
  end

  test "Slack identity fields must use canonical formats" do
    {
      slack_user_id: %w[U0123456789 W0123456789 USLACK],
      slack_channel_id: %w[C0123456789 D0123456789 G0123456789],
      slack_team_id: %w[T0123456789 E0123456789],
      slack_email: %w[ada@example.com]
    }.each do |field, values|
      values.each do |value|
        assert Principal.new(default_attrs(field => value)).valid?, "expected #{field}=#{value.inspect} to be valid"
      end
    end

    {
      slack_user_id: " U0123456789 ",
      slack_channel_id: "C123",
      slack_team_id: "t0123456789",
      slack_email: "not-an-email"
    }.each do |field, value|
      principal = Principal.new(default_attrs(field => value))
      assert_not principal.valid?
      assert_predicate principal.errors[field], :any?
    end
  end

  test "console user principals permit missing user references but require valid email" do
    user = users(:acme_admin)
    principal = Principal.new(default_attrs(
      kind: "console_user",
      console_user_id: user.id,
      console_user_email: user.email
    ))
    assert principal.valid?

    principal.console_user_id = nil
    assert principal.valid?

    principal.console_user_email = "not-an-email"
    assert_not principal.valid?
    assert_predicate principal.errors[:console_user_email], :any?
  end

  test "unchanged malformed migrated Slack identities do not block unrelated saves" do
    principal = principals(:acme_user_alice)
    principal.update_columns(
      slack_user_id: "U12345",
      slack_channel_id: "D123",
      slack_team_id: "TACME",
      slack_email: "pending"
    )

    assert principal.reload.update!(name: "Renamed legacy principal")

    principal.slack_user_id = "U67890"
    assert_not principal.valid?
    assert_includes principal.errors[:slack_user_id], "is not a valid Slack user ID"

    principal.slack_user_id = nil
    assert principal.save!
  end

  test "is valid with only a name" do
    assert Principal.new(default_attrs(name: "Just a label")).valid?
  end

  test "rejects a foreign_id that starts with the opaque id prefix" do
    principal = Principal.new(default_attrs(namespace: "acme", foreign_id: "prn_abc123"))
    assert_not principal.valid?
    assert_includes principal.errors[:foreign_id], "must not start with \"prn_\", which is reserved for opaque ids"
  end

  test "is invalid when namespace is blank" do
    principal = Principal.new(default_attrs(namespace: "", foreign_id: "C-blank"))
    assert_not principal.valid?
    assert_includes principal.errors[:namespace], "can't be blank"
  end

  test "foreign_id is unique within a namespace" do
    existing = principals(:acme_channel)
    dup = Principal.new(default_attrs(namespace: existing.namespace, foreign_id: existing.foreign_id))
    assert_not dup.valid?
    assert_includes dup.errors[:foreign_id], "has already been taken"
  end

  test "same foreign_id is allowed across different namespaces" do
    existing = principals(:acme_channel)
    other = Principal.new(default_attrs(namespace: "globex", foreign_id: existing.foreign_id))
    assert other.valid?
  end

  test "labels include sandbox repo-cache projection by default" do
    principal = Principal.create!(default_attrs(namespace: "acme", foreign_id: "C-default-labels"))
    assert_equal({ Principal::SANDBOX_REPO_CACHE_LABEL => "all" }, principal.reload.labels)
  end

  test "sandbox access defaults to enabled" do
    principal = Principal.create!(default_attrs(namespace: "acme", foreign_id: "C-default-sandbox-access"))
    principal.reload

    assert_equal "all", principal.sandbox_repo_cache
    assert_predicate principal, :sandbox_observability_enabled
    assert_predicate principal, :sandbox_api_server_enabled
  end

  test "new principals with no roles receive configured defaults from their namespace" do
    Role.update_all(assign_by_default: false)
    [ roles(:acme_infra), roles(:acme_admin_role), roles(:globex_infra) ].each do |role|
      role.update!(assign_by_default: true)
    end

    principal = Principal.create!(default_attrs(namespace: "acme", foreign_id: "U-default-roles"))

    assert_equal [ roles(:acme_infra), roles(:acme_admin_role) ].sort_by(&:id), principal.roles.order(:id)
  end

  test "preassigned roles suppress configured defaults" do
    roles(:acme_infra).update!(assign_by_default: true)
    principal = Principal.new(default_attrs(namespace: "acme", foreign_id: "U-explicit-role"))
    principal.roles = [ roles(:acme_admin_role) ]

    principal.save!

    assert_equal [ roles(:acme_admin_role) ], principal.reload.roles
  end

  test "configured defaults are not applied to existing roleless principals" do
    principal = principals(:acme_user_bob)
    principal.principal_roles.destroy_all
    roles(:acme_infra).update!(assign_by_default: true)

    principal.update!(name: "Still roleless")

    assert_empty principal.reload.roles
  end

  test "default sandbox repo-cache overwrites explicit label assignment" do
    principal = Principal.new(default_attrs(namespace: "acme", foreign_id: "C-explicit-repo-cache-label"))

    principal.apply_default_sandbox_capabilities!
    principal.assign_attributes(labels: { Principal::SANDBOX_REPO_CACHE_LABEL => "none" })
    principal.save!

    assert_equal "all", principal.reload.sandbox_repo_cache
    assert_equal({ Principal::SANDBOX_REPO_CACHE_LABEL => "all" }, principal.labels)
  end

  test "sandbox repo-cache stores canonical enum value" do
    principal = Principal.create!(default_attrs(namespace: "acme", foreign_id: "C-repo-cache-setting"))

    principal.update!(sandbox_repo_cache: "public")
    principal.reload

    assert_equal "public", principal.sandbox_repo_cache
    assert_equal "public", principal.labels[Principal::SANDBOX_REPO_CACHE_LABEL]
  end

  test "sandbox repo-cache enum survives labels assigned in the same update" do
    principal = Principal.create!(default_attrs(namespace: "acme", foreign_id: "C-repo-cache-label-order"))

    principal.update!(sandbox_repo_cache: "public", labels: { "team" => "platform" })
    principal.reload

    assert_equal "public", principal.sandbox_repo_cache
    assert_equal "platform", principal.labels["team"]
    assert_equal "public", principal.labels[Principal::SANDBOX_REPO_CACHE_LABEL]
  end

  test "sandbox repo-cache overwrites label with canonical value" do
    principal = Principal.create!(default_attrs(namespace: "acme", foreign_id: "C-repo-cache-label"))

    principal.update!(labels: { Principal::SANDBOX_REPO_CACHE_LABEL => "public" })
    principal.reload

    assert_equal "all", principal.sandbox_repo_cache
    assert_equal({ Principal::SANDBOX_REPO_CACHE_LABEL => "all" }, principal.labels)
  end

  test "sandbox repo-cache param takes precedence over conflicting label" do
    principal = Principal.create!(default_attrs(namespace: "acme", foreign_id: "C-repo-cache-param-wins"))

    principal.update!(
      sandbox_repo_cache: "public",
      labels: { Principal::SANDBOX_REPO_CACHE_LABEL => "none" }
    )
    principal.reload

    assert_equal "public", principal.sandbox_repo_cache
    assert_equal({ Principal::SANDBOX_REPO_CACHE_LABEL => "public" }, principal.labels)
  end

  test "sandbox repo-cache rejects invalid enum values" do
    principal = Principal.new(default_attrs(namespace: "acme", foreign_id: "C-repo-cache-invalid"))
    principal.sandbox_repo_cache = "pub"

    assert_not principal.valid?
    assert_includes principal.errors[:sandbox_repo_cache], "is not included in the list"
  end

  test "labels accepts arbitrary string map" do
    principal = Principal.create!(default_attrs(
      namespace: "acme",
      foreign_id: "C-labels",
      labels: { "env" => "prod", "team" => "platform" }
    ))
    assert_equal(
      {
        "env" => "prod",
        "team" => "platform",
        Principal::SANDBOX_REPO_CACHE_LABEL => "all"
      },
      principal.reload.labels
    )
  end

  test "effective Slack permissions merge direct and role rows deterministically" do
    principal = principals(:acme_channel)
    role = roles(:acme_infra)
    SlackChannelPermission.create!(
      principal: principal,
      channel_id: "C0123456789",
      upload_enabled: true
    )
    SlackChannelPermission.create!(
      role: role,
      channel_id: "C0123456789",
      download_enabled: true
    )
    SlackChannelPermission.create!(
      role: role,
      channel_id: "G9876543210",
      history_enabled: true
    )

    assert_equal(
      [
        {
          "channel_id" => "C0123456789",
          "upload_enabled" => true,
          "download_enabled" => true,
          "history_enabled" => false
        },
        {
          "channel_id" => "G9876543210",
          "upload_enabled" => false,
          "download_enabled" => false,
          "history_enabled" => true
        }
      ],
      principal.effective_slack_channel_permissions_payload
    )
    channel_ids = principal.slack_channel_ids_by_permission
    assert_equal [ "C0123456789" ], channel_ids.fetch(:upload)
    assert_equal [ "C0123456789" ], channel_ids.fetch(:download)
    assert_equal [ "G9876543210" ], principal.slack_history_channel_ids
  end

  test "unsaved principals do not load role-owned Slack permissions" do
    roles(:acme_infra).slack_channel_permissions.create!(
      channel_id: "G9876543210",
      history_enabled: true
    )
    principal = Principal.new
    principal.slack_channel_permissions.build(
      channel_id: "C0123456789",
      upload_enabled: true
    )

    assert_equal [ "C0123456789" ],
                 principal.effective_slack_channel_permissions_payload.pluck("channel_id")
  end

  test "api server JWT derives all Slack claims from one effective payload" do
    principal = principals(:acme_channel)
    calls = 0
    payload = [
      {
        "channel_id" => "C0123456789",
        "upload_enabled" => true,
        "download_enabled" => true,
        "history_enabled" => false
      }
    ]
    original = principal.method(:effective_slack_channel_permissions_payload)
    principal.define_singleton_method(:effective_slack_channel_permissions_payload) do
      calls += 1
      payload
    end

    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      assert_not_nil ApiServer::Jwt.encode_for_principal(principal)
    end
    assert_equal 1, calls
  ensure
    principal&.define_singleton_method(:effective_slack_channel_permissions_payload, original) if original
  end

  test "api server JWT includes inherited role Slack permissions" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      principal = principals(:acme_channel)
      roles(:acme_infra).slack_channel_permissions.create!(
        channel_id: "C0123456789",
        upload_enabled: true,
        history_enabled: true
      )

      claims = jwt_payload(ApiServer::Jwt.encode_for_principal(principal))
      assert_equal [ "C0123456789" ], claims.dig("slack", "upload_channels")
      assert_equal [ "C0123456789" ], claims.dig("slack", "history_channels")
    end
  end

  test "api server JWT does not infer permissions from Slack channel identity" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      principal = principals(:acme_channel)
      principal.update!(slack_channel_id: "C0123456789")

      assert_nil ApiServer::Jwt.encode_for_principal(principal)
    end
  end

  test "clearing slack channel permissions revokes slack access" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      principal = Principal.create!(
        default_attrs(
          namespace: "acme",
          foreign_id: "C-clear-slack-permissions"
        )
      )
      SlackChannelPermission.create!(
        principal: principal,
        channel_id: "C0123456789",
        upload_enabled: true,
        download_enabled: true,
        history_enabled: true
      )
      assert_not_nil ApiServer::Jwt.encode_for_principal(principal)

      SlackChannelPermission.replace_for!(principal, [])

      assert_empty principal.slack_channel_permissions.reload
      assert_nil ApiServer::Jwt.encode_for_principal(principal)
    end
  end

  test "api server JWT is deterministic inside the rotation window" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      principal = principals(:acme_channel)
      SlackChannelPermission.create!(
        principal: principal,
        channel_id: "C0123456789",
        upload_enabled: true
      )

      window = ApiServer::Jwt::DEFAULT_WINDOW_SECONDS
      boundary = 1_700_000_100 + ApiServer::Jwt.rotation_offset(principal)

      first = ApiServer::Jwt.encode_for_principal(
        principal,
        now: Time.zone.at(boundary + 23)
      )
      second = ApiServer::Jwt.encode_for_principal(
        principal,
        now: Time.zone.at(boundary + window - 1)
      )
      third = ApiServer::Jwt.encode_for_principal(
        principal,
        now: Time.zone.at(boundary + window)
      )

      assert_equal first, second
      refute_equal first, third
    end
  end

  test "namespace is immutable after creation" do
    principal = principals(:acme_channel)
    assert_raises(ActiveRecord::ReadonlyAttributeError) do
      principal.update!(namespace: "other")
    end
  end

  test "foreign_id is immutable after creation" do
    principal = principals(:acme_channel)
    assert_raises(ActiveRecord::ReadonlyAttributeError) do
      principal.update!(foreign_id: "C-other")
    end
  end

  test "labels remain mutable after creation" do
    principal = principals(:acme_channel)
    principal.update!(labels: { "changed" => "yes" })
    assert_equal(
      {
        "changed" => "yes",
        Principal::SANDBOX_REPO_CACHE_LABEL => "all"
      },
      principal.reload.labels
    )
  end

  test "name is editable after creation" do
    principal = principals(:acme_channel)
    principal.update!(name: "Acme Slack channel")
    assert_equal "Acme Slack channel", principal.reload.name
  end

  test "identity field changes invalidate the sync config cache" do
    principal = Principal.create!(default_attrs)
    previous_version = principal.sync_config_cache_version

    principal.update!(
      kind: "slack_dm",
      slack_user_id: "U0123456789",
      slack_team_id: "T0123456789",
      slack_email: "a@example.com"
    )

    assert_equal previous_version + 1, principal.reload.sync_config_cache_version
  end

  test "declares prn as its oid prefix" do
    assert_equal "prn", Principal.oid_prefix
  end

  test "requires created_by" do
    principal = Principal.new(namespace: "acme", foreign_id: "C-needs-key")
    assert_not principal.valid?
    assert_includes principal.errors[:created_by], "must exist"
  end

  test "destroying a principal unassigns its proxies rather than destroying them" do
    principal = principals(:acme_channel)
    proxy = proxies(:acme_proxy)
    assert_equal principal, proxy.principal

    assert_no_difference -> { Proxy.count } do
      principal.destroy!
    end

    proxy.reload
    assert_nil proxy.principal
    assert_equal "unassigned", proxy.status
  end

  # --- grant resolution ---------------------------------------------------

  test "granted_static_secrets includes secrets granted via an assigned role" do
    # acme_channel holds the acme_infra role, which is granted acme_prod_api_key.
    assert_includes principals(:acme_channel).granted_static_secrets, static_secrets(:acme_prod_api_key)
  end

  test "effective grants dedupe a secret reachable both directly and via a role" do
    principal = principals(:acme_channel)
    # acme_prod_api_key already reaches the principal through the acme_infra role;
    # also grant it directly and confirm it still appears exactly once.
    Grant.create!(principal: principal, static_secret: static_secrets(:acme_prod_api_key),
                  created_by: users(:acme_admin))
    ids = principal.granted_static_secrets.map(&:id)
    assert_equal ids.uniq, ids
    assert_equal 1, ids.count(static_secrets(:acme_prod_api_key).id)
  end

  test "directly granted secrets are emitted after role-granted secrets" do
    # acme_channel holds two direct grants (priority 100) and resolves
    # acme_prod_api_key through the acme_infra role (priority 0). The role secret
    # is emitted first so the higher-priority direct secrets win iron-proxy's
    # last-transform-wins.
    ids = principals(:acme_channel).granted_static_secrets.map(&:id)
    role_secret = static_secrets(:acme_prod_api_key)
    direct_secrets = [ static_secrets(:github_token_inject), static_secrets(:db_password_replace) ]

    assert_equal role_secret.id, ids.first
    direct_secrets.each do |s|
      assert_operator ids.index(s.id), :>, ids.index(role_secret.id)
    end
  end

  test "an explicitly higher-priority role grant outranks direct grants" do
    grants(:acme_infra_prod_api_key).update!(priority: 500)
    ids = principals(:acme_channel).granted_static_secrets.map(&:id)
    # Promoted above the direct grants (priority 100), it now sorts last and wins.
    assert_equal static_secrets(:acme_prod_api_key).id, ids.last
  end

  test "a secret reachable via several grants takes the strongest priority" do
    principal = principals(:acme_channel)
    # Already reaches acme_prod_api_key via the role at priority 0; add a direct
    # grant at a higher priority. The secret collapses to one row taking the MAX
    # priority, so it sorts last rather than first.
    Grant.create!(principal: principal, static_secret: static_secrets(:acme_prod_api_key),
                  created_by: users(:acme_admin), priority: 900)
    ids = principal.granted_static_secrets.map(&:id)

    assert_equal ids.uniq, ids
    assert_equal static_secrets(:acme_prod_api_key).id, ids.last
  end

  def jwt_payload(token)
    _header, payload, _signature = token.split(".")
    JSON.parse(Base64.urlsafe_decode64(payload))
  end
end
