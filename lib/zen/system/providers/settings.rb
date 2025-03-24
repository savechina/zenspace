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
  #
  # @example
  #   ENV["XXX_ENV"]
  #   Settings["database"]
  #
  # @type Zen::Application
  Application.register_provider(:settings, from: :dry_system) do
    # prepare hook
    prepare do
      # require "zen/config"
    end

    ##
    # setting intial require dependency
    #
    require "dotenv/load"

    # zen user config file
    local_zen_config = ".zen"

    Dotenv.load local_zen_config if File.exist?(local_zen_config) && File.file?(local_zen_config)

    # user root config .bluekit file
    user_config_bucket = File.join(Zen::USER_ROOT, "config", ".zen")

    Dotenv.load user_config_bucket if File.exist?(user_config_bucket)

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
        setting :url, default: ENV.fetch("database.url", "sqlite::memory:")
      end

      setting :logger_level, default: :info

      setting :options do
        setting :force, default: false
        setting :verbose, default: false
      end
      # new_variable = ENV.fetch("APP_ENV", :development)
      # puts "#{new_variable}....ENV"
    end
  end
end
