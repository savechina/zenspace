# typed: true
# frozen_string_literal: true

module Zen
  module Commands
    require "zen/components/starter_kit/model/java_project"

    ##
    # RepoCommand for repo Utils
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

      def init(project_name = nil, group_name = nil, package_name = nil, output_root = nil)
        puts "#{project_name},#{group_name},#{package_name},#{output_root}"

        # if project_name.nil?
        #   puts "init must project_name args is nil"
        #   exit!(1)
        # end

        raise ArgumentError, "init must given <project_name> args is nil" if project_name.nil?

        raise ArgumentError, "init must <group_name> args is nil" if group_name.nil?

        raise ArgumentError, "init must <package_name> args is nil" if package_name.nil?

        # if output is nil set current to output dir
        output_root = Dir.pwd if output_root.nil?

        puts "output: #{output_root}"

        # @type [Zen::Components::StarterKit::Model::JavaProject]
        java_project = Zen::Components::StarterKit::Model::JavaProject.new(
          project_name:,
          group_name:,
          package_name:
        )
        # puts java_project.project_name
        # java_project.group_name = group_name

        starter_kit.load(project_name, group_name, package_name, output_root)
      rescue StandardError => e
        puts "starter init error, Error:#{e.message}"
        exit(1)
      end

      ##
      # new command
      desc "add", "init project from template"
      def add(project_name)
        puts project_name

        starter_kit.add(project_name)
      end

      # base define
      self.command_name = "starter"
      self.command_usage = "starter [COMMAND]"
      self.command_description = "init project from template and add new feature to prjoct"
    end
  end
end
