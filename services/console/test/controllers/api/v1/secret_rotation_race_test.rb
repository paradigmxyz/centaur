require "test_helper"

module Api
  module V1
    # A metadata-only PATCH (an inline source without `secret`) retains the
    # stored value. That value must be read under the document's row lock, so
    # a rotation that commits while the PATCH waits for the lock is kept and
    # not overwritten by the value the PATCH would have read earlier.
    #
    # The rotation is simulated at the exact point the PATCH asks for the row
    # lock: a hook on the record's lock! commits the rotation first, as a
    # concurrent writer holding the lock would before releasing it.
    class SecretRotationRaceTest < ActionDispatch::IntegrationTest
      module RotateBeforeLock
        attr_accessor :rotate_before_lock

        def lock!(...)
          hook = rotate_before_lock
          self.rotate_before_lock = nil
          hook&.call
          super
        end
      end

      setup do
        sign_in_console(users(:acme_admin))
      end

      KINDS = {
        static: {
          model: StaticSecret, association: :source, path: :api_v1_static_secret_path,
          stored: "stored-value", rotated: "rotated-value",
          build: lambda { |secret|
            StaticSecret.create!(name: "race-static", inject_config: { "header" => "X-Key" },
                                 source: SecretSource.new(source_type: "control_plane", secret: secret))
          },
          patch: lambda { |_record|
            { name: "race-static-renamed", inject_config: { header: "X-Key" },
              source: { source_type: "control_plane", config: {} } }
          },
          renamed: ->(record) { record.name == "race-static-renamed" }
        },
        pg_dsn: {
          model: PgDsnSecret, association: :dsn_source, path: :api_v1_pg_dsn_secret_path,
          stored: "postgres://u:stored@db.example:5432/shop", rotated: "postgres://u:rotated@db.example:5432/shop",
          build: lambda { |secret|
            PgDsnSecret.create!(created_by: users(:acme_admin), foreign_id: "race-pg", database: "shop",
                                dsn_source: SecretSource.new(source_type: "control_plane", secret: secret))
          },
          patch: lambda { |record|
            { foreign_id: record.foreign_id, database: "shop", role: "reader", settings: [],
              dsn: { source_type: "control_plane", config: {} } }
          },
          renamed: ->(record) { record.role == "reader" }
        },
        gcp_auth: {
          model: GcpAuthSecret, association: :keyfile_source, path: :api_v1_gcp_auth_secret_path,
          stored: "{\"type\":\"service_account\",\"v\":\"stored\"}", rotated: "{\"type\":\"service_account\",\"v\":\"rotated\"}",
          build: lambda { |secret|
            GcpAuthSecret.create!(created_by: users(:acme_admin), name: "race-gcp-auth", scopes: [ "https://www.googleapis.com/auth/cloud-platform" ],
                                  keyfile_source: SecretSource.new(source_type: "control_plane", secret: secret))
          },
          patch: lambda { |_record|
            { name: "race-gcp-auth-renamed", scopes: [ "https://www.googleapis.com/auth/cloud-platform" ], rules: [],
              keyfile: { source_type: "control_plane", config: {} } }
          },
          renamed: ->(record) { record.name == "race-gcp-auth-renamed" }
        },
        gcp_id_token: {
          model: GcpIdTokenSecret, association: :keyfile_source, path: :api_v1_gcp_id_token_secret_path,
          stored: "{\"type\":\"service_account\",\"v\":\"stored\"}", rotated: "{\"type\":\"service_account\",\"v\":\"rotated\"}",
          build: lambda { |secret|
            GcpIdTokenSecret.create!(created_by: users(:acme_admin), name: "race-id-token", audience: "https://race-abc123-uc.a.run.app",
                                     keyfile_source: SecretSource.new(source_type: "control_plane", secret: secret),
                                     rules: [ RequestRule.new(host: "race-abc123-uc.a.run.app", position: 0) ])
          },
          patch: lambda { |_record|
            { name: "race-id-token-renamed", audience: "https://race-abc123-uc.a.run.app",
              keyfile: { source_type: "control_plane", config: {} },
              rules: [ { host: "race-abc123-uc.a.run.app", http_methods: [ "GET" ], paths: [] } ] }
          },
          renamed: ->(record) { record.name == "race-id-token-renamed" }
        }
      }.freeze

      KINDS.each do |kind, spec|
        test "a #{kind} metadata-only PATCH keeps a rotation that commits before its lock" do
          record = instance_exec(spec[:stored], &spec[:build])
          rotate = lambda do
            spec[:model].find(record.id).public_send(spec[:association]).update!(secret: spec[:rotated])
          end

          original_find = spec[:model].method(:find_by_oid!)
          finder = lambda do |oid|
            original_find.call(oid).tap do |found|
              found.singleton_class.prepend(RotateBeforeLock)
              found.rotate_before_lock = rotate
            end
          end

          spec[:model].stub(:find_by_oid!, finder) do
            patch public_send(spec[:path], record.oid), params: { data: spec[:patch].call(record) },
                                                        headers: csrf_headers, as: :json
          end

          assert_response :ok
          assert_no_match spec[:stored], response.body
          assert_no_match spec[:rotated], response.body
          record.reload
          assert spec[:renamed].call(record), "the metadata edit should apply"
          assert_equal spec[:rotated], record.public_send(spec[:association]).secret
        end
      end
    end
  end
end
