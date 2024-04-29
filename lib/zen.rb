# frozen_string_literal: true
# typed: true

##
# Zen is a productivity suite designed to help you conquer common workplace challenges with ease.
# It provides a set of integrated tools for managing tasks, data process, repository and development ,
# streamlining your workflow and boosting your efficiency.
#
module Zen
  # Common config define

  ##
  # The root path for Zen source libraries
  ROOT = File.dirname(File.expand_path(__dir__))

  ##
  # The template root path for Zen templates
  TEMPLATE_ROOT = File.join(ROOT, "templates")

  ##
  # The user root path for Zen store user's config and data ,templates etc.
  USER_ROOT = File.join(Dir.home, ".zen")

  # @deprecated Use {Config::CONFIG_DIR}
  # CONFIG_DIR = Config::CONFIG_DIR

  # require submodule lib
  require_relative "zen/version"
  require_relative "zen/errors"

  # System
  require_relative "zen/system"
  require_relative "zen/system/container"
  require_relative "zen/system/import"

  # Components
  # require_relative "zen/config"
  require_relative "zen/components/starter"

  # Commands
  require_relative "zen/command"
  require_relative "zen/cli"

  # Application.finalize!

  # puts Application.keys

  # puts "Container keys: #{Application.keys}"
  # puts "User repo:      #{service.user_repo.inspect}"
  # puts "Provider  keys: #{Application.providers.providers}"
  # puts "Settings  keys: #{Application.settings}"
  # puts "Loader:         #{Application.autoloader}"
  # pp "Configuration:  #{Application["settings"]}"

  # greeter = Application["zen.components.greeter"]
  # puts greeter.call("world zenspace")

  # include Zen::Import["logger"]

  # logger = Application.logger

  # logger.debug("logger first application")

  # # puts "_dir_ : #{__dir__}"

  # # puts "_file_ : #{__FILE__}"

  # puts "user:#{Dir.home}"

  # Using Dir.pwd and File.dirname
  # current_dir = __dir__
  # project_root = File.dirname(current_dir)
  # puts "project_root : #{project_root}"

  # puts "ROOT:#{ROOT}"
  # puts "ROOT: #{File.expand_path(ROOT)}"
  # puts "TEMPLATE_ROOT: #{File.expand_path(TEMPLATE_ROOT)}"

  # # Application
  # pp "Configuration:  #{Application["settings"].to_h}"

  # pp "#{Settings}"
end
