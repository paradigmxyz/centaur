# Credentials slice: broker credentials (HTML console routes).

get "console/credentials", to: "console#credentials", as: :console_credentials
# Create/edit form for broker credentials. Declared before the show route so
# /console/credentials/new wins over the generic `:id` match.
namespace :console do
  resources :broker_credentials, only: %i[new create edit update destroy], path: "credentials"
end
get "console/credentials/:id", to: "console#credential", as: :console_credential
