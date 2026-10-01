require "test_helper"

class CrewProfileTest < ActiveSupport::TestCase
  test "two bots have separate roles and revocation updates proxy config without restoring defaults" do
    alpha = provision("alpha", "A111", [ roles(:acme_infra) ])
    beta = provision("beta", "A222", [])
    assert_equal [ roles(:acme_infra).id ], alpha.principal.role_ids
    assert_empty beta.principal.roles
    version = alpha.principal.reload.sync_config_cache_version
    alpha.provision!(record("alpha", "A111"), user: users(:acme_admin), roles: [])
    assert_empty alpha.principal.roles
    assert_operator alpha.principal.reload.sync_config_cache_version, :>, version
    assert_empty beta.principal.roles
  end

  test "refuses to move app identity and existing credential grants" do
    alpha = provision("alpha", "A111", [ roles(:acme_infra) ])
    assert_raises(SlackCrewClient::Error) do
      alpha.provision!(record("alpha", "A222"), user: users(:acme_admin), roles: [])
    end
    assert_equal "A111", alpha.reload.app_id
    assert_equal [ roles(:acme_infra).id ], alpha.principal.role_ids
  end

  test "validates skill paths uniqueness document bounds and model fields" do
    profile = CrewProfile.new(crew_id: "alpha")
    skill = { "name" => "reviewing-code", "description" => "Reviews code when asked.", "content" => "Check boundaries." }
    profile.skills = [ skill ]
    assert profile.valid?
    [ "../escape", "search", "two--hyphens", "-prefix", "AName" ].each do |name|
      profile.skills = [ skill.merge("name" => name) ]
      assert_not profile.valid?, name
    end
    profile.skills = [ skill, skill ]
    assert_not profile.valid?
    profile.skills = [ skill.merge("content" => "x" * 65_537) ]
    assert_not profile.valid?
    profile.skills = []
    profile.default_models = { "unexpected" => "model" }
    assert_not profile.valid?
    profile.default_models = { "claude" => "one\ntwo" }
    assert_not profile.valid?
    profile.default_models = { "claude" => "my-model" }
    assert profile.valid?
  end

  private

  def record(id, app)
    { "id" => id, "name" => id.capitalize, "app_id" => app, "team_id" => "T123" }
  end

  def provision(id, app, roles)
    CrewProfile.new(crew_id: id).tap { |profile| profile.provision!(record(id, app), user: users(:acme_admin), roles: roles) }
  end
end
