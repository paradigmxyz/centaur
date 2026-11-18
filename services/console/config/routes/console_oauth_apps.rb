# Path helpers for server-side redirects (the consent callback and the landing page).
direct(:console_oauth_apps) { "/console/oauth_apps" }
direct(:console_oauth_app) { |oid| "/console/oauth_apps/#{oid}" }
direct(:console_integrations) { "/console/integrations" }

namespace :api do
  namespace :v1 do
    namespace :console do
      resources :oauth_apps, only: %i[index show create update destroy] do
        collection { post "presets/:provider", action: :preset, as: :preset }
        member do
          get "credentials"
          get "provider_details"
        end
      end
      resources :oauth_providers, only: %i[index]
      resources :integrations, only: %i[index] do
        collection { delete "connections/:id", action: :disconnect, as: :connection }
      end
    end
  end
end
