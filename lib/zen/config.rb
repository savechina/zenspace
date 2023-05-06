require "config"

module Zen
  #
  # Configuration
  class Configuration
    @options = {}

    DEFALUT_CONFIG_ROOT = "config".freeze
    DEFAULT_ENV = "production".freeze

    DEFALUT_USER_CONFIG_ROOT = "~/.config/zen".freeze

    DEFALUT_USER_DATA_ROOT = "~/.zen".freeze

    # @type Options
    def self.configurate(config_root = nil, env = nil)
      Config.setup do |config|
        config.use_env = true
      end

      # configuration file root path ,if is nil will set default root
      config_root = DEFALUT_CONFIG_ROOT if config_root.nil?

      # default env
      env = DEFAULT_ENV if env.nil?

      # 设置配置文件路径
      config_files = Config.setting_files(config_root, env)

      # 设置用户目录下配置文件路径
      user_config_files = Config.setting_files(File.expand_path(DEFALUT_USER_CONFIG_ROOT), env)

      config_source = Config::Sources::HashSource.new(@options)

      # 设置配置文件
      config = Config.load_and_set_settings(config_files, user_config_files, config_source)
    end
  end
end
