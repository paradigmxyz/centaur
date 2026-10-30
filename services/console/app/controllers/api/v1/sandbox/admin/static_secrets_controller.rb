module Api
  module V1
    module Sandbox
      module Admin
        class StaticSecretsController < ::Api::V1::StaticSecretsController
          include SandboxAdminSecretWrites

          private

          def secret_model = StaticSecret
          def source_key = :source
          def source_association = :source

          def current_document(ref)
            ref.slice(:name, :description, :kind, :labels, :inject_config, :replace_config).merge(
              "rules" => ref.rules.map { |rule| rule.slice(:host, :cidr, :http_methods, :paths) }
            )
          end
        end
      end
    end
  end
end
