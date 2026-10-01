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

        def show
          response.headers["Cache-Control"] = "no-store"
          render json: { data: client.self_get(@app_id) }
        end

        def update
          body = request.request_parameters
          fields = body["data"]
          unless body.keys == [ "data" ] && fields.is_a?(Hash) && fields.any? &&
              (fields.keys - %w[name description]).empty? && request.query_parameters.empty?
            return render_error(status: :bad_request, message: "Only name and description may be changed")
          end

          response.headers["Cache-Control"] = "no-store"
          render json: { data: client.self_update(@app_id, fields) }
        end

        private

        def require_crew_session!
          @app_id = CrewSession.app_id_for(current_proxy)
          render_error(status: :forbidden, message: "This sandbox is not assigned to a Crew session") unless @app_id
        end

        def client
          @client ||= self.class.client_factory.call
        end
      end
    end
  end
end
