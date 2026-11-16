module Api
  module V1
    module Console
      # The static secrets resource for the console SPA (session only), with the
      # console write semantics of ConsoleSecretDocumentWrites.
      class StaticSecretsController < ::Api::V1::StaticSecretsController
        include ConsoleApiAuthentication
        include ConsoleSecretDocumentWrites
      end
    end
  end
end
