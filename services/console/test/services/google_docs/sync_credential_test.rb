require "test_helper"

module GoogleDocs
  class SyncCredentialTest < ActiveSupport::TestCase
    PDF_INDEXING_ENABLED_ENV = "CENTAUR_CONSOLE_GOOGLE_DRIVE_PDF_INDEXING_ENABLED"

    setup do
      @previous_pdf_indexing_enabled = ENV[PDF_INDEXING_ENABLED_ENV]
      ENV[PDF_INDEXING_ENABLED_ENV] = "true"
    end

    teardown do
      if @previous_pdf_indexing_enabled.nil?
        ENV.delete(PDF_INDEXING_ENABLED_ENV)
      else
        ENV[PDF_INDEXING_ENABLED_ENV] = @previous_pdf_indexing_enabled
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

    test "syncable credentials centralize app, token, and scope eligibility" do
      current_credential = credential
      app = current_credential.oauth_app

      assert GoogleDocs::SyncCredential.syncable?(current_credential, oauth_app_slug: app.slug)
      refute GoogleDocs::SyncCredential.syncable?(current_credential, oauth_app_slug: "another-app")

      app.update!(enabled: false)
      refute GoogleDocs::SyncCredential.syncable?(current_credential.reload)
    end

    test "uses bounded user-corpus Drive file and change pages" do
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

      assert_equal "change-100", sync.user_start_page_token
      assert_equal "file-2", sync.list_user_files_page["nextPageToken"]
      assert_equal "change-101", sync.list_user_changes_page(page_token: "change-100")["newStartPageToken"]

      start_token_params = calls.find do |endpoint, _|
        endpoint == GoogleDocs::SyncCredential::START_PAGE_TOKEN_ENDPOINT
      end.last
      assert_equal "true", start_token_params["supportsAllDrives"]
      refute_includes start_token_params, "driveId"
      files_params = calls.find { |endpoint, _| endpoint == GoogleDocs::SyncCredential::FILES_LIST_ENDPOINT }.last
      assert_equal 100, files_params["pageSize"]
      assert_equal "user", files_params["corpora"]
      assert_equal "true", files_params["includeItemsFromAllDrives"]
      assert_equal "true", files_params["supportsAllDrives"]
      refute_includes files_params, "driveId"
      assert_includes files_params["q"], "trashed = false"
      assert_includes files_params["q"], GoogleDocs::SyncCredential::GOOGLE_DOC_MIME_TYPE
      refute_includes files_params["q"], GoogleDocs::SyncCredential::PDF_MIME_TYPE
      changes_params = calls.find { |endpoint, _| endpoint == GoogleDocs::SyncCredential::CHANGES_LIST_ENDPOINT }.last
      assert_equal "change-100", changes_params["pageToken"]
      assert_equal "true", changes_params["includeRemoved"]
      assert_equal "true", changes_params["includeItemsFromAllDrives"]
      assert_equal "true", changes_params["supportsAllDrives"]
      refute_includes changes_params, "driveId"
      assert_includes changes_params["fields"], "changes(changeType,driveId,fileId,removed"
    end

    test "classifies a rejected Drive page token for crawl recovery" do
      response = HttpClient::Response.new(
        status: 400,
        body: { error: { message: "Bad request" } }.to_json,
        headers: {}
      )
      api = Object.new
      api.define_singleton_method(:get) { |*, **| response }
      sync = GoogleDocs::SyncCredential.new(credential)

      HttpClient.stub(:new, api) do
        assert_raises(GoogleDocs::SyncCredential::InvalidPageTokenError) do
          sync.list_user_changes_page(page_token: "rejected-token")
        end
      end
    end

    test "does not classify a rejected files page token as a Changes cursor failure" do
      response = HttpClient::Response.new(
        status: 400,
        body: { error: { message: "Page token expired" } }.to_json,
        headers: {}
      )
      api = Object.new
      api.define_singleton_method(:get) { |*, **| response }
      sync = GoogleDocs::SyncCredential.new(credential)

      HttpClient.stub(:new, api) do
        error = assert_raises(GoogleDocs::SyncCredential::GoogleApiError) do
          sync.list_user_pdfs_page(page_token: "rejected-token")
        end
        refute_kind_of GoogleDocs::SyncCredential::InvalidPageTokenError, error
      end
    end

    test "uses an extended read timeout for Google API fetches" do
      response = HttpClient::Response.new(
        status: 200,
        body: { "startPageToken" => "change-100" }.to_json,
        headers: {}
      )
      api = Object.new
      api.define_singleton_method(:get) { |*, **| response }
      factory = lambda do |read_timeout:|
        assert_equal GoogleDocs::SyncCredential::FETCH_READ_TIMEOUT_SECONDS, read_timeout
        api
      end
      sync = GoogleDocs::SyncCredential.new(credential)

      HttpClient.stub(:new, factory) do
        assert_equal "change-100", sync.user_start_page_token
      end
    end

    test "classifies transient network failures for job retries" do
      [
        Net::ReadTimeout.new("read timed out"),
        Net::OpenTimeout.new("open timed out"),
        SocketError.new("host unavailable"),
        Socket::ResolutionError.new("temporary DNS failure"),
        Errno::ECONNRESET.new
      ].each do |network_error|
        google_http = ->(**) { raise network_error }
        sync = GoogleDocs::SyncCredential.new(credential, google_api_http: google_http)

        error = assert_raises(GoogleDocs::SyncCredential::GoogleApiError) do
          sync.user_start_page_token
        end

        assert_equal network_error, error.cause
        assert_includes error.message, network_error.class.name
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

    test "downloads, extracts, and chunks Drive PDFs" do
      credential.update!(scopes: [ GoogleDocs::SyncCredential::DRIVE_READONLY_SCOPE ])
      downloaded_pdf = "%PDF fixture bytes".b
      google_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "#{GoogleDocs::SyncCredential::FILES_LIST_ENDPOINT}/pdf-123", endpoint
        assert_equal({ "alt" => "media", "supportsAllDrives" => "true" }, params)
        assert_equal "ya29.live", access_token
        downloaded_pdf
      end
      extracted_text = "x" * (GoogleDocs::SyncCredential.chunk_chars + 1)
      temporary_path = nil
      extractor = lambda do |path|
        temporary_path = path
        assert_equal downloaded_pdf, File.binread(path)
        extracted_text
      end
      sync = GoogleDocs::SyncCredential.new(
        credential,
        google_api_http: google_http,
        pdf_text_extractor: extractor
      )
      file = google_doc.merge(
        "id" => "pdf-123",
        "name" => "Board Pack.pdf",
        "mimeType" => GoogleDocs::SyncCredential::PDF_MIME_TYPE
      )

      assert sync.eligible_file?(file)
      batch = sync.document_batch(file)

      refute File.exist?(temporary_path)
      assert_equal extracted_text, batch[:contents].first[:text_content]
      assert_equal GoogleDocs::SyncCredential::PDF_MIME_TYPE, batch[:contents].first[:export_mime_type]
      assert_equal 2, batch[:context_documents].length
      assert_equal "google_docs:pdf-123:chunk-0000", batch[:context_documents].first[:document_id]
      assert_equal "google_docs:pdf-123:chunk-0001", batch[:context_documents].second[:document_id]
      assert_equal GoogleDocs::SyncCredential::PDF_MIME_TYPE, batch[:context_documents].first[:mime_type]
      assert_equal({ source: "google_docs" }, batch[:context_documents].first[:metadata])
    end

    test "does not create chunks for PDFs without embedded text" do
      credential.update!(scopes: [ GoogleDocs::SyncCredential::DRIVE_READONLY_SCOPE ])
      sync = GoogleDocs::SyncCredential.new(
        credential,
        google_api_http: ->(**) { "%PDF fixture bytes".b },
        pdf_text_extractor: ->(*) { "" }
      )
      file = google_doc.merge(
        "id" => "pdf-123",
        "name" => "Scanned.pdf",
        "mimeType" => GoogleDocs::SyncCredential::PDF_MIME_TYPE
      )

      batch = sync.document_batch(file)

      assert_empty batch[:context_documents]
      assert_equal "", batch.dig(:contents, 0, :text_content)
    end

    test "rejects PDFs over the configured size limit before extraction" do
      credential.update!(scopes: [ GoogleDocs::SyncCredential::DRIVE_READONLY_SCOPE ])
      extractor = ->(*) { flunk "oversized PDF should not be extracted" }
      sync = GoogleDocs::SyncCredential.new(
        credential,
        google_api_http: ->(**) { "123456" },
        pdf_text_extractor: extractor,
        max_pdf_bytes: 5
      )
      file = google_doc.merge(
        "id" => "pdf-123",
        "name" => "Board Pack.pdf",
        "mimeType" => GoogleDocs::SyncCredential::PDF_MIME_TYPE
      )

      error = assert_raises(GoogleDocs::SyncCredential::PdfTooLargeError) do
        sync.document_batch(file)
      end

      assert_equal "PDF exceeds the 50 MB indexing limit", error.message

      failure = sync.content_failure_batch(file, error)
      assert_equal "7", failure.dig(:contents, 0, :source_version)
      assert_includes failure.dig(:contents, 0, :last_error), "PdfTooLargeError"
      assert_empty failure[:context_documents]
      assert failure[:replace_context_documents]
    end

    test "stops a streaming PDF download when chunks cross the size limit" do
      credential.update!(scopes: [ GoogleDocs::SyncCredential::DRIVE_READONLY_SCOPE ])
      response = Object.new
      response.define_singleton_method(:code) { "200" }
      response.define_singleton_method(:[]) { |_name| nil }
      response.define_singleton_method(:read_body) do |&block|
        block.call("123")
        block.call("456")
      end
      http = Object.new
      http.define_singleton_method(:use_ssl=) { |_| }
      http.define_singleton_method(:open_timeout=) { |_| }
      http.define_singleton_method(:read_timeout=) { |_| }
      http.define_singleton_method(:request) { |_request, &block| block.call(response) }
      sync = GoogleDocs::SyncCredential.new(credential, max_pdf_bytes: 5)
      file = google_doc.merge(
        "id" => "pdf-123",
        "name" => "Board Pack.pdf",
        "mimeType" => GoogleDocs::SyncCredential::PDF_MIME_TYPE
      )

      Net::HTTP.stub(:new, http) do
        assert_raises(GoogleDocs::SyncCredential::PdfTooLargeError) do
          sync.document_batch(file)
        end
      end
    end

    test "does not list PDFs when PDF indexing is disabled" do
      credential.update!(scopes: [ GoogleDocs::SyncCredential::DRIVE_READONLY_SCOPE ])
      ENV[PDF_INDEXING_ENABLED_ENV] = "false"
      requested_params = nil
      google_http = lambda do |params:, **|
        requested_params = params
        { "files" => [] }
      end

      GoogleDocs::SyncCredential.new(credential, google_api_http: google_http).list_user_files_page

      assert_includes requested_params["q"], GoogleDocs::SyncCredential::GOOGLE_DOC_MIME_TYPE
      refute_includes requested_params["q"], GoogleDocs::SyncCredential::PDF_MIME_TYPE
    end

    test "lists PDFs when the credential grants Drive content access" do
      credential.update!(scopes: [ GoogleDocs::SyncCredential::DRIVE_READONLY_SCOPE ])
      requested_params = nil
      google_http = lambda do |endpoint:, params:, **|
        assert_equal GoogleDocs::SyncCredential::FILES_LIST_ENDPOINT, endpoint
        requested_params = params
        { "files" => [] }
      end

      GoogleDocs::SyncCredential.new(credential, google_api_http: google_http).list_user_files_page

      assert_includes requested_params["q"], GoogleDocs::SyncCredential::GOOGLE_DOC_MIME_TYPE
      assert_includes requested_params["q"], GoogleDocs::SyncCredential::PDF_MIME_TYPE
    end

    test "truncates names sent to the sync API while preserving the raw payload" do
      file = google_doc.merge("name" => "a#{"📄" * (GoogleDocs::SyncCredential::NAME_MAX_BYTES / 4)}")
      sync = GoogleDocs::SyncCredential.new(credential)

      file_payload = sync.file_payload(file)
      observation_payload = sync.observation_payload(file, source: "full")

      expected_name = "a#{"📄" * ((GoogleDocs::SyncCredential::NAME_MAX_BYTES / 4) - 1)}"
      assert_equal expected_name, file_payload[:name]
      assert_equal expected_name, observation_payload[:observed_name]
      assert_operator file_payload[:name].bytesize, :<=, GoogleDocs::SyncCredential::NAME_MAX_BYTES
      assert_predicate file_payload[:name], :valid_encoding?
      assert_equal file, file_payload[:raw_payload]
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
