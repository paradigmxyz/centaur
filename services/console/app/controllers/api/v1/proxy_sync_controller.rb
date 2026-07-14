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
    # whole transforms the proxy splices into its pipeline: one gcp_auth,
    # gcp_id_token, hmac_sign, or aws_auth transform per granted secret, and one
    # bundled oauth_token transform. `postgres` carries one upstream-DSN
    # entry per granted PgDsnSecret, keyed by foreign_id; the proxy's
    # locally-defined listeners bind to these by foreign_id.
    #
    # `rules` is the egress allowlist -- the destinations the sandbox may reach
    # at all, as opposed to the per-secret `rules` that scope where a credential
    # may be injected. iron-proxy compiles it into its `allowlist` transform,
    # which runs first in the pipeline, so an unmatched host is rejected before
    # any secret is swapped in. Sent only when the operator has turned the
    # allowlist on; omitted otherwise, and omitted is not the same as empty --
    # an empty `rules` array is an allowlist that matches nothing and 403s
    # everything. See Principal#egress_rules.
    #
    # The top-level `mcp` and `ingest_token` fields the proxy also understands
    # remain omitted: centaur-console has no models for them yet.
    class ProxySyncController < Api::ProxyBaseController
      def create
        snapshot = current_proxy.sync_config_snapshot
        current_hash = snapshot[:config_hash]

        if params[:config_hash].presence == current_hash
          render json: { config_hash: current_hash }
        else
          # The config is assembled from the proxy's principal (empty when
          # unassigned). status and principal_id let an unassigned proxy tell "no
          # config yet" apart from "config is genuinely empty", and detect a swap.
          config = snapshot[:config]
          payload = {
            config_hash: current_hash,
            status: current_proxy.status,
            principal_id: current_proxy.principal&.oid,
            secrets: config["secrets"],
            transforms: config["transforms"],
            postgres: config["postgres"]
          }
          # Present only when the allowlist is on. A nil `rules` and an empty one
          # mean opposite things to the proxy, so the key must be absent, not null.
          payload[:rules] = config["rules"] if config["rules"]
          payload[:allowlist_warn] = true if config["allowlist_warn"]

          render json: payload
        end
      end
    end
  end
end
