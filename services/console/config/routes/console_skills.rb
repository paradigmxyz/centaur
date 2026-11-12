# Open to any active user.
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
