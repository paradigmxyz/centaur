require "test_helper"

class Api::V1::SandboxSkillsControllerTest < ActionDispatch::IntegrationTest
  setup do
    principal = Principal.create!(
      foreign_id: "console-user-member",
      name: "Member User",
      kind: :console_user,
      console_user: users(:member_user),
      labels: {},
      created_by: users(:member_user)
    )
    @member_proxy = Proxy.create!(
      name: "member-console-proxy",
      principal: principal,
      bearer_token_hash: Digest::SHA256.hexdigest("iprx_#{'d' * 64}")
    )
    @channel_proxy = proxies(:acme_proxy)
  end

  test "user principal sees its private skills and shared skills" do
    with_token(@member_proxy) do |headers|
      get "/api/v1/sandbox/skills", headers: headers
    end
    assert_response :ok

    ids = json_body.fetch("data").map { |skill| skill.fetch("id") }
    assert_includes ids, skills(:member_private).oid
    assert_includes ids, skills(:admin_shared).oid
    refute_includes ids, skills(:other_private).oid
  end

  test "shared principal sees shared skills only" do
    with_token(@channel_proxy) do |headers|
      get "/api/v1/sandbox/skills", headers: headers
    end
    assert_response :ok

    ids = json_body.fetch("data").map { |skill| skill.fetch("id") }
    assert_equal [ skills(:admin_shared).oid ], ids
  end

  test "search is principal scoped and read returns current content" do
    with_token(@member_proxy) do |headers|
      get "/api/v1/sandbox/skills/search", params: { q: "production incidents" }, headers: headers
    end
    assert_response :ok
    assert_equal skills(:member_private).oid, json_body.dig("data", 0, "id")

    with_token(@member_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skills(:member_private).oid}", headers: headers
    end
    assert_response :ok
    assert_equal skills(:member_private).skill_document, json_body.dig("data", "document")
    assert_equal "no-store", response.headers["Cache-Control"]

    with_token(@member_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skills(:member_private).name}", headers: headers
    end
    assert_response :ok
    assert_equal skills(:member_private).oid, json_body.dig("data", "id")
  end

  test "does not leak private skills" do
    with_token(@channel_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skills(:member_private).oid}", headers: headers
    end
    assert_response :not_found

    with_token(@channel_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skills(:member_private).name}", headers: headers
    end
    assert_response :not_found

    with_token(@channel_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skills(:admin_shared).oid}", headers: headers
    end
    assert_response :ok
  end

  test "console user principal authors through the sandbox namespace" do
    with_token(@member_proxy) do |headers|
      post "/api/v1/sandbox/skills",
           params: {
             data: {
               name: "sandbox-authored",
               description: "Authored through the sandbox API.",
               instructions: "# Instructions\n\nInitial instructions."
             }
           },
           headers: headers,
           as: :json
    end
    assert_response :created
    skill = users(:member_user).skills.find_by!(name: "sandbox-authored")
    assert_equal "shared", skill.visibility
    assert_not_nil skill.shared_at

    with_token(@member_proxy) do |headers|
      patch "/api/v1/sandbox/skills/#{skill.oid}",
            params: {
              data: {
                name: "sandbox-authored",
                description: "Updated through the sandbox API.",
                instructions: "# Instructions\n\nUpdated instructions.",
                lock_version: skill.lock_version
              }
            },
            headers: headers,
            as: :json
    end
    assert_response :ok
    assert_equal "Updated through the sandbox API.", skill.reload.description
    assert_includes skill.content, "Updated instructions."

    with_token(@member_proxy) do |headers|
      post "/api/v1/sandbox/skills/#{skill.oid}/share", headers: headers
    end
    assert_response :ok
    assert skill.reload.shared?

    with_token(@member_proxy) do |headers|
      post "/api/v1/sandbox/skills/#{skill.oid}/unshare", headers: headers
    end
    assert_response :ok
    assert_not skill.reload.shared?

    with_token(@member_proxy) do |headers|
      delete "/api/v1/sandbox/skills/#{skill.oid}", headers: headers
    end
    assert_response :no_content
    assert_not_nil skill.reload.archived_at
  end

  test "editor principal reads and updates a private skill but cannot archive it" do
    skill = skills(:other_private)
    skill.editors << users(:member_user)

    with_token(@member_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skill.oid}", headers: headers
    end
    assert_response :ok

    with_token(@member_proxy) do |headers|
      patch "/api/v1/sandbox/skills/#{skill.oid}",
            params: {
              data: {
                name: skill.name,
                description: "Updated by a collaborator.",
                instructions: skill.content,
                lock_version: skill.lock_version
              }
            },
            headers: headers,
            as: :json
    end
    assert_response :ok
    assert_equal "Updated by a collaborator.", skill.reload.description

    with_token(@member_proxy) do |headers|
      delete "/api/v1/sandbox/skills/#{skill.oid}", headers: headers
    end
    assert_response :not_found
    assert_nil skill.reload.archived_at
  end

  test "owner lists adds and removes editors by email or user OID" do
    skill = skills(:member_private)
    editor = users(:globex_admin)
    initial_lock_version = skill.lock_version

    with_token(@member_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skill.oid}/editors", headers: headers
    end
    assert_response :ok
    assert_equal [], json_body.dig("data", "editors")

    assert_difference("SkillEditor.count", 1) do
      with_token(@member_proxy) do |headers|
        post "/api/v1/sandbox/skills/#{skill.oid}/editors",
             params: { data: { user: editor.email.upcase } },
             headers: headers,
             as: :json
      end
    end
    assert_response :ok
    assert_equal editor.oid, json_body.dig("data", "editors", 0, "id")
    assert_equal editor.email, json_body.dig("data", "editors", 0, "email")
    assert_equal initial_lock_version + 1, json_body.dig("data", "lock_version")

    assert_no_difference("SkillEditor.count") do
      with_token(@member_proxy) do |headers|
        post "/api/v1/sandbox/skills/#{skill.oid}/editors",
             params: { data: { user: editor.oid } },
             headers: headers,
             as: :json
      end
    end
    assert_response :ok
    assert_equal initial_lock_version + 1, json_body.dig("data", "lock_version")

    assert_difference("SkillEditor.count", -1) do
      with_token(@member_proxy) do |headers|
        delete "/api/v1/sandbox/skills/#{skill.oid}/editors",
               params: { data: { user: editor.email } },
               headers: headers,
               as: :json
      end
    end
    assert_response :ok
    assert_equal [], json_body.dig("data", "editors")
    assert_equal initial_lock_version + 2, json_body.dig("data", "lock_version")
  end

  test "editor can list membership but cannot manage editors" do
    skill = skills(:other_private)
    skill.editors << users(:member_user)

    with_token(@member_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skill.oid}/editors", headers: headers
    end
    assert_response :ok
    assert_equal users(:member_user).oid, json_body.dig("data", "editors", 0, "id")

    with_token(@member_proxy) do |headers|
      post "/api/v1/sandbox/skills/#{skill.oid}/editors",
           params: { data: { user: users(:globex_admin).oid } },
           headers: headers,
           as: :json
    end
    assert_response :not_found

    with_token(@member_proxy) do |headers|
      delete "/api/v1/sandbox/skills/#{skill.oid}/editors",
             params: { data: { user: users(:member_user).oid } },
             headers: headers,
             as: :json
    end
    assert_response :not_found
    assert_equal [ users(:member_user) ], skill.reload.editors.to_a
  end

  test "shared skill editor membership is visible to user and shared principals" do
    skill = skills(:admin_shared)
    skill.editors << users(:globex_admin)

    with_token(@member_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skill.oid}/editors", headers: headers
    end
    assert_response :ok
    assert_equal users(:globex_admin).email, json_body.dig("data", "editors", 0, "email")

    with_token(@channel_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skill.oid}/editors", headers: headers
    end
    assert_response :ok
    assert_equal users(:globex_admin).oid, json_body.dig("data", "editors", 0, "id")
  end

  test "private skill editor membership remains limited to users who can edit it" do
    skill = skills(:other_private)
    skill.editors << users(:globex_admin)

    with_token(@member_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skill.oid}/editors", headers: headers
    end
    assert_response :not_found

    with_token(@channel_proxy) do |headers|
      get "/api/v1/sandbox/skills/#{skill.oid}/editors", headers: headers
    end
    assert_response :not_found
  end

  test "owner cannot add itself or a disabled user as an editor" do
    skill = skills(:member_private)

    with_token(@member_proxy) do |headers|
      post "/api/v1/sandbox/skills/#{skill.oid}/editors",
           params: { data: { user: users(:member_user).oid } },
           headers: headers,
           as: :json
    end
    assert_response :unprocessable_entity

    with_token(@member_proxy) do |headers|
      post "/api/v1/sandbox/skills/#{skill.oid}/editors",
           params: { data: { user: users(:disabled_user).email } },
           headers: headers,
           as: :json
    end
    assert_response :not_found
    assert_empty skill.reload.editors
  end

  test "non-user principal cannot mutate skills" do
    assert_no_difference("Skill.count") do
      with_token(@channel_proxy) do |headers|
        post "/api/v1/sandbox/skills",
             params: {
               data: {
                 name: "forbidden-skill",
                 description: "Must not be created.",
                 instructions: "# Instructions"
               }
             },
             headers: headers,
             as: :json
      end
    end
    assert_response :forbidden
  end

  test "duplicate-name create races return a validation response" do
    duplicate_race = lambda do |skill|
      raise ActiveRecord::RecordNotUnique, "duplicate skill name" if skill.name == "duplicate-skill"
    end
    Skill.set_callback(:validation, :after, duplicate_race)

    with_token(@member_proxy) do |headers|
      post "/api/v1/sandbox/skills",
           params: {
             data: {
               name: "duplicate-skill",
               description: "Conflicts with a concurrent create.",
               instructions: "# Instructions"
             }
           },
           headers: headers,
           as: :json
    end

    assert_response :unprocessable_entity
    assert_equal "record conflicts with an existing record", json_body.dig("error", "message")
  ensure
    Skill.skip_callback(:validation, :after, duplicate_race) if duplicate_race
  end

  test "rejects a token after its proxy principal changes" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      token = SandboxEntitlements::Jwt.encode_for_proxy(@member_proxy)
      @member_proxy.update!(principal: principals(:acme_channel))
      get "/api/v1/sandbox/skills", headers: auth_headers(token)
    end
    assert_response :unauthorized
  end

  private

  def with_token(proxy)
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      yield auth_headers(SandboxEntitlements::Jwt.encode_for_proxy(proxy))
    end
  end

  def auth_headers(token)
    { "Authorization" => "Bearer #{token}" }
  end

  def json_body
    JSON.parse(response.body)
  end
end
