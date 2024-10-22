# typed: true
# frozen_string_literal: true

module Zen
  module Commands
    require "zen/components/starter_kit/model/java_project"

    ##
    # StarterCommand for initialize work
    #
    class StarterCommand < Zen::Commands::BaseCommand
      include Import["zen.components.starter_kit"
                  ]
      ##
      # init command
      #
      desc "init <PROJECT> <GROUP> <PACKAGE>", "init project from template by project."
      long_desc <<-LONGDESC
        init project from template, <template name > is java ,give project have  <project name>, <group name>, <package name> ,[output root] etc four elements.

        1. <project name> , the maven project  artifact name

        2. <group name> , the maven project  artifact group

        3. <package name>, the maven project java base package name.

        4. <output root> , optional ,value is  default current directory.

        Example:

        $ zen starter init bluekit-sample org.renyan.bluekit.sample  org.renyan.bluekit.sample

      LONGDESC
      option :verbose, type: :boolean, aliases: "-v", desc: "Verbose output"
      option :force, type: :boolean, aliases: "-f", desc: "Force Overwriter"
      option :arch_type, type: :string, aliases: "-t", default: "ddd", desc: "arch type template name"
      def init(project_name = nil, group_name = nil, package_name = nil, output_root = nil)
        #        puts "#{project_name},#{group_name},#{package_name},#{output_root}"

        raise ArgumentError, "init must given <project_name> args is nil" if project_name.nil?

        raise ArgumentError, "init must <group_name> args is nil" if group_name.nil?

        raise ArgumentError, "init must <package_name> args is nil" if package_name.nil?

        # if output is nil set current to output dir
        output_root = Dir.pwd if output_root.nil?

        overwrite = options[:force]

        arch_type = options[:arch_type]

        Application.logger.level = "DEBUG" if options[:verbose]

        Application[:settings].options.verbose = true if options[:verbose]

        Application[:settings].options.force = true if options[:force]

        # @type [Bluekit::Components::StarterKit::Model::JavaProject]
        project = Components::StarterKit::Model::JavaProject.new
        # project = java_project
        project.project_name = project_name
        project.group_name = group_name
        project.package_name = package_name

        project.arch_type = if arch_type.nil? || arch_type.strip.empty?
                              ENV.fetch("arch_type", "ddd")
                            else
                              arch_type
                            end

        starter_kit.load(project, output_root)

        puts "Starter init project, #{project_name},#{group_name},#{package_name}!" if options[:verbose]
      rescue StandardError => e
        puts "starter init error, Error:#{e.message}"
        exit(1)
      end

      ##
      # add command
      desc "add NAME TABLE_NAME", "Add project feature"
      option :all, type: :boolean, aliases: "-a", desc: "enable all layer"
      option :verbose, type: :boolean, aliases: "-v", desc: "Verbose output"
      option :controler, type: :boolean, aliases: "-c", desc: "enable controler layer"
      option :infra, type: :boolean, aliases: "-i", desc: "enable infrastructure layer"
      option :project_name, type: :string, aliases: "-p", desc: "project name"
      option :package_name, type: :string, desc: "base package name"
      option :group_name, type: :string, desc: "group name"
      option :force, type: :boolean, aliases: "-f", desc: "Force Overwriter"
      option :arch_type, type: :string, aliases: "-t", default: "ddd", desc: "arch type template name"
      def add(feature_name, table_name, output_root = nil)
        project_name = options[:project_name]

        package_name = options[:package_name]

        group_name = options[:group_name]

        overwrite = options[:force]

        arch_type = options[:arch_type]

        # if output is nil set current to output dir
        output_root = Dir.pwd if output_root.nil? || output_root.strip.empty?

        Application.logger.level = "DEBUG" if options[:verbose]

        Application[:settings].options.verbose = true if options[:verbose]

        Application[:settings].options.force = true if options[:force]

        @project = Components::StarterKit::Model::JavaProject.new

        @project.project_name = if project_name.nil? || project_name.strip.empty?
                                  ENV.fetch("project_name", nil)
                                else
                                  project_name
                                end

        @project.package_name = if package_name.nil? || package_name.strip.empty?
                                  ENV.fetch("package_name", nil)
                                else
                                  package_name
                                end

        @project.group_name = if group_name.nil? || group_name.strip.empty?
                                ENV.fetch("group_name", nil)
                              else
                                group_name
                              end

        # pp @project

        @project.arch_type = if arch_type.nil? || arch_type.strip.empty?
                               ENV.fetch("arch_type", "ddd")
                             else
                               arch_type
                             end

        # pp @project

        starter_kit.add(@project, feature_name, table_name, output_root)

        puts "Hello, #{@project.project_name}!,#{table_name}"
      end

      desc "workspace ", "Initialize Worksapce directory structure."
      def workspace
        # call starter workspace initialize
        starter_kit.workspace
      end

      des "develop", "Initialize develop ENV to install develop tools."
      def develop
        starter_kit.develop_tools
      end

      # base define
      self.command_name = "starter"
      self.command_usage = "starter [COMMAND]"
      self.command_description = "init project from template and add new feature to prjoct"
    end
  end
end
