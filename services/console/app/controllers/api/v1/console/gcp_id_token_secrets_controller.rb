module Api
  module V1
    module Console
      # The gcp_id_token secrets resource for the console SPA (session only), with the
      # console write semantics of ConsoleSecretDocumentWrites.
      class GcpIdTokenSecretsController < ::Api::V1::GcpIdTokenSecretsController
        include ConsoleApiAuthentication
        include ConsoleSecretDocumentWrites
      end
    end
  end
end
