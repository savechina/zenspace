require "thor"
module Zen
  module Commands
    class BaseCommand < Thor
      class << self
        attr_accessor :command_name, :command_usage, :command_description
      end
    end
  end
end
