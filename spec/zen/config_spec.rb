# typed: false
# frozen_string_literal: true

require "zen"
require "zen/configuration"
require "config"

RSpec.describe Zen::Configuration do
  it "does zen load configuation" do
    config_root = File.join(File.dirname(__FILE__), "config")

    puts config_root

    config = Zen::Configuration.configurate(config_root, "test")

    puts "config.name #{config}"

    expect(config.has_key?(:zen)).to eq(true)

    puts Settings.key?(:zen)

    puts File.expand_path("~/.config/settings.yml")

    puts "members: #{Settings.zen.members.db}"

    # Settings.zen.members = { "db" => "hello.db", "database" => "data" }

    puts config.to_yaml

    f = File.absolute_path("../config/settings.yml", __FILE__)

    puts File.exist?(f)

    # puts config
    puts config.to_json

    # YAML.add_tag=nil
    fc = config.to_yaml({ indentation: 2, header: true }).gsub("!ruby/object:Config::Options", "")
    puts fc
  end
end
