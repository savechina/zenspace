require "thor"
# require_relative "version"
require "yaml"
require_relative "cmd/repo"

module Zen
  class CLI < Thor
    map "-v" => :version

    class_option :verbose, type: :boolean

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

    desc "version", "Zen version"
    def version
      puts VERSION.to_s
    end

    desc "repo SUBCOMMAND ...ARGS", "manage set of tracked repositories"
    subcommand "repo", Zen::RepoCmd
  end
end
