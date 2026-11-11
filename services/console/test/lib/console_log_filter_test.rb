require "test_helper"

class ConsoleLogFilterTest < ActiveSupport::TestCase
  setup { @filter = ActiveSupport::ParameterFilter.new(Rails.application.config.filter_parameters) }

  def filtered?(name) = @filter.filter(name => "v")[name] == "[FILTERED]"

  test "filters encoded, double-encoded, and nested names" do
    %w[code CODE c%6fde %63%6F%64%65 c%256fde data[code] data%5Bcode%5D data%5Bc%6Fde%5D
       code_verifier t%6Fken p%61ssword a.code].each do |name|
      assert filtered?(name), name
    end
  end

  test "leaves unrelated names alone" do
    %w[country_code state c%6fdex scope redirect_uri].each { |name| assert_not filtered?(name), name }
  end

  test "malformed encodings do not raise" do
    assert_nothing_raised { @filter.filter("%zz%" => "v", "c%6" => "v") }
  end
end
