module Api
  module V1
    module Sandbox
      class CrewController < Api::SandboxBaseController
        wrap_parameters false
        before_action :require_crew_session!
        class_attribute :client_factory, default: -> { SlackCrewClient.new }

        rescue_from SlackCrewClient::Error do |error|
          status = [ 400, 404, 409 ].include?(error.status) ? error.status : :bad_gateway
          render_error(status: status, message: error.message)
        end

        rescue_from ActiveRecord::RecordInvalid do |error|
          render_error(status: :unprocessable_entity, message: error.record.errors.full_messages.to_sentence)
        end

        rescue_from ActiveRecord::StaleObjectError do
          render_error(status: :conflict, message: "Crew settings changed; read your profile again before editing")
        end

        def show
          response.headers["Cache-Control"] = "no-store"
          render json: { data: payload(client.self_get(@app_id)) }
        end

        def update
          body = request.request_parameters
          fields = body["data"]
          unless body.keys == [ "data" ] && fields.is_a?(Hash) && fields.any? &&
              (fields.keys - %w[name description system_prompt skills default_models lock_version]).empty? && request.query_parameters.empty?
            return render_error(status: :bad_request, message: "Only your own identity, prompt, skills and model defaults may be changed; roles are admin-only")
          end

          record = client.self_get(@app_id) # Fail closed for paused or inactive bots.
          configuration = fields.slice("system_prompt", "skills", "default_models")
          @profile.with_lock do
            if configuration.any?
              unless fields["lock_version"].is_a?(Integer) && fields["lock_version"] == @profile.lock_version
                raise ActiveRecord::StaleObjectError.new(@profile, "update")
              end
              @profile.update!(configuration)
            end
            identity = fields.slice("name", "description")
            record = client.self_update(@app_id, identity) if identity.any?
          end
          response.headers["Cache-Control"] = "no-store"
          render json: { data: payload(record) }
        end

        private

        def require_crew_session!
          @app_id = CrewSession.app_id_for(current_proxy)
          @profile = CrewProfile.find_by(app_id: @app_id, principal: current_proxy.principal) if @app_id
          render_error(status: :forbidden, message: "This sandbox is not assigned to a configured Crew bot") unless @profile
        end

        def payload(record)
          record.merge(@profile.runtime_configuration).merge(
            lock_version: @profile.lock_version,
            roles: @profile.principal.roles.order(:name).map { |role| { id: role.oid, name: role.name } }
          )
        end

        def client
          @client ||= self.class.client_factory.call
        end
      end
    end
  end
end
