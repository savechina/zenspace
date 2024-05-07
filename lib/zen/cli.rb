# typed: true
# frozen_string_literal: true

require "thor"

module Zen
  module Commands
    ##
    # Zen::Commands::CLI is the Zen CLI Entry
    class CLI < Thor
      include Import["logger"]

      map "-v" => :version

      Application.logger.debug(" Zen::ClI init ....")

      # base option
      class_option :verbose, type: :boolean

      ##
      # base command for CLI tool
      #
      desc "hello NAME", "say hello to NAME"
      options from: :required, yell: :boolean
      def hello(name)
        puts "> saying hello" if options[:verbose]
        output = []
        output << "from: #{options[:from]}" if options[:from]
        output << "Hello #{name}"
        output = output.join("\n")

        puts options[:yell] ? output.upcase : output
        puts "> done saying hello" if options[:verbose]
      end

      desc "goodbye", "say goodbye to the world"
      def goodbye
        puts "> saying goodbye" if options[:verbose]
        puts "Goodbye World"

        puts "> done saying goodbye" if options[:verbose]
      end

      ## the version command define
      #
      desc "version", "Zen version"
      def version
        logger.debug("zen version.....logging cli....")
        puts VERSION
      end

      # finish application container initial
      Application.finalize!

      # Application.keys

      ##
      # the SubCommand defines

      # foreach all keys from Application.keys and filter the commands key,
      # after register the command class as subcommand for CLI Tool
      #
      Application.keys.grep(/commands./) do |command_key|
        # fetch command class instance from application containter
        command_class = Application.resolve(command_key)

        # register the {command_class} as subcommand for CLI tool
        desc command_class.class.command_usage, command_class.class.command_description
        subcommand command_class.class.command_name, command_class.class
      end
    end
  end
end
