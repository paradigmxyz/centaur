module Api
  module V1
    module Console
      # The pg_dsn secrets resource for the console SPA (session only), with the
      # console write semantics of ConsoleSecretDocumentWrites.
      class PgDsnSecretsController < ::Api::V1::PgDsnSecretsController
        include ConsoleApiAuthentication
        include ConsoleSecretDocumentWrites
      end
    end
  end
end
