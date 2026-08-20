require "test_helper"

class Console::AuthoredWorkflowsControllerTest < ActionDispatch::IntegrationTest
  setup do
    @operator = users(:acme_admin)
    post login_url, params: { email: @operator.email, password: "password123456" }
  end

  test "renders the single-turn workflow form" do
    get new_console_authored_workflow_url

    assert_response :ok
    assert_select "textarea[name='authored_workflow[prompt]']"
    assert_select "select[name='authored_workflow[principal_oid]']"
    assert_select "select[name='authored_workflow[schedule_preset]']"
    assert_select "select[name='authored_workflow[timezone]']", count: 0
    assert_select "[data-schedule-fields-target=cron][hidden]"
    assert_select "input[name='authored_workflow[cron_expression]'][disabled]"
    assert_select "p", text: "All schedules use Pacific Time."
    assert_select "form[data-controller='slack-channel-autocomplete']"
    assert_select "[data-slack-channel-autocomplete-url-value=?]",
                  slack_channel_options_console_authored_workflows_path
    assert_select "input[type=hidden][name='authored_workflow[delivery_channel]'][data-slack-channel-autocomplete-target=value]"
    assert_select "input[role=combobox][placeholder='Search channels or enter an ID']"
    assert_select "input[type=submit][data-slack-channel-autocomplete-target=submit]:not([disabled])"
  end

  test "creates an author-principal workflow from a schedule preset" do
    assert_difference -> { AuthoredWorkflow.count }, 1 do
      post console_authored_workflows_url, params: { authored_workflow: workflow_params }
    end

    workflow = AuthoredWorkflow.order(:id).last
    assert_redirected_to console_workflows_path
    assert_equal @operator, workflow.author
    assert_nil workflow.principal
    assert_equal "0 9 * * 1-5", workflow.cron_expression
    assert_equal AuthoredWorkflow::DEFAULT_TIMEZONE, workflow.timezone
    assert_not_nil workflow.next_run_at
  end

  test "edit form preserves the selected delivery channel" do
    workflow = create_workflow

    get edit_console_authored_workflow_url(workflow.oid)

    assert_response :ok
    assert_select "input[type=hidden][name='authored_workflow[delivery_channel]'][value=?]", workflow.delivery_channel
    assert_select "input[role=combobox][value=?]", workflow.delivery_channel
  end

  test "edit form shows the cron expression for a cron schedule" do
    workflow = create_workflow
    workflow.update!(cron_expression: "15 6 * * *")

    get edit_console_authored_workflow_url(workflow.oid)

    assert_response :ok
    assert_select "option[value=cron][selected]"
    assert_select "[data-schedule-fields-target=cron]:not([hidden])"
    assert_select "input[name='authored_workflow[cron_expression]'][required]:not([disabled])"
  end

  test "shows authored workflows on the workflow dashboard" do
    workflow = create_workflow
    catalog = SlackChannelCatalog::Result.new(
      channels: [ SlackChannelCatalog::Channel.new(id: workflow.delivery_channel, name: "general", private: false) ],
      error: nil,
      configured: true
    )

    SlackChannelCatalogProvider.stub(:fetch, catalog) do
      CentaurWorkflowRun.stub(:available?, false) do
        get console_workflows_url
      end
    end

    assert_response :ok
    assert_select "a[href=?]", edit_console_authored_workflow_path(workflow.oid), text: workflow.name
    assert_select "td", text: /Hourly/
    assert_select "td", text: /#general/
    assert_select "td", text: /#{workflow.delivery_channel}/
    assert_no_match workflow.timezone, response.body
  end

  test "creates a workflow with a defined principal" do
    principal = principals(:acme_channel)
    params = workflow_params.merge(principal_oid: principal.oid)

    post console_authored_workflows_url, params: { authored_workflow: params }

    assert_response :redirect
    assert_equal principal, AuthoredWorkflow.order(:id).last.principal
  end

  test "rejects an unavailable principal selection" do
    assert_no_difference -> { AuthoredWorkflow.count } do
      post console_authored_workflows_url, params: {
        authored_workflow: workflow_params.merge(principal_oid: "prn_missing")
      }
    end

    assert_response :unprocessable_entity
    assert_match "Principal is unavailable", response.body
  end

  test "queues a manual run" do
    workflow = create_workflow

    assert_enqueued_jobs 1, only: AuthoredWorkflowRunJob do
      post run_console_authored_workflow_url(workflow.oid)
    end

    assert_redirected_to console_workflows_path
  end

  test "non-admins cannot author workflows" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    assert_no_difference -> { AuthoredWorkflow.count } do
      post console_authored_workflows_url, params: { authored_workflow: workflow_params }
    end

    assert_redirected_to console_threads_path
  end

  private

  def workflow_params
    {
      name: "Weekday incident summary",
      prompt: "Summarize open incidents.",
      principal_oid: "",
      delivery_channel: "C0123456789",
      schedule_preset: "weekdays",
      cron_expression: "",
      timezone: "America/Denver",
      enabled: "1"
    }
  end

  def create_workflow
    AuthoredWorkflow.create!(
      name: "Manual run",
      prompt: "Summarize open incidents.",
      author: @operator,
      principal: principals(:acme_channel),
      delivery_channel: "C0123456789",
      cron_expression: "0 * * * *",
      enabled: true
    )
  end
end
