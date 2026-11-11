# Automation slice: workflows and scheduled tasks (HTML console routes).

namespace :console do
  resources :workflows, only: %i[index show] do
    member do
      post :run, action: :force_start
    end
  end
  resources :scheduled_tasks, except: :show do
    post :run, on: :member
    get :slack_channel_options,
        on: :collection,
        to: "slack_channel_options#index",
        defaults: { owner_type: "scheduled_task" }
  end
end

# /api/v1 endpoints behind the automation pages (see docs/API.md#workflows and
# docs/API.md#scheduled-tasks). GET /api/v1/scheduled_tasks/:id is drawn in
# api.rb.
namespace :api do
  namespace :v1 do
    resources :workflows, only: %i[index show], constraints: { id: %r{[^/]+} }, format: false do
      member do
        get :runs
        post :runs, action: :create_run
      end
    end
    resources :scheduled_tasks, only: %i[index new create update destroy] do
      post :run, on: :member
      get :slack_channel_options, on: :collection
    end
  end
end
