require "test_helper"

class Console::EtlsControllerTest < ActionDispatch::IntegrationTest
  class FakeClient
    attr_reader :calls

    def initialize
      @calls = []
      @imports = []
    end

    def imports=(imports)
      @imports = imports
    end

    def list_slack_archive_imports(limit:)
      @calls << [ :list, limit ]
      { "imports" => @imports }
    end

    def create_slack_archive_import(**kwargs)
      @calls << [ :create, kwargs ]
      {
        "ok" => true,
        "import" => { "import_id" => "sai_test", "status" => "upload_pending" },
        "upload" => { "upload_url" => "https://uploads.example/archive.zip" }
      }
    end

    def start_slack_archive_import(import_id)
      @calls << [ :start, import_id ]
      { "ok" => true }
    end

    def retry_slack_archive_import(import_id)
      @calls << [ :retry, import_id ]
      { "ok" => true }
    end

    def delete_slack_archive_import(import_id)
      @calls << [ :delete, import_id ]
      { "ok" => true }
    end
  end

  setup do
    @operator = users(:acme_admin)
    @client = FakeClient.new
    Console::EtlsController.client_factory = -> { @client }
    post login_url, params: { email: @operator.email, password: "password123456" }
  end

  teardown do
    Console::EtlsController.client_factory = -> { CentaurApiClient.new }
  end

  test "redirects to login when not signed in" do
    delete logout_url
    get console_etls_url
    assert_redirected_to login_path
  end

  test "an active non-admin is redirected away from the Data Sync page" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get console_etls_url
    assert_redirected_to console_threads_path
    assert_nil flash[:alert]
    # The gate fires before the action, so the api client is never touched.
    assert_empty @client.calls

    post console_slack_archive_imports_url, params: { filename: "export.zip" }
    assert_redirected_to console_threads_path
    assert_empty @client.calls
  end

  test "renders Slack archive imports on the Data Sync page" do
    @client.imports = [
      {
        "import_id" => "sai_uploaded",
        "original_filename" => "export.zip",
        "status" => "uploaded",
        "archive_uri" => "s3://bucket/key.zip",
        "file_size_bytes" => 12_345,
        "channels_imported" => 2,
        "users_imported" => 3,
        "messages_imported" => 4,
        "created_at" => "2026-06-23T18:00:00Z",
        "created_by" => "admin@example.com"
      },
      {
        "import_id" => "sai_failed",
        "original_filename" => "failed.zip",
        "status" => "failed",
        "archive_uri" => "s3://bucket/failed.zip",
        "error_text" => "bad zip"
      }
    ]

    get console_etls_url
    assert_response :ok
    assert_select "h1", text: "Data Sync"
    assert_select "nav a[href=?]", console_etls_path, text: "Data Sync"
    assert_select "td", text: /export\.zip/
    assert_select "th", text: "Workspace", count: 0
    assert_select "span", text: "uploaded"
    assert_select "span", text: "failed"
    assert_no_match "s3://bucket/key.zip", response.body
    assert_no_match "s3://bucket/failed.zip", response.body
    assert_select "form[action=?]", console_retry_slack_archive_import_path("sai_failed")
    assert_select "form[action=?]", console_delete_slack_archive_import_path("sai_failed")
  end

  test "create archive import returns the upload contract" do
    post console_slack_archive_imports_url,
         params: {
           filename: "export.zip",
           content_type: "application/zip"
         },
         as: :json

    assert_response :created
    body = JSON.parse(response.body)
    assert_equal "sai_test", body.dig("import", "import_id")
    assert_equal "https://uploads.example/archive.zip", body.dig("upload", "upload_url")
    create_call = @client.calls.find { |name, _| name == :create }
    assert_equal @operator.email, create_call.last.fetch(:created_by)
    assert_equal({ source: "centaur_console" }, create_call.last.fetch(:metadata))
  end

  test "start retry and delete actions call the API client" do
    post console_start_slack_archive_import_url("sai_start")
    assert_redirected_to console_etls_path
    post console_retry_slack_archive_import_url("sai_retry")
    assert_redirected_to console_etls_path
    delete console_delete_slack_archive_import_url("sai_delete")
    assert_redirected_to console_etls_path

    assert_includes @client.calls, [ :start, "sai_start" ]
    assert_includes @client.calls, [ :retry, "sai_retry" ]
    assert_includes @client.calls, [ :delete, "sai_delete" ]
  end
end
