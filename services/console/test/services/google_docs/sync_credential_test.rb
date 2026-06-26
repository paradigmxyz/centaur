require "test_helper"

module GoogleDocs
  class SyncCredentialTest < ActiveSupport::TestCase
    class FakeApiClient
      attr_reader :batch

      def get_google_docs_sync_checkpoint(broker_credential_id:)
        {
          "checkpoint" => {
            "broker_credential_id" => broker_credential_id,
            "last_incremental_sync_at" => "2026-06-01T00:00:00Z"
          }
        }
      end

      def ingest_google_docs_sync_batch(payload)
        @batch = payload
        { "ok" => true }
      end
    end

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
        credential_namespace: "acme",
        created_by: users(:acme_admin)
      )
    end

    def credential
      @credential ||= BrokerCredential.create!(
        oauth_app: google_app,
        namespace: "acme",
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

    test "required scopes allow drive readonly or metadata plus docs readonly" do
      assert GoogleDocs::SyncCredential.required_scopes_granted?([
        GoogleDocs::SyncCredential::DRIVE_METADATA_SCOPE,
        GoogleDocs::SyncCredential::DOCS_READONLY_SCOPE
      ])
      assert GoogleDocs::SyncCredential.required_scopes_granted?([
        GoogleDocs::SyncCredential::DRIVE_READONLY_SCOPE,
        GoogleDocs::SyncCredential::DOCS_READONLY_SCOPE
      ])
      refute GoogleDocs::SyncCredential.required_scopes_granted?([
        GoogleDocs::SyncCredential::DRIVE_METADATA_SCOPE
      ])
    end

    test "sync normalizes files observations contents context docs and checkpoint" do
      api_client = FakeApiClient.new
      google_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "ya29.live", access_token
        case endpoint
        when GoogleDocs::SyncCredential::FILES_LIST_ENDPOINT
          assert_includes params["q"], "modifiedTime > '2026-06-01T00:00:00Z'"
          {
            "files" => [
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
                "lastModifyingUser" => { "displayName" => "Bob" },
                "capabilities" => { "canEdit" => true },
                "labelInfo" => { "labels" => [] },
                "trashed" => false,
                "explicitlyTrashed" => false,
                "createdTime" => "2026-06-01T12:00:00Z",
                "modifiedTime" => "2026-06-02T12:00:00Z",
                "version" => "7"
              }
            ],
            "nextPageToken" => ""
          }
        when "#{GoogleDocs::SyncCredential::DOCS_GET_ENDPOINT}/doc-123"
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
        else
          flunk "unexpected Google endpoint #{endpoint}"
        end
      end

      GoogleDocs::SyncCredential.new(
        credential,
        api_client: api_client,
        google_api_http: google_http
      ).call

      batch = api_client.batch
      assert_equal "completed", batch[:run][:status]
      assert_equal credential.oid, batch[:run][:broker_credential_id]
      assert_equal "google-sub-alice", batch[:run][:provider_subject]
      assert_equal 1, batch[:files].length
      assert_equal "doc-123", batch[:files].first[:file_id]
      assert_equal "writer", batch[:observations].first[:role_hint]
      assert_equal "Ship the Google Docs ingest flow.\n", batch[:contents].first[:text_content]
      assert_equal "google_docs:doc-123:chunk-0000", batch[:context_documents].first[:document_id]
      assert_equal "Launch Plan", batch[:context_documents].first[:title]
      assert_equal "2026-06-02T12:00:00Z", batch[:checkpoint][:last_incremental_sync_at]
      assert_equal "", batch[:checkpoint][:last_error]
    end
  end
end
