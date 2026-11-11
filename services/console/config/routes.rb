Rails.application.routes.draw do
  # Reveal health status on /up that returns 200 if the app boots with no exceptions, otherwise 500.
  # Can be used by load balancers and uptime monitors to verify that the app is live.
  get "up" => "rails/health#show", as: :rails_health_check

  # One file per console area under config/routes/: its /api/v1 endpoints, the
  # Rails protocol routes (SSO, OAuth, MCP OAuth, PWA), and the few path
  # helpers server-side redirects use. Routes drawn here take precedence over
  # the SPA catch-all below. Broker credentials need no additions beyond api.rb.
  draw :console_auth_pages
  draw :console_principals
  draw :console_roles
  draw :console_secrets
  draw :console_oauth_apps
  draw :console_admin
  draw :console_automation
  draw :console_skills

  # Existing /api/v1 resources (slices add theirs in their own files above).
  draw :api

  # HTML navigations to SPA paths serve the built single-page app.
  draw :spa

  # Render a JSON 404 for any unmatched route instead of the static error page.
  match "*path", to: "errors#not_found", via: :all
end
