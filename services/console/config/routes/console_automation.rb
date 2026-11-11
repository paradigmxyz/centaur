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
