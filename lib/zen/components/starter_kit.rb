# typed: true
# frozen_string_literal: true

require "fileutils"
require "erb"
require "active_support/all"

module Zen
  module Components
    require_relative "starter_kit/model/java_project"
    # require_relative "starter_kit/repository/starter_repository"

    ##
    # StarterKit
    #
    class StarterKit
      include Import["logger"]
      include Import["zen.components.starter_kit.repository.starter_repository"]

      # class StarterKit
      # end
      def load(project_name, group_name, package_name, output_root)
        logger.info "StarterKit init project load ...."
        project = StarterKit::Model::JavaProject.new(
          project_name:,
          group_name:,
          package_name:
        )

        logger.info "StarterKit::JavaProject :#{project.group_name}"

        java_starter = JavaScaffold.new(project_name, group_name, package_name, output_root)

        java_starter.generate(output_root)

        logger.info "StarterKit init project done."
      end

      def add(_class_name)
        # starter_repository = StarterRepository.new

        all_tables = starter_repository.fetch_all_tables

        all_tables.each do |table_name|
          table_schama = starter_repository.fetch_table_schema(table_name)

          table_schama.each do |col, value|
            puts "#{col},#{value[:type]}"
          end
        end

        all_class = starter_repository.fetch_all_class

        puts "All Java Class: "
        pp all_class

        # Prepare data
        @table_name = "table_name"

        @columns = [
          { column_name: "id", data_type: "INT" },
          { column_name: "name", data_type: "VARCHAR(255)" },
          { column_name: "email", data_type: "VARCHAR(255)" }
        ]

        class_template_name = "class.erb"
        template_class = File.join(Zen::TEMPLATE_ROOT, "class", class_template_name)

        # Create ERB template object
        template = ERB.new(File.read(template_class))

        # Prepare template data
        template_data = {
          table_name: @table_name,
          columns: @columns
        }

        puts template_data

        b = binding
        puts binding.local_variables

        # Parse and generate Java class code
        generated_code = template.result(binding)

        puts "generated_code : #{generated_code}"
      end

      #
      # Scaffold 脚手架
      #
      class Scaffold
        String TEMPLATE_ROOT = "templates"

        def initialize; end

        def new; end
      end

      #
      # Java 脚手架工具
      #
      class JavaScaffold < Scaffold
        log = Application["logger"]

        # template name
        TEMPLATE_NAME = "java"

        attr_accessor :project_name, :project_group, :package_name, :output_base

        def initialize(project_name, group_name, package_name, output_root = nil)
          @project_name = project_name
          @group_name = group_name
          @project_package = package_name
          @output_root = output_root
        end

        ##
        # generate project structure
        def generate(output_root)
          # template root directory
          templates_root = File.join(Zen::TEMPLATE_ROOT, TEMPLATE_NAME)

          # output root directory
          target_root = File.join(output_root, ".")

          puts "JavaStarter generate ...#{templates_root},#{target_root}"

          # init project output directory
          init_project_dir(target_root)

          handle_templates(templates_root) do |source_name|
            # handle template directory
            target_name = handle_template_file(source_name, true)

            puts "source：#{source_name} ,target: #{target_name} "

            source = File.join(templates_root, source_name)
            target = File.join(target_root, target_name)

            if Dir.exist?(source)
              FileUtils.mkdir_p(target)
            else
              content = handle_template_file(File.read(source), false)
              File.write(target, content)
            end
          end
        end

        ##
        # init project dir in output dir
        def init_project_dir(project_dir)
          puts("create:")

          FileUtils.mkdir(project_dir)
        rescue Errno::EEXIST
          puts("directory already exists: #{project_dir}")
        end

        #
        # foreach template file,and process template
        #
        def handle_templates(template_root)
          root = Pathname.new(template_root)

          #
          # foreach all template file
          Dir.glob("#{template_root}/**/**").each do |path|
            # template file path
            el = Pathname.new(path)

            # template source file name
            source_file = el.relative_path_from(root).to_s

            # handle template file
            yield(source_file)
          end
        end

        ##
        # eval template variables
        #
        def handle_template_file(str, isPath)
          # package directory in path replace pathname
          package_name = if isPath
                           @project_package.gsub(".", "/")
                         else
                           @project_package
                         end
          #
          # project
          # __app__   project anme
          # __group__ maven group
          #
          str.gsub(/__app__/, @project_name)
             .gsub(/__group__/, @group_name)
             .gsub(/__package__/, package_name)
        end
      end
    end
  end
end
