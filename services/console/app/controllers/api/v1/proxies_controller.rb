module Api
  module V1
    class ProxiesController < Api::BaseController
      def index
        scope = ::Proxy.all
        scope = scope.where(principal: principal_filter) if params[:principal_id].present?
        scope = scope.order(created_at: :asc, id: :asc)

        limit = pagination_limit
        page = pagination_page
        total = scope.count
        records = scope.limit(limit).offset((page - 1) * limit)
        total_pages = total.zero? ? 0 : ((total + limit - 1) / limit)
        render json: {
          data: records.map { |p| record_payload(p) },
          meta: { page: page, limit: limit, total: total, total_pages: total_pages }
        }
      end

      def show
        proxy = ::Proxy.find_by_oid!(params[:id])
        render json: { data: record_payload(proxy) }
      end

      def create
        attrs = data_params.permit(:name, :principal_id)
        # principal_id is optional: a proxy may boot unassigned and be assigned later.
        principal = attrs[:principal_id].present? ? Principal.find_by_oid!(attrs[:principal_id]) : nil
        proxy = ::Proxy.new(name: attrs[:name], principal: principal)
        proxy.save!
        render status: :created, json: { data: record_payload(proxy).merge(token: proxy.token) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # PATCH/PUT assigns, swaps, or clears the proxy's principal on the fly. A
      # principal_id of null unassigns; an opaque id assigns or swaps; omitting
      # the key leaves the assignment unchanged. The token never changes.
      def update
        proxy = ::Proxy.find_by_oid!(params[:id])
        if data_params.key?(:principal_id)
          oid = data_params[:principal_id]
          proxy.principal = oid.present? ? Principal.find_by_oid!(oid) : nil
        end
        proxy.name = data_params[:name] if data_params.key?(:name)
        proxy.save!
        render json: { data: record_payload(proxy) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def destroy
        proxy = ::Proxy.find_by_oid!(params[:id])
        proxy.destroy!
        head :no_content
      end

      private

      def principal_filter
        Principal.find_by_oid!(params[:principal_id])
      end

      def record_payload(proxy)
        {
          id: proxy.oid,
          name: proxy.name,
          principal_id: proxy.principal&.oid,
          status: proxy.status,
          principal_assigned_at: proxy.principal_assigned_at,
          created_at: proxy.created_at,
          updated_at: proxy.updated_at
        }
      end
    end
  end
end
