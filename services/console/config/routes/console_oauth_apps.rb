# OAuth apps slice: app list/detail/forms, provider presets, and the
# user-facing Integrations page (HTML console routes).

post "console/oauth_apps/presets/:provider", to: "console/oauth_apps#preset", as: :console_oauth_app_preset
get "console/oauth_apps", to: "console#oauth_apps", as: :console_oauth_apps
# User-facing list of enabled OAuth apps and their consent start links. Not
# admin-gated: any signed-in team member connects integrations from here.
get "console/integrations", to: "console/integrations#index", as: :console_integrations
# Create/edit forms for OAuth apps. Declared before the show route so
# /console/oauth_apps/new wins over the generic `:id` match. Named
# `*_oauth_app_form*` so the form helpers don't collide with the read
# (list/show) routes below, which keep the clean `console_oauth_app(s)` names.
namespace :console do
  resources :oauth_apps, only: %i[new create edit update], as: :oauth_app_forms
end
get "console/oauth_apps/:id", to: "console#oauth_app", as: :console_oauth_app
