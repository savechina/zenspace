# typed: true
# frozen_string_literal: true

module Zen
  module Commands
    ##
    # TestCommand for Test Utils
    #
    class TestCommand < Zen::Commands::BaseCommand
      include Import["zen.components.test_kit"]

      desc "init [DIRECTORY] [OUTPUT_FILE]", "initialize test config."
      def init(_output_file = nil)
        puts "test intialize "

        test_kit.init
      end

      desc "data [TYPE]", "Generate test data etc."
      def data(_output_file = nil)
        puts "test data genreate."
      end

      # command base define
      self.command_name = "test"
      self.command_usage = "test [COMMAND]"
      self.command_description = "Test process suite.generate test data and mock test service."
    end
  end
end
