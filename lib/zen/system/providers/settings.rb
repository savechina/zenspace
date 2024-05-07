# typed: true
# frozen_string_literal: true

require "dry/system/provider_sources"
require "config"

##
# Zen System Setting provider
#
module Zen
  ##
  # Settings for Application common config
  #
  # @example
  #   ENV["XXX_ENV"]
  #   Settings["database"]
  #
  #
  Application.register_provider(:settings, from: :dry_system) do
    # prepare hook
    prepare do
      # require "zen/config"
    end

    ##
    # setting intial require dependency
    #
    require "dotenv/load"

    # puts "root: #{target.root}, env: #{target.config.env}"
    # puts "#{target.settings["env"]}"

    settings do
      # puts "setting prepare.... config init"

      # Config.setup do |config|
      #   config.use_env = true
      # end

      config_root = File.join(ROOT, "config")
      # config_files = Config.setting_files(File.expand_path(config_root), "production")

      # puts "setting config load :#{config_files}"

      Zen::Configuration.configurate(config_root)

      # Pass a block for nested configuration (works to any depth)
      setting :database do
        # Can pass a default value
        setting :url, default: "sqlite:memory"
      end

      new_variable = ENV.fetch("APP_ENV", :development)
      puts "#{new_variable}....ENV"
    end

    # #open happen error
    # start do
    # end

    # stop do
    # end
  end
end
