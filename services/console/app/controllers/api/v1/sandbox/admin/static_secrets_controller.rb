module Api
  module V1
    module Sandbox
      module Admin
        # Static secrets an admin creates and edits from their Slack DM. Secret
        # values never pass through the conversation: sources may only reference
        # an external store, inline values are set on the Console page in
        # console_url, and OAuth wrapper secrets stay read-only.
        class StaticSecretsController < ::Api::V1::StaticSecretsController
          include SandboxAdminAuthentication

          REFUSED_SOURCE_TYPES = %w[control_plane token_broker].freeze

          def create
            return if refuse_source!

            super
          end

          # PATCH/PUT /api/v1/sandbox/admin/static_secrets/:id
          #
          # Updates an existing secret by OID. Unlike the operator upsert, omitted
          # fields keep their current values, including the source.
          def update
            ref = StaticSecret.find_by_oid!(params[:id])
            if ref.broker_credential_id || ref.source&.source_type == "token_broker"
              return render_error(status: :forbidden, message: "OAuth credential secrets cannot be edited from a sandbox")
            end
            return if refuse_source!

            assign_and_save!(ref, data_params.reverse_merge(current_document(ref)), keep_source: !data_params.key?(:source))
            render json: { data: record_payload(ref) }
          rescue ActiveRecord::RecordInvalid => e
            render_validation_error(e.record)
          end

          private

          def refuse_source!
            raise ActionController::BadRequest, "data must be an object" unless data_params.is_a?(ActionController::Parameters)

            source = data_params[:source]
            return false unless source.is_a?(ActionController::Parameters) &&
              REFUSED_SOURCE_TYPES.include?(source[:source_type])

            render_error(
              status: :forbidden,
              message: "#{source[:source_type]} sources cannot be set from a sandbox; " \
                       "omit the source and set the value in the Console UI"
            )
            true
          end

          def current_document(ref)
            ref.slice(:name, :description, :kind, :labels, :inject_config, :replace_config).merge(
              "rules" => ref.rules.map { |rule| rule.slice(:host, :cidr, :http_methods, :paths) }
            )
          end

          def record_payload(ref)
            super.merge(console_url: URI.join(public_base_url, "/console/secrets/static/#{ref.oid}/edit").to_s)
          end

          def public_base_url
            ConsoleEnv["PUBLIC_URL"].presence || request.base_url
          end
        end
      end
    end
  end
end
