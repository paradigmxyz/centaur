require "test_helper"

module GoogleDocs
  class SyncCredentialTest < ActiveSupport::TestCase
    def google_app
      OauthApp.create!(
        provider: "google",
        slug: "google-docs-#{SecureRandom.hex(6)}",
        client_id: "google-client-#{SecureRandom.hex(4)}",
        client_secret: "secret",
        allowed_scopes: [
          GoogleDocs::SyncCredential::DRIVE_METADATA_SCOPE,
          GoogleDocs::SyncCredential::DOCS_READONLY_SCOPE
        ],
        created_by: users(:acme_admin)
      )
    end

    def credential
      @credential ||= BrokerCredential.create!(
        oauth_app: google_app,
        foreign_id: "google-docs-#{SecureRandom.hex(6)}",
        token_endpoint: "https://oauth2.googleapis.com/token",
        access_token: "ya29.live",
        refresh_token: "refresh",
        last_refresh: Time.current,
        expires_at: 1.hour.from_now,
        scopes: [
          GoogleDocs::SyncCredential::DRIVE_METADATA_SCOPE,
          GoogleDocs::SyncCredential::DOCS_READONLY_SCOPE
        ],
        provider_subject: "google-sub-alice",
        provider_email: "alice@example.com"
      )
    end

    test "oauth_app_slug defaults to google and honors console env prefix" do
      env_key = "CENTAUR_CONSOLE_GOOGLE_DOCS_SYNC_OAUTH_APP_SLUG"
      legacy_env_key = "IRON_CONTROL_GOOGLE_DOCS_SYNC_OAUTH_APP_SLUG"
      previous = {
        env_key => ENV[env_key],
        legacy_env_key => ENV[legacy_env_key]
      }
      ENV.delete(env_key)
      ENV.delete(legacy_env_key)

      assert_equal "google", GoogleDocs::SyncCredential.oauth_app_slug

      ENV[env_key] = "custom-google"
      assert_equal "custom-google", GoogleDocs::SyncCredential.oauth_app_slug
    ensure
      previous.each do |key, value|
        if value.nil?
          ENV.delete(key)
        else
          ENV[key] = value
        end
      end
    end

    test "required scopes allow drive readonly alone or metadata plus docs readonly" do
      assert GoogleDocs::SyncCredential.required_scopes_granted?([
        GoogleDocs::SyncCredential::DRIVE_METADATA_SCOPE,
        GoogleDocs::SyncCredential::DOCS_READONLY_SCOPE
      ])
      assert GoogleDocs::SyncCredential.required_scopes_granted?([
        GoogleDocs::SyncCredential::DRIVE_READONLY_SCOPE
      ])
      refute GoogleDocs::SyncCredential.required_scopes_granted?([
        GoogleDocs::SyncCredential::DRIVE_METADATA_SCOPE
      ])
      refute GoogleDocs::SyncCredential.required_scopes_granted?([
        GoogleDocs::SyncCredential::DOCS_READONLY_SCOPE
      ])
    end

    test "uses bounded Drive file and change pages" do
      calls = []
      google_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "ya29.live", access_token
        calls << [ endpoint, params ]
        case endpoint
        when GoogleDocs::SyncCredential::START_PAGE_TOKEN_ENDPOINT
          { "startPageToken" => "change-100" }
        when GoogleDocs::SyncCredential::FILES_LIST_ENDPOINT
          { "files" => [], "nextPageToken" => "file-2" }
        when GoogleDocs::SyncCredential::CHANGES_LIST_ENDPOINT
          { "changes" => [], "newStartPageToken" => "change-101" }
        else
          flunk "unexpected Google endpoint #{endpoint}"
        end
      end
      sync = GoogleDocs::SyncCredential.new(credential, google_api_http: google_http)

      assert_equal "change-100", sync.start_page_token
      assert_equal "file-2", sync.list_files_page["nextPageToken"]
      assert_equal "change-101", sync.list_changes_page(page_token: "change-100")["newStartPageToken"]

      files_params = calls.find { |endpoint, _| endpoint == GoogleDocs::SyncCredential::FILES_LIST_ENDPOINT }.last
      assert_equal 100, files_params["pageSize"]
      assert_includes files_params["q"], "trashed = false"
      changes_params = calls.find { |endpoint, _| endpoint == GoogleDocs::SyncCredential::CHANGES_LIST_ENDPOINT }.last
      assert_equal "change-100", changes_params["pageToken"]
      assert_equal "true", changes_params["includeRemoved"]
    end

    test "classifies a rejected Drive page token for crawl recovery" do
      response = HttpClient::Response.new(
        status: 404,
        body: {
          error: {
            code: 404,
            message: "Page token expired",
            errors: [ { reason: "notFound", location: "pageToken" } ]
          }
        }.to_json,
        headers: {}
      )
      api = Object.new
      api.define_singleton_method(:get) { |*, **| response }
      sync = GoogleDocs::SyncCredential.new(credential)

      HttpClient.stub(:new, api) do
        assert_raises(GoogleDocs::SyncCredential::InvalidPageTokenError) do
          sync.list_changes_page(page_token: "rejected-token")
        end
      end
    end

    test "normalizes canonical content without credential-specific metadata" do
      file = google_doc
      google_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "#{GoogleDocs::SyncCredential::DOCS_GET_ENDPOINT}/doc-123", endpoint
        assert_equal({ "includeTabsContent" => "true" }, params)
        assert_equal "ya29.live", access_token
        {
          "title" => "Launch Plan",
          "body" => {
            "content" => [
              {
                "paragraph" => {
                  "elements" => [
                    { "textRun" => { "content" => "Ship the Google Docs ingest flow.\n" } }
                  ]
                }
              }
            ]
          }
        }
      end
      sync = GoogleDocs::SyncCredential.new(credential, google_api_http: google_http)

      batch = sync.document_batch(file)

      assert_equal "Ship the Google Docs ingest flow.\n", batch[:contents].first[:text_content]
      assert_equal "7", batch[:contents].first[:source_version]
      assert_equal "google_docs:doc-123:chunk-0000", batch[:context_documents].first[:document_id]
      assert_equal({ source: "google_docs" }, batch[:context_documents].first[:metadata])
      refute_includes batch[:context_documents].first[:metadata], :broker_credential_id
    end

    private

    def google_doc
      {
        "id" => "doc-123",
        "name" => "Launch Plan",
        "mimeType" => GoogleDocs::SyncCredential::GOOGLE_DOC_MIME_TYPE,
        "webViewLink" => "https://docs.google.com/document/d/doc-123/edit",
        "driveId" => "drive-1",
        "owners" => [
          {
            "permissionId" => "perm-owner",
            "displayName" => "Alice",
            "emailAddress" => "alice@example.com"
          }
        ],
        "capabilities" => { "canEdit" => true },
        "trashed" => false,
        "createdTime" => "2026-06-01T12:00:00Z",
        "modifiedTime" => "2026-06-02T12:00:00Z",
        "version" => "7"
      }
    end
  end
end
