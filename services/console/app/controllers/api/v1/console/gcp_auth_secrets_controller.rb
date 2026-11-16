module Api
  module V1
    module Console
      # The gcp_auth secrets resource for the console SPA (session only), with the
      # console write semantics of ConsoleSecretDocumentWrites.
      class GcpAuthSecretsController < ::Api::V1::GcpAuthSecretsController
        include ConsoleApiAuthentication
        include ConsoleSecretDocumentWrites
      end
    end
  end
end
