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
          render_error(status: :conflict, message: "Crew data changed; read it again before editing")
        end

        rescue_from ActionController::BadRequest do |error|
          render_error(status: :bad_request, message: error.message)
        end

        def show
          response.headers["Cache-Control"] = "no-store"
          render json: { data: payload(client.self_get(@app_id)) }
        end

        def update
          body = request.request_parameters
          fields = body["data"]
          unless body.keys == [ "data" ] && fields.is_a?(Hash) && fields.any? &&
              (fields.keys - %w[name description icon_url system_prompt skills default_models lock_version]).empty? && request.query_parameters.empty?
            return render_error(status: :bad_request, message: "Only your own identity, prompt, skills and model defaults may be changed; roles are admin-only")
          end

          record = client.self_get(@app_id) # Fail closed for paused or inactive bots.
          configuration = fields.slice("system_prompt", "skills", "default_models")
          @profile.with_lock do
            if configuration.any?
              unless fields["lock_version"].is_a?(Integer) && fields["lock_version"] == @profile.lock_version
                raise ActiveRecord::StaleObjectError.new(@profile, "update")
              end
              @profile.revision_source = "self"
              @profile.update!(configuration)
            end
            identity = fields.slice("name", "description", "icon_url")
            record = client.self_update(@app_id, identity) if identity.any?
          end
          response.headers["Cache-Control"] = "no-store"
          render json: { data: payload(record) }
        end

        def memories
          ensure_active!
          unless (request.query_parameters.keys - %w[q include_expired]).empty?
            raise ActionController::BadRequest, "Only memory search and expiry filters are supported"
          end
          rows = @profile.memories.where(scope_key: [ "shared", @identity[:conversation_id] ])
          rows = rows.unexpired unless params[:include_expired] == "true"
          if params[:q].present?
            query = "%#{CrewMemory.sanitize_sql_like(params[:q].to_s)}%"
            rows = rows.where("key ILIKE ? OR content ILIKE ?", query, query)
          end
          render json: { data: { memories: rows.order(updated_at: :desc, id: :desc).map(&:payload) } }
        end

        def remember
          fields = write_fields(%w[key scope content source expires_at lock_version])
          ensure_active!
          memory = @profile.remember!(scope_key: memory_scope(fields), key: fields["key"],
            attributes: fields.except("lock_version").reverse_merge("source" => @identity[:thread_key]),
            expected_version: fields["lock_version"])
          render json: { data: memory.payload }
        end

        def forget
          fields = write_fields(%w[key scope lock_version])
          ensure_active!
          @profile.forget!(scope_key: memory_scope(fields), key: fields["key"], expected_version: fields["lock_version"])
          render json: { data: { forgotten: true } }
        end

        def history
          ensure_active!
          unless (request.query_parameters.keys - %w[version]).empty?
            raise ActionController::BadRequest, "Only a revision version may be selected"
          end
          data = { lock_version: @profile.lock_version,
            revisions: @profile.revisions.order(version: :desc).as_json(only: %i[version source created_at]) }
          data[:configuration] = @profile.revisions.find_by!(version: params[:version]).configuration if params[:version].present?
          render json: { data: data }
        end

        def restore
          fields = write_fields(%w[version lock_version])
          unless fields["version"].is_a?(Integer) && fields["version"] >= 0
            raise ActionController::BadRequest, "A non-negative version is required"
          end
          record = ensure_active!
          @profile.revision_source = "self"
          @profile.restore!(fields.fetch("version"), expected_version: fields["lock_version"])
          render json: { data: payload(record) }
        end

        private

        def require_crew_session!
          @identity = CrewSession.identity_for(current_proxy)
          @app_id = @identity&.fetch(:app_id)
          @profile = CrewProfile.find_by(app_id: @app_id, principal: current_proxy.principal) if @app_id
          response.headers["Cache-Control"] = "no-store"
          render_error(status: :forbidden, message: "This sandbox is not assigned to a configured Crew bot") unless @profile
        end

        def ensure_active!
          client.self_get(@app_id)
        end

        def write_fields(allowed)
          body = request.request_parameters
          fields = body["data"]
          unless body.keys == [ "data" ] && fields.is_a?(Hash) && fields.any? &&
              (fields.keys - allowed).empty? && request.query_parameters.empty?
            raise ActionController::BadRequest, "Unsupported Crew fields"
          end
          fields
        end

        def memory_scope(fields)
          unless fields["key"].is_a?(String) &&
              %w[content source].all? { |key| !fields.key?(key) || fields[key].is_a?(String) }
            raise ActionController::BadRequest, "Memory key, content and source must be text"
          end
          case fields.fetch("scope", "conversation")
          when "conversation" then @identity[:conversation_id]
          when "shared" then "shared"
          else raise ActionController::BadRequest, "Memory scope must be conversation or shared"
          end
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
