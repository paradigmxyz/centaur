# Skills slice (HTML console routes).

namespace :console do
  resources :skills do
    collection do
      get :mine
    end
    member do
      post :share
      post :unshare
    end
  end
end

# /api/v1 endpoints behind the Skills and personal SSH key pages (see
# docs/API.md#console-skills and docs/API.md#personal-ssh-key). Open to any
# active user.
namespace :api do
  namespace :v1 do
    resources :skills, only: %i[index show create update destroy] do
      collection do
        get :editor_candidates
      end
      member do
        post :share
        post :unshare
      end
    end

    scope "me", as: "me" do
      resource :ssh_key, only: %i[show update destroy], controller: "personal_ssh_keys" do
        post :validate
      end
    end
  end
end
