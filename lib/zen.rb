# frozen_string_literal: true

require_relative "zen/version"
require_relative "zen/config"
require_relative "zen/cli"
require_relative "zen/repo"
require_relative "zen/member"

module Zen
  class Error < StandardError; end
  # Your code goes here...

  Zen::Configuration.configurate
end
