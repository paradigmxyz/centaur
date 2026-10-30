module Api
  module V1
    module Sandbox
      module Admin
        class PgDsnSecretsController < ::Api::V1::PgDsnSecretsController
          include SandboxAdminSecretWrites

          private

          def secret_model = PgDsnSecret
          def source_key = :dsn
          def source_association = :dsn_source

          def current_document(ref)
            ref.slice(:name, :description, :database, :role, :labels, :settings)
          end
        end
      end
    end
  end
end
