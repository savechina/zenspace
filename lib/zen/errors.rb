# typed: true
# frozen_string_literal: true

module Zen
  #
  # Error ZenError
  #
  class ZenError < StandardError; end

  class ZenComponentError < ZenError; end

  class ZenCommandError < ZenError; end

  class ZenProviderError < ZenError; end
end
