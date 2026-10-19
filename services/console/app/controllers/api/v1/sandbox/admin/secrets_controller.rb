module Api
  module V1
    module Sandbox
      module Admin
        # Grantable secrets of every kind, so an admin can pick one to grant.
        # Only identifying metadata is returned, never sources or values.
        class SecretsController < ::Api::BaseController
          include SandboxAdminAuthentication

          def index
            query = params[:q].to_s.strip
            secrets = ::Api::V1::GrantsController::GRANTABLE_TYPES.flat_map do |grant_key, model|
              scope = model.order(:id)
              if query.present?
                pattern = "%#{model.sanitize_sql_like(query)}%"
                scope = scope.where("name ILIKE :pattern OR foreign_id ILIKE :pattern", pattern: pattern)
              end
              scope.map { |secret| secret_payload(secret, grant_key) }
            end
            render json: { data: secrets }
          end

          private

          def secret_payload(secret, grant_key)
            {
              id: secret.oid,
              grant_key: grant_key,
              foreign_id: secret.foreign_id,
              name: secret.name,
              description: secret.description,
              labels: secret.labels
            }
          end
        end
      end
    end
  end
end
