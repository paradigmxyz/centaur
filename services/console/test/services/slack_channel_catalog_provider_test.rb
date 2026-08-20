require "test_helper"

class SlackChannelCatalogProviderTest < ActiveJob::TestCase
  TOKEN = "xoxb-test-token"
  API_URL = "https://slack.test/api"

  test "cache miss returns immediately and enqueues one refresh without exposing the token" do
    cache = ActiveSupport::Cache::MemoryStore.new
    key = SlackChannelCatalogProvider.cache_key(token: TOKEN, api_url: API_URL)

    with_catalog_env do
      Rails.stub(:cache, cache) do
        SlackChannelCatalog.stub(:new, ->(**) { flunk("request path must not construct the Slack client") }) do
          assert_enqueued_jobs 1, only: SlackChannelCatalogRefreshJob do
            first = SlackChannelCatalogProvider.fetch
            second = SlackChannelCatalogProvider.fetch

            assert_empty first.channels
            assert_match(/loading/, first.error)
            assert_equal first, second
          end
          assert_enqueued_with(job: SlackChannelCatalogRefreshJob, args: [ key ])
        end
      end
    end

    refute_includes key, TOKEN
  end

  test "refresh caches a successful catalog for request-time reads" do
    cache = ActiveSupport::Cache::MemoryStore.new
    key = SlackChannelCatalogProvider.cache_key(token: TOKEN, api_url: API_URL)
    result = catalog_result
    catalog = Minitest::Mock.new
    catalog.expect(:fetch, result)

    with_catalog_env do
      Rails.stub(:cache, cache) do
        SlackChannelCatalog.stub(:new, ->(token:, api_url:) do
          assert_equal TOKEN, token
          assert_equal API_URL, api_url
          catalog
        end) do
          SlackChannelCatalogProvider.refresh(cache_key: key)

          assert_no_enqueued_jobs do
            cached = SlackChannelCatalogProvider.fetch
            assert_equal [ "C0123456789" ], cached.channels.map(&:id)
          end
        end
      end
    end

    catalog.verify
  end

  test "stale catalog is returned while a refresh is enqueued" do
    cache = ActiveSupport::Cache::MemoryStore.new
    key = SlackChannelCatalogProvider.cache_key(token: TOKEN, api_url: API_URL)
    catalog = Minitest::Mock.new
    catalog.expect(:fetch, catalog_result)

    with_catalog_env do
      Rails.stub(:cache, cache) do
        SlackChannelCatalog.stub(:new, ->(**) { catalog }) do
          SlackChannelCatalogProvider.refresh(cache_key: key)
        end

        travel SlackChannelCatalogProvider::FRESH_TTL + 1.second do
          assert_enqueued_with(job: SlackChannelCatalogRefreshJob, args: [ key ]) do
            stale = SlackChannelCatalogProvider.fetch
            assert_equal [ "C0123456789" ], stale.channels.map(&:id)
          end
        end
      end
    end

    catalog.verify
  end

  test "failed refresh does not replace a successful catalog" do
    cache = ActiveSupport::Cache::MemoryStore.new
    key = SlackChannelCatalogProvider.cache_key(token: TOKEN, api_url: API_URL)
    success_catalog = Minitest::Mock.new
    success_catalog.expect(:fetch, catalog_result)
    failed_catalog = Minitest::Mock.new
    failed_catalog.expect(
      :fetch,
      SlackChannelCatalog::Result.new(channels: [], error: "Slack API request failed.", configured: true)
    )

    with_catalog_env do
      Rails.stub(:cache, cache) do
        SlackChannelCatalog.stub(:new, ->(**) { success_catalog }) do
          SlackChannelCatalogProvider.refresh(cache_key: key)
        end
        SlackChannelCatalog.stub(:new, ->(**) { failed_catalog }) do
          SlackChannelCatalogProvider.refresh(cache_key: key)
        end

        cached = SlackChannelCatalogProvider.fetch
        assert_equal [ "C0123456789" ], cached.channels.map(&:id)
        assert_nil cached.error
      end
    end

    success_catalog.verify
    failed_catalog.verify
  end

  test "failed cold refresh is briefly cached before another attempt" do
    cache = ActiveSupport::Cache::MemoryStore.new
    key = SlackChannelCatalogProvider.cache_key(token: TOKEN, api_url: API_URL)
    failed_catalog = Minitest::Mock.new
    failed_catalog.expect(
      :fetch,
      SlackChannelCatalog::Result.new(channels: [], error: "Slack API request failed.", configured: true)
    )

    with_catalog_env do
      Rails.stub(:cache, cache) do
        SlackChannelCatalog.stub(:new, ->(**) { failed_catalog }) do
          SlackChannelCatalogProvider.refresh(cache_key: key)
        end

        assert_no_enqueued_jobs do
          cached_error = SlackChannelCatalogProvider.fetch
          assert_equal "Slack API request failed.", cached_error.error
        end

        travel SlackChannelCatalogProvider::ERROR_TTL + 1.second do
          assert_enqueued_with(job: SlackChannelCatalogRefreshJob, args: [ key ]) do
            assert_match(/loading/, SlackChannelCatalogProvider.fetch.error)
          end
        end
      end
    end

    failed_catalog.verify
  end

  test "unconfigured provider does not enqueue a refresh" do
    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => nil, "SLACK_BOT_TOKEN" => nil) do
      assert_no_enqueued_jobs do
        result = SlackChannelCatalogProvider.fetch
        refute result.configured
        assert_match(/not configured/, result.error)
      end
    end
  end

  test "search returns bounded matches and excludes assigned channels" do
    result = SlackChannelCatalog::Result.new(
      channels: [
        SlackChannelCatalog::Channel.new(id: "C000000001", name: "engineering", private: false),
        SlackChannelCatalog::Channel.new(id: "C000000002", name: "engineering-private", private: true),
        SlackChannelCatalog::Channel.new(id: "C000000003", name: "general", private: false)
      ],
      error: nil,
      configured: true
    )

    SlackChannelCatalogProvider.stub(:fetch, result) do
      matches = SlackChannelCatalogProvider.search(
        query: "ENGINEER",
        limit: 1,
        exclude_ids: [ "C000000001" ]
      )

      assert_equal [ "C000000002" ], matches.channels.map(&:id)
    end
  end

  private

  def with_catalog_env(&)
    with_env(
      "CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => TOKEN,
      "SLACK_BOT_TOKEN" => nil,
      "SLACK_API_URL" => API_URL,
      &
    )
  end

  def catalog_result
    SlackChannelCatalog::Result.new(
      channels: [ SlackChannelCatalog::Channel.new(id: "C0123456789", name: "general", private: false) ],
      error: nil,
      configured: true
    )
  end
end
