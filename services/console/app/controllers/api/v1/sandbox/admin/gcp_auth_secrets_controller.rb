module Api
  module V1
    module Sandbox
      module Admin
        class GcpAuthSecretsController < ::Api::V1::GcpAuthSecretsController
          include SandboxAdminSecretWrites

          private

          def secret_model = GcpAuthSecret
          def source_key = :keyfile
          def source_association = :keyfile_source

          def current_document(ref)
            ref.slice(:name, :description, :subject, :labels, :credentials_provider, :scopes).merge(
              "rules" => ref.rules.map { |rule| rule.slice(:host, :cidr, :http_methods, :paths) }
            )
          end
        end
      end
    end
  end
end
