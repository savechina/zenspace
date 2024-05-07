# typed: true
# frozen_string_literal: true

require "config"

module Zen
  #
  # Configuration
  module Configuration
    @options = {}

    String DEFALUT_CONFIG_ROOT = "config"

    DEFALUT_DATA_ROOT = "data"

    DEFAULT_ENV = "production"

    user_root = Zen::USER_ROOT

    DEFALUT_USER_CONFIG_ROOT = File.join(user_root, DEFALUT_CONFIG_ROOT)

    user_data_root = Zen::USER_ROOT_DATA

    def self.configurate(config_root = nil, env = nil)
      Config.setup do |config|
        config.use_env = true
      end

      # configuration file root path ,if is nil will set default root
      config_root = DEFALUT_CONFIG_ROOT if config_root.nil?

      # default env
      env = DEFAULT_ENV if env.nil?

      # 设置配置文件路径
      config_files = Config.setting_files(File.expand_path(config_root), env)

      # 设置用户目录下配置文件路径
      user_config_files = Config.setting_files(
        File.expand_path(DEFALUT_USER_CONFIG_ROOT), env
      )

      config_source = Config::Sources::HashSource.new(@options)

      # 设置配置文件
      config = Config.load_and_set_settings(config_files, user_config_files,
                                            config_source)
      # puts "#{config}"
    end
  end
end
