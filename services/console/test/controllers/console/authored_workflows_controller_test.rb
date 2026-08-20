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
    assert_select "input[name='authored_workflow[delivery_channel]']"
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
    assert_not_nil workflow.next_run_at
  end

  test "shows authored workflows on the workflow dashboard" do
    workflow = create_workflow

    CentaurWorkflowRun.stub(:available?, false) do
      get console_workflows_url
    end

    assert_response :ok
    assert_select "a[href=?]", edit_console_authored_workflow_path(workflow.oid), text: workflow.name
    assert_match workflow.delivery_channel, response.body
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
      timezone: "UTC",
      enabled: true
    )
  end
end
