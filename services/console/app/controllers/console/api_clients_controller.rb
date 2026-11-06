module Console
  # Admin UI for API clients: machine callers of the api-rs session API. A
  # client's secret is shown once, in the response that issues it, and never
  # again.
  class ApiClientsController < ApplicationController
    layout "console"

    before_action :require_api_clients_enabled
    before_action :require_admin
    before_action :set_api_client, except: %i[index new create]
    before_action :set_principal_link, only: %i[detach_principal make_default_principal]

    def index
      @api_clients = ApiClient.includes(:principals).order(:id)
    end

    def show
      load_show
    end

    def new
      @api_client = ApiClient.new(scopes: ApiClient::SCOPES)
    end

    def create
      @api_client = ApiClient.new(created_by: current_user)
      @api_client.assign_attributes(api_client_attributes)
      principal_ref = params.dig(:api_client, :principal).to_s.strip
      principal = find_principal(principal_ref)
      if principal_ref.present? && principal.nil?
        @api_client.errors.add(:base, "No principal matches #{principal_ref.inspect}.")
        return render :new, status: :unprocessable_entity
      end
      saved = ApiClient.transaction do
        next false unless @api_client.save
        @api_client.attach_principal!(principal) if principal
        true
      end
      return render :new, status: :unprocessable_entity unless saved

      render_issued_secret(notice: "API client created.")
    end

    def edit; end

    def update
      if @api_client.update(api_client_attributes)
        redirect_to console_api_client_path(@api_client), notice: "API client updated."
      else
        render :edit, status: :unprocessable_entity
      end
    end

    def destroy
      @api_client.destroy!
      redirect_to console_api_clients_path, notice: "Deleted API client #{@api_client.name}."
    end

    def rotate_secret
      @api_client.rotate_secret!
      render_issued_secret(notice: "Secret rotated. The previous secret no longer works.")
    end

    def disable
      @api_client.update!(disabled_at: Time.current) unless @api_client.disabled?
      redirect_to console_api_client_path(@api_client), notice: "API client disabled."
    end

    def enable
      @api_client.update!(disabled_at: nil)
      redirect_to console_api_client_path(@api_client), notice: "API client enabled."
    end

    def attach_principal
      principal = find_principal(params[:principal].to_s.strip)
      return redirect_to console_api_client_path(@api_client), alert: "No principal matches that OID or foreign ID." unless principal

      @api_client.attach_principal!(principal)
      redirect_to console_api_client_path(@api_client), notice: "Attached #{principal_label(principal)}."
    rescue ActiveRecord::RecordNotUnique
      # A concurrent submit attached it first; the requested end state holds.
      redirect_to console_api_client_path(@api_client), notice: "Attached #{principal_label(principal)}."
    end

    def detach_principal
      @principal_link.destroy!
      redirect_to console_api_client_path(@api_client),
                  notice: "Detached #{principal_label(@principal_link.principal)}."
    end

    def make_default_principal
      @api_client.make_default!(@principal_link)
      redirect_to console_api_client_path(@api_client),
                  notice: "#{principal_label(@principal_link.principal)} is now the default principal."
    end

    private

    # Answers as for an unknown route, so a disabled feature is not advertised.
    def require_api_clients_enabled
      render status: :not_found, json: { error: { message: "not found" } } unless ApiClientsFeature.enabled?
    end

    def set_api_client
      @api_client = ApiClient.find_by_oid!(params[:id])
    end

    def set_principal_link
      @principal_link = @api_client.api_client_principals.find_by_oid!(params[:link_id])
    end

    def api_client_attributes
      fields = params.fetch(:api_client, ActionController::Parameters.new)
                     .permit(:name, scopes: [])
      fields[:scopes] = Array(fields[:scopes]).compact_blank if fields.key?(:scopes)
      fields
    end

    # Renders the secret in this response only: it is not stored, flashed, or
    # cached, so reloading the page cannot show it again.
    def render_issued_secret(notice:)
      @secret = @api_client.secret
      flash.now[:notice] = notice
      response.headers["Cache-Control"] = "no-store"
      load_show
      render :show, status: :ok
    end

    def load_show
      @principal_links = @api_client.api_client_principals.includes(:principal).order(:id)
    end

    def find_principal(ref)
      return nil if ref.blank?
      Principal.find_by_oid(ref) || Principal.resolve_foreign_id(ref)
    end

    def principal_label(principal)
      principal.name.presence || principal.foreign_id.presence || principal.oid
    end
  end
end
