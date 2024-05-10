# typed: true
# frozen_string_literal: true

require "dry/system"
require "dry/system/container"
# require "zen"
module Zen
  ##
  # Application Container
  # @see [Dry::System::Container]
  #
  class Application < Dry::System::Container
    # use plugin
    use :logging
    use :env, inferrer: -> { ENV.fetch("APP_ENV", :development).to_sym }
    # use :monitoring
    use :zeitwerk, debug: false

    # application config
    configure do |config|
      # define application root workspace
      config.root = File.dirname(File.expand_path(File.join(__dir__, "../..")))

      # puts "application root :#{config.root} #{Zen::VERSION}  #{Zen::ROOT}  #{Zen::USER_ROOT}"

      # component dir lib
      config.component_dirs.add "lib" do |dir|
        #
        # filter and  load componet
        dir.auto_register = lambda do |component|
          component.identifier.start_with?("zen.commands") || component.identifier.start_with?("zen.components")
        end
      end

      # define inflector customize acronym
      config.inflector = Dry::Inflector.new do |inflections|
        inflections.acronym("CLI")
      end

      log_dir = File.join(Zen::USER_ROOT, "log")

      FileUtils.mkdir_p(log_dir) unless File.exist?(log_dir)

      config.log_dir = log_dir
    end
  end

  #
  # Register Zen System Provider
  Dry::System.register_provider_sources(File.join(__dir__, "providers"))
end
