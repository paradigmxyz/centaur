module Api
  module V1
    # POST /api/v1/proxy/sync
    #
    # iron-proxy polls this endpoint to fetch its config. It sends its current
    # config_hash; when that matches the freshly computed hash we return only the
    # hash (no payload), so the proxy skips re-applying. Otherwise we return the
    # full `secrets` and `transforms` payload.
    #
    # `secrets` populates the proxy's `secrets` transform. `transforms` carries
    # whole transforms the proxy splices into its pipeline: a gcp_auth transform
    # per granted GcpAuthSecret, an hmac_sign transform per granted HmacSecret,
    # and one bundled oauth_token transform. `postgres` carries one upstream-DSN
    # entry per granted PgDsnSecret, keyed by foreign_id; the proxy's
    # locally-defined listeners bind to these by foreign_id.
    #
    # The top-level `rules`, `mcp`, and `ingest_token` fields the proxy also
    # understands are intentionally omitted: centaur-console has no models for them
    # yet. Each secret still carries its own per-secret `rules`.
    class ProxySyncController < Api::ProxyBaseController
      def create
        current_hash = current_proxy.config_hash

        if params[:config_hash].presence == current_hash
          render json: { config_hash: current_hash }
        else
          # The config is assembled from the proxy's principal (empty when
          # unassigned). status and principal_id let an unassigned proxy tell "no
          # config yet" apart from "config is genuinely empty", and detect a swap.
          config = current_proxy.sync_config
          render json: {
            config_hash: current_hash,
            status: current_proxy.status,
            principal_id: current_proxy.principal&.oid,
            secrets: config["secrets"],
            transforms: config["transforms"],
            postgres: config["postgres"]
          }
        end
      end
    end
  end
end
