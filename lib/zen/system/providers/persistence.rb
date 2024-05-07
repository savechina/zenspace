# typed: true
# frozen_string_literal: true

# require "dry/system/provider_sources"

##
# Zen System persistence provider
module Zen
  ##
  # Persistence provider
  # @return [Sequel::DB]
  # @see Sequel
  Application.register_provider(:persistence) do
    prepare do
      require "sequel"
    end

    start do
      url = "sqlite::memory"
      register("persistence.db", Sequel.connect(url))
    end

    stop do
      container["persistence.db"].close_connection
    end
  end
end
