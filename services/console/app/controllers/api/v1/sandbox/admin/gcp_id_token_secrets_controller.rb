module Api
  module V1
    module Sandbox
      module Admin
        class GcpIdTokenSecretsController < ::Api::V1::GcpIdTokenSecretsController
          include SandboxAdminSecretWrites

          private

          def secret_model = GcpIdTokenSecret
          def source_key = :keyfile
          def source_association = :keyfile_source

          def current_document(ref)
            ref.slice(:name, :description, :audience, :header, :labels).merge(
              "rules" => ref.rules.map { |rule| rule.slice(:host, :cidr, :http_methods, :paths) }
            )
          end
        end
      end
    end
  end
end
