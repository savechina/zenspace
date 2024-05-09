# typed: true
# frozen_string_literal: true

require_relative "container"

module Zen
  ##
  # Import is injector for Zen::Application container
  #
  # An injector is a useful mixin which injects dependencies into
  # automatically defined constructor.
  #
  # @example
  #   # Define an injection mixin
  #   #
  #   # system/import.rb
  #   Import = Application.injector
  #
  #   # Use it in your auto-registered classes
  #   #
  #   # lib/user_repo.rb
  #   require 'import'
  #
  #   class UserRepo
  #     include Import['persistence.db']
  #   end
  #
  #   MyApp['user_repo].db # instance under 'persistence.db' key
  # Dry::AutoInject::Builder
  # @return Dry::AutoInject
  #
  Import = Application.injector
end
