require "test_helper"

class Console::CrewControllerTest < ActionDispatch::IntegrationTest
  class FakeClient
    attr_accessor :result, :error
    attr_reader :calls
    def initialize
      @calls = []
      @result = { "crew" => [], "profiles" => [ "engineer" ] }
    end
    def list
      raise error if error
      result
    end
    def create(attributes)
      calls << [ :create, attributes ]
      raise error if error
      result.fetch("created")
    end
    def update(id, attributes)
      calls << [ :update, id, attributes ]
      raise error if error
      result["crew"].find { |bot| bot["id"] == id }.merge(attributes)
    end
    def install(id)
      calls << [ :install, id ]
      result.fetch("created")
    end
  end

  setup do
    @client = FakeClient.new
    @bot = { "id" => "alpha", "name" => "Alpha", "status" => "active", "app_id" => "A111", "team_id" => "T123", "crew_id" => "engineer" }
    @client.result["created"] = @bot
    Console::CrewController.client_factory = -> { @client }
    login(users(:acme_admin))
  end

  teardown do
    Console::CrewController.client_factory = -> { SlackCrewClient.new }
  end

  test "requires active acting admin for configuration and secret roles" do
    delete logout_url
    get new_console_crew_url
    assert_redirected_to login_path
    login(users(:member_user))
    post console_crew_index_url, params: { crew: fields }
    assert_redirected_to console_integrations_path
    delete logout_url
    login(users(:acme_admin))
    post console_descope_url
    post console_crew_index_url, params: { crew: fields }
    assert_redirected_to console_integrations_path
    assert_empty @client.calls
  end

  test "disabled account is denied and empty state offers creation" do
    get console_crew_index_url
    assert_response :ok
    assert_select "a", text: "Create your first bot"
    users(:acme_admin).update!(status: :disabled)
    get console_crew_index_url
    assert_redirected_to login_path
  end

  test "create automatically installs and provisions exactly the selected roles and behavior" do
    post console_crew_index_url, params: { crew: fields }
    assert_redirected_to edit_console_crew_path("alpha")
    profile = CrewProfile.find_by!(crew_id: "alpha")
    assert_equal "slack-crew-t123-a111", profile.principal.foreign_id
    assert_equal [ roles(:acme_infra).id ], profile.principal.role_ids
    assert_equal "Investigate carefully.", profile.system_prompt
    assert_equal "reviewing-incidents", profile.skills.first["name"]
    assert_equal({ "codex" => "model-a", "claude" => "model-b" }, profile.default_models)
    assert_equal %w[crew_id description id name], @client.calls.first.last.keys.sort
  end

  test "editor preserves settings and can remove all skills and roles without granting defaults" do
    post console_crew_index_url, params: { crew: fields }
    @client.result["crew"] = [ @bot.merge("description" => "Helper") ]
    get edit_console_crew_url("alpha")
    assert_response :ok
    assert_select "textarea[name='crew[system_prompt]']", text: "Investigate carefully."
    assert_select "input[name='crew[role_oids][]'][checked]", count: 1
    @client.calls.clear
    patch console_crew_url("alpha"), params: { crew: fields.except(:id, :crew_id, :skills).merge(role_oids: [ "" ], lock_version: 0) }
    assert_redirected_to edit_console_crew_path("alpha")
    profile = CrewProfile.find_by!(crew_id: "alpha")
    assert_empty profile.skills
    assert_empty profile.principal.roles
    assert_empty @client.calls, "Behavior and role edits must not require a valid Slack configuration token"
  end

  test "invalid skill and forged roles do not create a Slack app" do
    post console_crew_index_url, params: { crew: fields.merge(skills: { "0" => { name: "../escape", description: "Bad", content: "Bad" } }) }
    assert_response :unprocessable_entity
    assert_empty @client.calls
    assert_select "textarea[name='crew[system_prompt]']", text: "Investigate carefully."
    post console_crew_index_url, params: { crew: fields.merge(role_oids: [ "role_invalid" ]) }
    assert_response :not_found
    assert_empty @client.calls
  end

  test "stale configuration cannot overwrite a newer bot edit or mutate Slack" do
    post console_crew_index_url, params: { crew: fields }
    profile = CrewProfile.find_by!(crew_id: "alpha")
    profile.update!(system_prompt: "Newer instructions")
    @client.result["crew"] = [ @bot ]
    @client.calls.clear
    patch console_crew_url("alpha"), params: { crew: fields.merge(lock_version: 0) }
    assert_redirected_to edit_console_crew_path("alpha")
    assert_match(/changed since/, flash[:alert])
    assert_equal "Newer instructions", profile.reload.system_prompt
    assert_empty @client.calls
  end

  test "interrupted install exposes no blind retry while known pending app can install" do
    @client.result["crew"] = [ @bot.merge("status" => "installing", "install_error" => "Installation outcome is unknown") ]
    get edit_console_crew_url("alpha")
    assert_response :ok
    assert_select "p", text: "Installation outcome is unknown"
    assert_select "input[type=submit][value='Save changes'][disabled]"
    assert_select "form[action=?]", install_console_crew_path("alpha"), count: 0
    @client.result["crew"] = [ @bot.merge("status" => "needs_install") ]
    post install_console_crew_url("alpha")
    assert_redirected_to edit_console_crew_path("alpha")
    assert_equal [ :install, "alpha" ], @client.calls.last
  end

  test "replayed pending creation does not claim Slack installation succeeded" do
    @client.result["created"] = @bot.merge("status" => "needs_install")
    post console_crew_index_url, params: { crew: fields }
    assert_redirected_to edit_console_crew_path("alpha")
    assert_equal "Crew app exists; review its installation status.", flash[:notice]
  end

  private

  def fields
    { id: "alpha", name: "Alpha", crew_id: "engineer", description: "Helper", system_prompt: "Investigate carefully.",
      default_models: { codex: "model-a", claude: "model-b" }, role_oids: [ roles(:acme_infra).oid ],
      skills: { "0" => { name: "reviewing-incidents", description: "Reviews incidents when asked.", content: "Follow the runbook." } } }
  end

  def login(user)
    post login_url, params: { email: user.email, password: "password123456" }
  end
end
