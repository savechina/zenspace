# typed: true
# frozen_string_literal: true

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
  # The user root path for Zen store user's config and data ,templates etc.
  USER_ROOT = File.join(Dir.home, ".zen")

  # require submodule lib
  require_relative "zen/version"
  require_relative "zen/contants"
  require_relative "zen/errors"

  # System
  require_relative "zen/system"
  require_relative "zen/system/container"
  require_relative "zen/system/import"

  # Components
  # require_relative "zen/config"
  # require_relative "zen/components/starter"

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
end
