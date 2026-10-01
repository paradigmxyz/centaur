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
      calls << [ :list ]
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
      {}
    end
  end

  setup do
    @client = FakeClient.new
    Console::CrewController.client_factory = -> { @client }
    @public_url = ENV["CENTAUR_CONSOLE_SLACK_CREW_PUBLIC_URL"]
    ENV["CENTAUR_CONSOLE_SLACK_CREW_PUBLIC_URL"] = "https://slackbot.example"
    login(users(:acme_admin))
  end

  teardown do
    Console::CrewController.client_factory = -> { SlackCrewClient.new }
    @public_url ? ENV["CENTAUR_CONSOLE_SLACK_CREW_PUBLIC_URL"] = @public_url : ENV.delete("CENTAUR_CONSOLE_SLACK_CREW_PUBLIC_URL")
  end

  test "requires an active acting admin" do
    delete logout_url
    get console_crew_index_url
    assert_redirected_to login_path

    login(users(:member_user))
    get console_crew_index_url
    assert_redirected_to console_integrations_path
    assert_empty @client.calls

    delete logout_url
    login(users(:acme_admin))
    post console_descope_url
    get console_crew_index_url
    assert_redirected_to console_integrations_path
    assert_empty @client.calls

    delete logout_url
    get console_crew_index_url
    assert_redirected_to login_path
  end

  test "renders empty state and unavailable service" do
    get console_crew_index_url
    assert_response :ok
    assert_select "td", text: "No Crew bots yet."

    @client.error = SlackCrewClient::Error.new("Crew service is unavailable")
    get console_crew_index_url
    assert_response :bad_gateway
    assert_select "div", text: /Crew service is unavailable/
  end

  test "a disabled account cannot access Crew" do
    users(:acme_admin).update!(status: :disabled)
    get console_crew_index_url
    assert_redirected_to login_path
    assert_empty @client.calls
  end

  test "creation immediately redirects to a validated external install URL" do
    @client.result["created"] = {
      "install_url" => "https://slackbot.example/api/slack/crew/alpha/install?ticket=fresh"
    }
    post console_crew_index_url, params: { crew: { id: "alpha", name: "Alpha", crew_id: "engineer", description: "Helper" } }

    assert_redirected_to "https://slackbot.example/api/slack/crew/alpha/install?ticket=fresh"
    assert_equal "Alpha", @client.calls.last.last["name"]
  end

  test "rejects install redirects with the wrong origin path or missing ticket" do
    [
      "https://evil.example/api/slack/crew/alpha/install?ticket=x",
      "https://slackbot.example/api/slack/crew/other/install?ticket=x",
      "https://slackbot.example/api/slack/crew/alpha/install?return_url=https://evil.example",
      "https://slackbot.example/api/slack/crew/alpha/install?ticket=x&return_url=https://evil.example"
    ].each do |url|
      @client.result["created"] = { "install_url" => url }
      post console_crew_index_url, params: { crew: { id: "alpha", name: "Alpha", crew_id: "engineer" } }
      assert_response :unprocessable_entity
      assert_select "input[name='crew[name]'][value='Alpha']"
    end
  end

  test "install action fetches a fresh URL and disables Turbo" do
    @client.result["crew"] = [ { "id" => "alpha", "status" => "needs_install", "install_url" => "https://slackbot.example/api/slack/crew/alpha/install?ticket=new" } ]
    post install_console_crew_url("alpha")
    assert_redirected_to "https://slackbot.example/api/slack/crew/alpha/install?ticket=new"

    get console_crew_index_url
    assert_select "form[data-turbo='false'][action=?]", install_console_crew_path("alpha")
  end

  test "casts paused while allowing only manageable fields" do
    patch console_crew_url("alpha"), params: { crew: { paused: "false", app_id: "forged", crew_id: "forged" } }
    assert_redirected_to console_crew_index_path
    assert_equal [ :update, "alpha", { "paused" => false } ], @client.calls.last
  end

  test "missing form fields render an error and an already installed duplicate returns to Crew" do
    post console_crew_index_url, params: {}
    assert_response :unprocessable_entity
    @client.result["created"] = { "id" => "alpha", "status" => "active" }
    post console_crew_index_url, params: { crew: { id: "alpha", name: "Alpha", crew_id: "engineer" } }
    assert_redirected_to console_crew_index_path
    assert_equal "Crew bot is already installed.", flash[:notice]
  end

  test "interrupted installation is not shown as active or offered pause and install controls" do
    @client.result["crew"] = [ { "id" => "alpha", "name" => "Alpha", "status" => "installing", "app_id" => "A123" } ]
    get console_crew_index_url
    assert_response :ok
    assert_select "td", text: "installing"
    assert_select "input[type=submit][value=Save][disabled]"
    assert_select "input[value=Pause]", count: 0
    assert_select "form[action=?]", install_console_crew_path("alpha"), count: 0
  end

  test "backend validation preserves creation fields" do
    @client.error = SlackCrewClient::Error.new("Name is invalid", status: 422)
    post console_crew_index_url, params: { crew: { id: "alpha", name: "Bad name", crew_id: "engineer", description: "Keep me" } }
    assert_response :unprocessable_entity
    assert_select "input[value='Bad name']"
    assert_select "input[value='Keep me']"
  end

  private

  def login(user)
    post login_url, params: { email: user.email, password: "password123456" }
  end
end
