# OAuth apps slice. The Apps pages and the user-facing Integrations page live
# in the SPA (web/src/features/oauth-apps); Rails only keeps path helpers for
# redirects (the consent callback and the landing page).
direct(:console_oauth_apps) { "/console/oauth_apps" }
direct(:console_oauth_app) { |oid| "/console/oauth_apps/#{oid}" }
direct(:console_integrations) { "/console/integrations" }

# /api/v1 additions used by the OAuth apps and Integrations pages (see
# docs/API.md#oauth-apps and docs/API.md#integrations).
namespace :api do
  namespace :v1 do
    resources :oauth_apps, only: [] do
      collection { post "presets/:provider", action: :preset, as: :preset }
      member do
        get "credentials"
        get "provider_details"
      end
    end
    resources :oauth_providers, only: %i[index]
    resources :integrations, only: %i[index]
  end
end
