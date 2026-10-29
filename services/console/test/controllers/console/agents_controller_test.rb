require "test_helper"

class Console::AgentsControllerTest < ActionDispatch::IntegrationTest
  # Stands in for CentaurApiClient's persona routes.
  class FakeApiClient
    def initialize(personas: [], default_persona_id: nil, error: nil)
      @personas = personas
      @default_persona_id = default_persona_id
      @error = error
    end

    def list_personas
      raise @error if @error

      summaries = @personas.map { |persona| persona.except("prompt") }
      { "ok" => true, "default_persona_id" => @default_persona_id, "personas" => summaries }
    end

    def get_persona(persona_id)
      raise @error if @error

      persona = @personas.find { |candidate| candidate["id"] == persona_id }
      raise CentaurApiClient::Error.new("persona is not deployed", status: 404) unless persona

      { "ok" => true, "persona" => persona }
    end
  end

  ENG = {
    "id" => "eng",
    "source_root" => "/repo/tools",
    "source_path" => "/repo/tools/personas/eng",
    "prompt_hash" => "sha256:0123456789abcdef",
    "is_default" => true,
    "public" => false,
    "harness" => "claudecode",
    "models" => { "claudecode" => "opus" },
    "prompt" => "# Engineering agent\n\nReview <b>carefully</b>."
  }.freeze

  OPS = {
    "id" => "ops.v2",
    "source_root" => "/repo/overlay/tools",
    "source_path" => "/repo/overlay/tools/personas/ops.v2",
    "prompt_hash" => "sha256:fedcba9876543210",
    "is_default" => false,
    "public" => false,
    "prompt" => "Ops prompt"
  }.freeze

  setup do
    @original_client_factory = Console::AgentsController.client_factory
    @original_channel_defaults_factory = Console::AgentsController.channel_defaults_factory
    with_api_client(FakeApiClient.new(personas: [ ENG, OPS ], default_persona_id: "eng"))
    with_channel_defaults(
      principals(:acme_channel).slack_channel_id => { "persona" => "ops.v2", "harness" => "claude", "reasoning" => "high" },
      "C0MISSING01" => { "persona" => "email" },
      "C0LOWONLY01" => { "reasoning" => "low" }
    )
    post login_url, params: { email: users(:acme_admin).email, password: "password123456" }
  end

  teardown do
    Console::AgentsController.client_factory = @original_client_factory
    Console::AgentsController.channel_defaults_factory = @original_channel_defaults_factory
  end

  test "an admin sees each persona with its channels and no prompt text" do
    get console_agents_url

    assert_response :ok
    assert_select ".console-thread-group-title-active", text: /Agents/
    assert_select "a[href=?]", console_agent_path("eng"), text: "eng"
    assert_select "a[href=?]", console_agent_path("ops.v2"), text: "ops.v2"
    assert_select "a[href=?]", console_principal_path(principals(:acme_channel).oid)
    assert_select ".role-pill", text: "default", count: 1
    assert_match "0123456789ab", response.body
    assert_no_match "Engineering agent", response.body
  end

  test "channels naming an undeployed persona are called out" do
    get console_agents_url

    assert_response :ok
    assert_match "C0MISSING01", response.body
    assert_match "→ email", response.body
    assert_no_match "C0LOWONLY01", response.body
  end

  test "the agent page renders the prompt as sanitized markdown and lists its channels" do
    get console_agent_url("eng")

    assert_response :ok
    assert_select "h1", text: "eng"
    assert_select "article.console-markdown h1", text: "Engineering agent"
    assert_select "dd", text: "/repo/tools/personas/eng"
    assert_select "dd", text: "claudecode: opus"
    assert_match "it is the default for channels without one", response.body

    get console_agent_url("ops.v2")

    assert_response :ok
    assert_select "a[href=?]", console_principal_path(principals(:acme_channel).oid)
    assert_select "td", text: "high"
  end

  test "an undeployed persona renders not found" do
    get console_agent_url("email")

    assert_response :not_found
    assert_match "did not load a persona named", response.body
    assert_select "article.console-markdown", count: 0
  end

  test "the pages degrade when the API is unavailable" do
    with_api_client(FakeApiClient.new(error: CentaurApiClient::Error.new("connection refused", status: 503)))

    get console_agents_url
    assert_response :ok
    assert_match "The Centaur API is unavailable", response.body

    get console_agent_url("eng")
    assert_response :ok
    assert_match "The Centaur API is unavailable", response.body
  end

  test "a non-admin cannot see agents or their prompts" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get console_agents_url
    assert_redirected_to console_integrations_path

    get console_agent_url("eng")
    assert_redirected_to console_integrations_path

    get console_integrations_url
    assert_select ".console-thread-group-title", text: /Agents/, count: 0
  end

  private

  def with_api_client(client)
    Console::AgentsController.client_factory = -> { client }
  end

  def with_channel_defaults(entries)
    defaults = SlackChannelDefaults.new(entries.to_json)
    Console::AgentsController.channel_defaults_factory = -> { defaults }
  end
end
