# frozen_string_literal: true
# typed: true

require_relative "container"

module Zen
  ##
  # Zen::Application Import
  # @return Dry::AutoInject::Builder
  #
  Import = Application.injector
end
