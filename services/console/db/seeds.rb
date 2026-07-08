# This file should ensure the existence of records required to run the application in every environment (production,
# development, test). The code here should be idempotent so that it can be executed at any point in every environment.
# The data can then be loaded with the bin/rails db:seed command (or created alongside the database with db:setup).

# Sample OAuth apps for the Integrations page, one per supported provider.
# Dev/test only: the client ids and secrets are placeholders, so the consent
# flows they name will not complete against real providers -- they exist to
# exercise the console UI (Apps list, Integrations cards, start links).
unless Rails.env.production?
  seed_user = User.order(:id).first || User.create!(
    email: ConsoleEnv["INITIAL_USER_EMAIL"].presence || "dev@iron.local",
    password: ConsoleEnv["INITIAL_USER_PASSWORD"].presence || "dev-password-1234",
    status: "active",
    admin: true
  )

  [
    {
      slug: "attio",
      provider: "attio",
      description: "Attio workspace access for CRM records and object configuration",
      # Attio scopes are configured in the Attio developer dashboard; this
      # allowlist mirrors the dashboard configuration we expect: user
      # management read-only, everything else read-write.
      allowed_scopes: %w[
        user_management:read
        record_permission:read-write
        object_configuration:read-write
        list_entry:read-write
        list_configuration:read-write
        comment:read-write
        note:read-write
        task:read-write
        meeting:read-write
        call_recording:read-write
        webhook:read-write
        file:read-write
      ]
    },
    {
      slug: "google",
      provider: "google",
      description: "Google Workspace (Gmail, Calendar, Drive)",
      allowed_scopes: %w[
        https://www.googleapis.com/auth/gmail.readonly
        https://www.googleapis.com/auth/calendar.readonly
        https://www.googleapis.com/auth/drive.readonly
      ]
    },
    {
      slug: "slack",
      provider: "slack",
      description: "Slack workspace access for messages and channels",
      allowed_scopes: %w[chat:write channels:history channels:read users:read]
    },
    {
      slug: "github",
      provider: "github",
      description: "GitHub repositories and user profile",
      allowed_scopes: %w[repo read:user]
    },
    {
      slug: "granola",
      provider: "granola",
      description: "Granola MCP access",
      # A real client_id/client_secret comes from one-time dynamic registration:
      # POST https://mcp-auth.granola.ai/oauth2/register
      allowed_scopes: %w[mcp]
    },
    {
      slug: "linear",
      provider: "linear",
      description: "Linear workspace access for issues and comments",
      allowed_scopes: %w[read write]
    }
  ].each do |attrs|
    OauthApp.find_or_create_by!(slug: attrs[:slug]) do |app|
      app.provider = attrs[:provider]
      app.description = attrs[:description]
      app.allowed_scopes = attrs[:allowed_scopes]
      app.client_id = "seed-#{attrs[:slug]}-client-id"
      app.client_secret = "seed-#{attrs[:slug]}-client-secret"
      app.credential_namespace = "default"
      app.enabled = true
      app.created_by = seed_user
    end
  end
end
