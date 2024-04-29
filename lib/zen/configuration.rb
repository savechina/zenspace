# frozen_string_literal: true
# typed: true

require "config"

module Zen
  #
  # Configuration
  module Configuration
    @options = {}

    String DEFALUT_CONFIG_ROOT = "config"

    DEFALUT_DATA_ROOT = "data"

    DEFAULT_ENV = "production"

    DEFALUT_USER_ROOT = Zen::USER_ROOT

    DEFALUT_USER_CONFIG_ROOT = File.join(DEFALUT_USER_ROOT, DEFALUT_CONFIG_ROOT)

    DEFALUT_USER_DATA_ROOT = File.join(DEFALUT_USER_ROOT, DEFALUT_DATA_ROOT)

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
