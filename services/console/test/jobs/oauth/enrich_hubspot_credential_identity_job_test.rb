require "test_helper"

module Oauth
  class EnrichHubspotCredentialIdentityJobTest < ActiveJob::TestCase
    setup do
      Oauth::EnrichHubspotCredentialIdentityJob.hubspot_api_http = nil
    end

    teardown do
      Oauth::EnrichHubspotCredentialIdentityJob.hubspot_api_http = nil
    end

    def hubspot_credential(**overrides)
      app = oauth_apps(:acme_hubspot)
      app.update!(client_secret: "hubspot-secret")
      BrokerCredential.create!({
        namespace: "acme",
        foreign_id: "hubspot-hubspot-pending-abc123",
        name: "HubSpot – Pending HubSpot account",
        token_endpoint: Oauth::Providers::Hubspot::TOKEN_ENDPOINT,
        oauth_app: app,
        provider_subject: "pending-abc123",
        access_token: "hub-token",
        refresh_token: "hub-refresh",
        scopes: %w[oauth crm.objects.contacts.read]
      }.merge(overrides))
    end

    def wrap_credential(credential, name: "#{credential.name} token")
      StaticSecret.create!(
        namespace: credential.namespace,
        name: name,
        broker_credential: credential,
        inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }
      )
    end

    test "updates the credential and wrapper secret names from HubSpot token metadata" do
      Oauth::EnrichHubspotCredentialIdentityJob.hubspot_api_http = ->(url:, access_token:) {
        assert_equal Oauth::EnrichHubspotCredentialIdentityJob::ACCESS_TOKEN_INFO_ENDPOINT, url
        assert_equal "hub-token", access_token
        {
          "hub_id" => 12_345,
          "user_id" => 67_890,
          "user" => "rep@example.com",
          "hub_domain" => "example.hubspot.com"
        }
      }
      credential = hubspot_credential
      secret = wrap_credential(credential)

      Oauth::EnrichHubspotCredentialIdentityJob.perform_now(credential.id)

      assert_equal "HubSpot – rep@example.com", credential.reload.name
      assert_equal "12345:67890", credential.provider_subject
      assert_equal "rep@example.com", credential.provider_email
      assert_equal "hubspot-hubspot-12345:67890", credential.foreign_id
      assert_equal "12345", credential.labels["hubspot_hub_id"]
      assert_equal "67890", credential.labels["hubspot_user_id"]
      assert_equal "example.hubspot.com", credential.labels["hubspot_hub_domain"]
      assert_equal "HubSpot – rep@example.com token", secret.reload.name
    end

    test "does not clobber an operator-renamed wrapper secret" do
      Oauth::EnrichHubspotCredentialIdentityJob.hubspot_api_http = ->(url:, access_token:) {
        { "hub_id" => 1, "user_id" => 2, "user" => "rep@example.com" }
      }
      credential = hubspot_credential
      secret = wrap_credential(credential, name: "operator name")

      Oauth::EnrichHubspotCredentialIdentityJob.perform_now(credential.id)

      assert_equal "HubSpot – rep@example.com", credential.reload.name
      assert_equal "operator name", secret.reload.name
    end
  end
end
