module Api
  module V1
    module Console
      # API clients: machine callers of the api-rs session API, such as eval
      # runners. Admin-only, and answered as an unknown route (404, before
      # authentication) while CENTAUR_CONSOLE_API_CLIENTS_ENABLED is off so a
      # disabled feature is not advertised.
      #
      # The client secret is returned only in the create and rotate_secret
      # responses (with Cache-Control: no-store). It is stored as a digest and
      # can never be read back.
      class ApiClientsController < BaseController
        prepend_before_action :require_api_clients_enabled
        before_action :set_api_client, except: %i[index create]
        before_action :set_principal_link, only: %i[detach_principal make_default_principal]

        def index
          clients = ApiClient.includes(api_client_principals: :principal).order(:id).to_a
          render json: { data: clients.map { |client| client_payload(client) }, meta: { total: clients.size } }
        end

        def show
          render_client
        end

        def create
          @api_client = ApiClient.new(created_by: current_user)
          @api_client.assign_attributes(client_attributes)
          principal_ref = data_params[:principal].to_s.strip
          principal = find_principal(principal_ref)
          if principal_ref.present? && principal.nil?
            return render_error(status: :unprocessable_entity, message: "validation failed",
                                details: { base: [ "No principal matches #{principal_ref.inspect}." ] })
          end

          saved = ApiClient.transaction do
            next false unless @api_client.save

            @api_client.attach_principal!(principal) if principal
            true
          end
          return render_validation_error(@api_client) unless saved

          render_issued_secret(status: :created)
        end

        def update
          return render_validation_error(@api_client) unless @api_client.update(client_attributes)

          render_client
        end

        def destroy
          @api_client.destroy!
          head :no_content
        end

        def rotate_secret
          @api_client.rotate_secret!
          render_issued_secret(status: :ok)
        end

        def disable
          @api_client.update!(disabled_at: Time.current) unless @api_client.disabled?
          render_client
        end

        def enable
          @api_client.update!(disabled_at: nil)
          render_client
        end

        def attach_principal
          principal = find_principal(principal_ref_param)
          unless principal
            message = "No principal matches that OID or foreign ID."
            return render_error(status: :unprocessable_entity, message: message, details: { base: [ message ] })
          end

          begin
            link = @api_client.attach_principal!(principal)
          rescue ActiveRecord::RecordNotUnique
            # A concurrent request attached it first; the requested end state holds.
            link = @api_client.api_client_principals.find_by!(principal: principal)
          end
          render status: :created, json: { data: link_payload(link) }
        end

        def detach_principal
          @principal_link.destroy!
          head :no_content
        end

        def make_default_principal
          @api_client.make_default!(@principal_link)
          render json: { data: link_payload(@principal_link.reload) }
        end

        private

        def require_api_clients_enabled
          render status: :not_found, json: { error: { message: "not found" } } unless ApiClientsFeature.enabled?
        end

        def set_api_client
          @api_client = ApiClient.find_by_oid!(params[:id])
        end

        def set_principal_link
          @principal_link = @api_client.api_client_principals.includes(:principal).find_by_oid!(params[:link_id])
        end

        def client_attributes
          fields = data_params.permit(:name, scopes: [])
          fields[:scopes] = Array(fields[:scopes]).compact_blank if fields.key?(:scopes)
          fields
        end

        # The principal to attach: `data.principal` (or a top-level `principal`).
        def principal_ref_param
          (params.dig(:data, :principal) || params[:principal]).to_s.strip
        end

        def find_principal(ref)
          return nil if ref.blank?

          Principal.find_by_oid(ref) || Principal.find_by(foreign_id: ref)
        end

        def render_client
          render json: { data: client_payload(reloaded_client) }
        end

        # The plaintext secret appears in this response only; it is never stored,
        # logged, or cached.
        def render_issued_secret(status:)
          response.headers["Cache-Control"] = "no-store"
          secret = @api_client.secret
          render status: status, json: { data: client_payload(reloaded_client).merge(secret: secret) }
        end

        # The client as stored, with its principal links preloaded for the payload.
        def reloaded_client
          ApiClient.includes(api_client_principals: :principal).find(@api_client.id)
        end

        def client_payload(client)
          links = client.api_client_principals.sort_by(&:id)
          {
            id: client.oid,
            name: client.name,
            scopes: client.scopes,
            disabled: client.disabled?,
            disabled_at: client.disabled_at,
            principals_count: links.size,
            principals: links.map { |link| link_payload(link) },
            created_at: client.created_at,
            updated_at: client.updated_at
          }
        end

        def link_payload(link)
          principal = link.principal
          {
            id: link.oid,
            default: link.default,
            principal: {
              id: principal.oid,
              foreign_id: principal.foreign_id,
              name: principal.name,
              kind: principal.kind
            }
          }
        end
      end
    end
  end
end
