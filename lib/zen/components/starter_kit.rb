# typed: true
# frozen_string_literal: true

require "fileutils"
require "erb"
require "active_support/all"

module Zen
  module Components
    require_relative "starter_kit/model/java_project"

    ##
    # StarterKit
    #
    class StarterKit
      include Import["logger", "settings"]
      include Import["zen.components.starter_kit.repository.starter_repository"]

      # settings do
      #   setting :verbose
      #   setting :force
      # end

      #
      # @param [project]
      def load(project, output_root)
        puts "StarterKit in ... #{Application["settings"].logger_level} .." if Application.logger.debug?

        puts "settings  #{settings.options.verbose}"

        @user_template = File.join(USER_TEMPLATE_ROOT, "starter/ddd_init")

        @template = File.join(TEMPLATE_ROOT, "starter/ddd_init")

        puts "Load:..#{project}"
        template_base = Pathname.new(@user_template)

        puts "JavaTypes:#{StarterKit::Model::JavaTypes::VARCHAR[:jdbc_type]}"

        puts "StarterKit ... load templates :#{@template}, #{@user_template}"

        logger.info "StarterKit::JavaProject :#{project.group_name}"

        if project.arch_type.downcase == "ddd"
          puts "use arch type. #{project.arch_type}"

          template_base = load_template("starter/ddd_init")

        elsif project.arch_type.downcase == "mvc"
          puts "use arch type. #{project.arch_type}"

          template_base = load_template("starter/mvc_init")

        else
          puts "not support arch type.#{project.arch_type}"
        end

        java_starter = JavaScaffold.new(project.project_name, project.group_name, project.package_name, output_root)

        java_starter.generate(template_base, output_root)

        logger.info "StarterKit init project done."
      end

      # def add(_class_name)
      #   # starter_repository = StarterRepository.new

      #   all_tables = starter_repository.fetch_all_tables

      #   all_tables.each do |table_name|
      #     table_schama = starter_repository.fetch_table_schema(table_name)

      #     table_schama.each do |col, value|
      #       puts "#{col},#{value[:type]}"
      #     end
      #   end

      #   all_class = starter_repository.fetch_all_class

      #   puts "All Java Class: "
      #   pp all_class

      #   # Prepare data
      #   @table_name = "table_name"

      #   @columns = [
      #     { column_name: "id", data_type: "INT" },
      #     { column_name: "name", data_type: "VARCHAR(255)" },
      #     { column_name: "email", data_type: "VARCHAR(255)" }
      #   ]

      #   class_template_name = "class.erb"
      #   template_class = File.join(Zen::TEMPLATE_ROOT, "class", class_template_name)

      #   # Create ERB template object
      #   template = ERB.new(File.read(template_class))

      #   # Prepare template data
      #   template_data = {
      #     table_name: @table_name,
      #     columns: @columns
      #   }

      #   puts template_data

      #   b = binding
      #   puts binding.local_variables

      #   # Parse and generate Java class code
      #   generated_code = template.result(binding)

      #   puts "generated_code : #{generated_code}"
      # end

      ##
      # Starter Add feature
      # @api
      def add(project, feature_name, table_name, output_root)
        # puts "JavaTypes:#{StarterKit::Model::JavaTypes.value("varchar".upcase.to_sym)}"
        # puts "settings  #{settings.options.verbose}"

        all_tables = starter_repository.fetch_all_tables

        clazz_model = starter_repository.fetch_all_class("#{table_name}").first

        clazz_model.package_name = "#{feature_name}"
        clazz_model.feature_name = feature_name

        # 生成新的文件

        if project.arch_type.downcase == "ddd"
          puts "use arch type. #{project.arch_type}"

          add_ddd(project, feature_name, table_name, clazz_model, output_root)
        elsif project.arch_type.downcase == "mvc"
          puts "use arch type. #{project.arch_type}"

          add_mvc(project, feature_name, table_name, clazz_model, output_root)
        else
          puts "not support arch type.#{project.arch_type}"
        end

        puts "add: #{table_name} done"
      end

      def add_ddd(project, feature_name, table_name, clazz_model, output_root)
        # puts "JavaTypes:#{StarterKit::Model::JavaTypes.value("varchar".upcase.to_sym)}"
        # puts "settings  #{settings.options.verbose}"

        clazz_model.package_name = "#{feature_name}"
        clazz_model.feature_name = feature_name

        # puts "#{table_name} create class : #{pp clazz_model}"

        template_base = load_template("starter/ddd_spec")

        class_template_name = "model.erb"
        class_template_name = "entity.java.erb"
        mapper_template_name = "mapper.xml.erb"

        # Entity class
        # @example
        #   __app__/__app__-infrastructure/src/main/java/__package__/infrastructure/entity
        entity_module = Zen::Components::StarterKit::Model::JavaModule.new

        entity_module.module_name = "ENTITY"
        entity_module.module_template = "entity.java.erb"
        entity_module.project = project
        entity_module.module_package = "infrastructure.entity"
        entity_module.module_path = "#{project.project_name}-infrastructure"
        entity_module.module_type = "java"

        # Mapper Interface Module
        # @example
        #   __app__/__app__-infrastructure/src/main/java/__package__/infrastructure/mapper
        #   __app__/__app__-infrastructure/src/main/java/__package__/infrastructure/mapper/user/xxxMapper.java
        # @type [Model::JavaModule]
        mapper_module = Zen::Components::StarterKit::Model::JavaModule.new

        mapper_module.module_name = "MAPPER"
        mapper_module.module_template = "dao.java.erb"
        mapper_module.project = project
        mapper_module.module_package = "infrastructure.mapper"
        mapper_module.module_path = "#{project.project_name}-infrastructure"
        mapper_module.module_type = "java"
        mapper_module.module_suffix = "Mapper"
        mapper_module.module_output = "Mapper.java"

        # Mapper Resource Module
        # __app__/__app__-infrastructure/src/main/resources/mapper/xxxMapper.xml
        mapper_res_module = Zen::Components::StarterKit::Model::JavaModule.new

        mapper_res_module.module_name = "MAPPER_RESOURCE"
        mapper_res_module.module_template = "mapper.xml.erb"
        mapper_res_module.project = project
        mapper_res_module.module_package = "mapper"
        mapper_res_module.module_path = "#{project.project_name}-infrastructure"
        mapper_res_module.module_type =  Model::JavaModule::RESOURCE_TYPE
        mapper_res_module.module_suffix = "Mapper"
        mapper_res_module.module_output = "Mapper.xml"

        # Entity Convert Module
        #
        # @example
        #      __app__/__app__-infrastructure/src/main/java/__package__/infrastructure/convert
        entity_convert_module = Zen::Components::StarterKit::Model::JavaModule.new

        entity_convert_module.module_name = "ENTITY_CONVERT"
        entity_convert_module.module_template = "entity_convert.java.erb"
        entity_convert_module.project = project
        entity_convert_module.module_package = "infrastructure.convert"
        entity_convert_module.module_path = "#{project.project_name}-infrastructure"
        entity_convert_module.module_type = Model::JavaModule::SOURCE_TYPE
        entity_convert_module.module_suffix = "EntityConvert"
        entity_convert_module.module_output = "EntityConvert.java"

        # Domain Model Module
        #
        # @example
        #      __app__/__app__-domain/src/main/java/__package__/domain/model
        #      __app__/__app__-domain/src/main/java/__package__/domain/model/sku/SkuDO.java
        model_module = Zen::Components::StarterKit::Model::JavaModule.new

        model_module.module_name = "MODEL"
        model_module.module_template = "model.java.erb"
        model_module.project = project
        model_module.module_package = "domain.model"
        model_module.module_path = "#{project.project_name}-domain"
        model_module.module_type = Model::JavaModule::SOURCE_TYPE
        model_module.module_suffix = "DO"
        model_module.module_output = "DO.java"

        # Repository Module
        #
        # @example
        # __app__/__app__-domain/src/main/java/__package__/domain/repository/sku
        # __app__/__app__-domain/src/main/java/__package__/domain/repository/sku/SkuExampleRepository.java
        #
        repository_module = Zen::Components::StarterKit::Model::JavaModule.new

        repository_module.module_name = "REPOSITORY"
        repository_module.module_template = "repository.java.erb"
        repository_module.project = project
        repository_module.module_package = "domain.repository"
        repository_module.module_path = "#{project.project_name}-domain"
        repository_module.module_type = Model::JavaModule::SOURCE_TYPE
        repository_module.module_suffix = "Repository"
        repository_module.module_output = "Repository.java"
        # Repository Impl Module
        #
        # @example
        #   __app__/__app__-infrastructure/src/main/java/__package__/infrastructure/repository
        #   __app__/__app__-infrastructure/src/main/java/__package__/infrastructure/repository/sku/xxxRepositoryImpl.java
        repository_impl_module = Zen::Components::StarterKit::Model::JavaModule.new

        repository_impl_module.module_name = "REPOSITORY_IMPL"
        repository_impl_module.module_template = "repository_impl.java.erb"
        repository_impl_module.project = project
        repository_impl_module.module_package = "infrastructure.repository"
        repository_impl_module.module_path = "#{project.project_name}-infrastructure"
        repository_impl_module.module_type = Model::JavaModule::SOURCE_TYPE
        repository_impl_module.module_suffix = "RepositoryImpl"
        repository_impl_module.module_output = "RepositoryImpl.java"

        pp repository_impl_module.model(clazz_model)

        #
        # __app__/__app__-domain/src/main/java/__package__/domain/service
        # __app__/__app__-domain/src/main/java/__package__/domain/service/sku/SkuService.java
        service_module = Zen::Components::StarterKit::Model::JavaModule.new
        service_module.module_name = "SERVICE"
        service_module.module_template = "service.java.erb"
        service_module.project = project
        service_module.module_package = "domain.service"
        service_module.module_path = "#{project.project_name}-domain"
        service_module.module_type = Model::JavaModule::SOURCE_TYPE
        service_module.module_suffix = "Service"
        service_module.module_output = "Service.java"

        # __app__/__app__-domain/src/main/java/__package__/domain/service/sku/impl/SkuServiceImpl.java
        #
        service_impl_module = Zen::Components::StarterKit::Model::JavaModule.new
        service_impl_module.module_name = "SERVICE_IMPL"
        service_impl_module.module_template = "service_impl.java.erb"
        service_impl_module.project = project
        service_impl_module.module_package = "domain.service"
        service_impl_module.module_path = "#{project.project_name}-domain"
        service_impl_module.module_type = Model::JavaModule::SOURCE_TYPE
        service_impl_module.module_suffix = "ServiceImpl"
        service_impl_module.module_output = "ServiceImpl.java"

        # __app__/__app__-domain/src/main/java/__package__/domain/vo
        # __app__/__app__-domain/src/main/java/__package__/domain/vo/sku/SkuDetailVo.java
        #

        view_module = Zen::Components::StarterKit::Model::JavaModule.new
        view_module.module_name = "VIEW"
        view_module.module_template = "vo.java.erb"
        view_module.project = project
        view_module.module_package = "domain.vo"
        view_module.module_path = "#{project.project_name}-domain"
        view_module.module_type = Model::JavaModule::SOURCE_TYPE
        view_module.module_suffix = "Vo"
        view_module.module_output = "Vo.java"

        ##
        # @type [Hash(String,JavaModule)]
        modules = { entity: entity_module,
                    mapper: mapper_module,
                    mapper_resource: mapper_res_module,
                    entity_convert: entity_convert_module,
                    model: model_module,
                    repository: repository_module,
                    repository_impl: repository_impl_module,
                    service: service_module,
                    service_impl: service_impl_module,
                    vo: view_module }

        # Prepare template data
        context = Model::JavaContext.new
        context.project = project
        context.model = clazz_model
        context.modules = modules
        # pp context

        # Mapper MyBatis Module Process template
        template_name = mapper_res_module.module_template

        output_path = File.join(output_root,
                                mapper_res_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Mapper.xml")

        process_template(template_base, template_name, output_path, context)

        # Entity Module Process template
        template_name = entity_module.module_template

        output_path = File.join(output_root,
                                entity_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Entity.java")

        process_template(template_base, template_name, output_path, context)

        # Mapper Module process template
        template_name = mapper_module.module_template

        output_path = File.join(output_root,
                                mapper_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Mapper.java")

        process_template(template_base, template_name, output_path, context)

        # Entity Convert Module process template
        template_name = entity_convert_module.module_template

        output_path = File.join(output_root,
                                entity_convert_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}EntityConvert.java")

        process_template(template_base, template_name, output_path, context)

        # Model Module process template
        template_name = model_module.module_template

        output_path = File.join(output_root,
                                model_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}DO.java")

        process_template(template_base, template_name, output_path, context)

        # Repository Module process template
        template_name = repository_module.module_template

        output_path = File.join(output_root,
                                repository_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Repository.java")

        process_template(template_base, template_name, output_path, context)

        # Repository Impl Module process template
        template_name = repository_impl_module.module_template

        output_path = File.join(output_root,
                                repository_impl_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}RepositoryImpl.java")

        process_template(template_base, template_name, output_path, context)

        # Service Module process template
        template_name = service_module.module_template

        output_path = File.join(output_root,
                                service_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Service.java")

        process_template(template_base, template_name, output_path, context)

        # Service Impl Module process template
        template_name = service_impl_module.module_template

        output_path = File.join(output_root,
                                service_impl_module.full_path,
                                clazz_model.package_name,
                                "impl",
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}ServiceImpl.java")

        process_template(template_base, template_name, output_path, context)

        # View Module process template
        template_name = view_module.module_template

        output_path = File.join(output_root,
                                view_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Vo.java")
        process_template(template_base, template_name, output_path, context)

        # 生成新的文件

        puts "add: #{table_name} done"
      end

      def add_mvc(project, feature_name, table_name, clazz_model, output_root)
        # puts "JavaTypes:#{StarterKit::Model::JavaTypes.value("varchar".upcase.to_sym)}"
        # puts "settings  #{settings.options.verbose}"

        clazz_model.package_name = "#{feature_name}"
        clazz_model.feature_name = feature_name

        # puts "#{table_name} create class : #{pp clazz_model}"

        template_base = load_template("starter/mvc_spec")

        # Entity class
        # @example
        #   __app__/__app__-domain/src/main/java/__package__/domain/entity
        entity_module = Zen::Components::StarterKit::Model::JavaModule.new

        entity_module.module_name = "ENTITY"
        entity_module.module_template = "entity.java.erb"
        entity_module.project = project
        entity_module.module_package = "domain.entity"
        entity_module.module_path = "#{project.project_name}-domain"
        entity_module.module_type = "java"

        # Mapper Interface Module
        # @example
        #   __app__/__app__-domain/src/main/java/__package__/domain/mapper
        #   __app__/__app__-domain/src/main/java/__package__/domain/mapper/user/xxxMapper.java
        # @type [Model::JavaModule]
        mapper_module = Zen::Components::StarterKit::Model::JavaModule.new

        mapper_module.module_name = "MAPPER"
        mapper_module.module_template = "dao.java.erb"
        mapper_module.project = project
        mapper_module.module_package = "domain.mapper"
        mapper_module.module_path = "#{project.project_name}-domain"
        mapper_module.module_type = "java"
        mapper_module.module_suffix = "Mapper"
        mapper_module.module_output = "Mapper.java"

        # Mapper Resource Module
        # __app__/__app__-domain/src/main/resources/mapper/xxxMapper.xml
        mapper_res_module = Zen::Components::StarterKit::Model::JavaModule.new

        mapper_res_module.module_name = "MAPPER_RESOURCE"
        mapper_res_module.module_template = "mapper.xml.erb"
        mapper_res_module.project = project
        mapper_res_module.module_package = "mapper"
        mapper_res_module.module_path = "#{project.project_name}-domain"
        mapper_res_module.module_type = "resource"
        mapper_res_module.module_suffix = "Mapper"
        mapper_res_module.module_output = "Mapper.xml"

        # Entity Convert Module
        #
        # @example
        #      __app__/__app__-domain/src/main/java/__package__/domain/convert
        entity_convert_module = Zen::Components::StarterKit::Model::JavaModule.new

        entity_convert_module.module_name = "ENTITY_CONVERT"
        entity_convert_module.module_template = "entity_convert.java.erb"
        entity_convert_module.project = project
        entity_convert_module.module_package = "domain.convert"
        entity_convert_module.module_path = "#{project.project_name}-domain"
        entity_convert_module.module_type = Model::JavaModule::SOURCE_TYPE
        entity_convert_module.module_suffix = "EntityConvert"
        entity_convert_module.module_output = "EntityConvert.java"

        # Domain Model Module
        #
        # @example
        #      __app__/__app__-domain/src/main/java/__package__/domain/model
        #      __app__/__app__-domain/src/main/java/__package__/domain/model/sku/SkuDO.java
        model_module = Zen::Components::StarterKit::Model::JavaModule.new

        model_module.module_name = "MODEL"
        model_module.module_template = "model.java.erb"
        model_module.project = project
        model_module.module_package = "domain.model"
        model_module.module_path = "#{project.project_name}-domain"
        model_module.module_type = Model::JavaModule::SOURCE_TYPE
        model_module.module_suffix = "DO"
        model_module.module_output = "DO.java"

        # Repository Module
        #
        # @example
        # __app__/__app__-domain/src/main/java/__package__/domain/repository/sku
        # __app__/__app__-domain/src/main/java/__package__/domain/repository/sku/SkuExampleRepository.java
        #
        repository_module = Zen::Components::StarterKit::Model::JavaModule.new

        repository_module.module_name = "REPOSITORY"
        repository_module.module_template = "repository.java.erb"
        repository_module.project = project
        repository_module.module_package = "domain.repository"
        repository_module.module_path = "#{project.project_name}-domain"
        repository_module.module_type = Model::JavaModule::SOURCE_TYPE
        repository_module.module_suffix = "Repository"
        repository_module.module_output = "Repository.java"
        # Repository Impl Module
        #
        # @example
        #   __app__/__app__-infrastructure/src/main/java/__package__/infrastructure/repository
        #   __app__/__app__-infrastructure/src/main/java/__package__/infrastructure/repository/sku/xxxRepositoryImpl.java
        repository_impl_module = Zen::Components::StarterKit::Model::JavaModule.new

        repository_impl_module.module_name = "REPOSITORY_IMPL"
        repository_impl_module.module_template = "repository_impl.java.erb"
        repository_impl_module.project = project
        repository_impl_module.module_package = "domain.repository"
        repository_impl_module.module_path = "#{project.project_name}-domain"
        repository_impl_module.module_type = Model::JavaModule::SOURCE_TYPE
        repository_impl_module.module_suffix = "RepositoryImpl"
        repository_impl_module.module_output = "RepositoryImpl.java"

        pp repository_impl_module.model(clazz_model)

        #
        # __app__/__app__-domain/src/main/java/__package__/domain/service
        # __app__/__app__-domain/src/main/java/__package__/domain/service/sku/SkuService.java
        service_module = Zen::Components::StarterKit::Model::JavaModule.new
        service_module.module_name = "SERVICE"
        service_module.module_template = "service.java.erb"
        service_module.project = project
        service_module.module_package = "domain.service"
        service_module.module_path = "#{project.project_name}-domain"
        service_module.module_type = Model::JavaModule::SOURCE_TYPE
        service_module.module_suffix = "Service"
        service_module.module_output = "Service.java"

        # __app__/__app__-domain/src/main/java/__package__/domain/service/sku/impl/SkuServiceImpl.java
        #
        service_impl_module = Zen::Components::StarterKit::Model::JavaModule.new
        service_impl_module.module_name = "SERVICE_IMPL"
        service_impl_module.module_template = "service_impl.java.erb"
        service_impl_module.project = project
        service_impl_module.module_package = "domain.service"
        service_impl_module.module_path = "#{project.project_name}-domain"
        service_impl_module.module_type = Model::JavaModule::SOURCE_TYPE
        service_impl_module.module_suffix = "ServiceImpl"
        service_impl_module.module_output = "ServiceImpl.java"

        # __app__/__app__-domain/src/main/java/__package__/domain/vo
        # __app__/__app__-domain/src/main/java/__package__/domain/vo/sku/SkuDetailVo.java
        #

        view_module = Zen::Components::StarterKit::Model::JavaModule.new
        view_module.module_name = "VIEW"
        view_module.module_template = "vo.java.erb"
        view_module.project = project
        view_module.module_package = "domain.vo"
        view_module.module_path = "#{project.project_name}-domain"
        view_module.module_type = Model::JavaModule::SOURCE_TYPE
        view_module.module_suffix = "Vo"
        view_module.module_output = "Vo.java"

        ##
        # @type [Hash(String,JavaModule)]
        modules = { entity: entity_module,
                    mapper: mapper_module,
                    mapper_resource: mapper_res_module,
                    entity_convert: entity_convert_module,
                    model: model_module,
                    repository: repository_module,
                    repository_impl: repository_impl_module,
                    service: service_module,
                    service_impl: service_impl_module,
                    vo: view_module }

        # Prepare template data
        context = Model::JavaContext.new
        context.project = project
        context.model = clazz_model
        context.modules = modules
        # pp context

        # Mapper MyBatis Module Process template
        template_name = mapper_res_module.module_template

        output_path = File.join(output_root,
                                mapper_res_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Mapper.xml")

        process_template(template_base, template_name, output_path, context)

        # Entity Module Process template
        template_name = entity_module.module_template

        output_path = File.join(output_root,
                                entity_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Entity.java")

        process_template(template_base, template_name, output_path, context)

        # Mapper Module process template
        template_name = mapper_module.module_template

        output_path = File.join(output_root,
                                mapper_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Mapper.java")

        process_template(template_base, template_name, output_path, context)

        # Entity Convert Module process template
        template_name = entity_convert_module.module_template

        output_path = File.join(output_root,
                                entity_convert_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}EntityConvert.java")

        process_template(template_base, template_name, output_path, context)

        # Model Module process template
        template_name = model_module.module_template

        output_path = File.join(output_root,
                                model_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}DO.java")

        process_template(template_base, template_name, output_path, context)

        # Service Module process template
        template_name = service_module.module_template

        output_path = File.join(output_root,
                                service_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Service.java")

        process_template(template_base, template_name, output_path, context)

        # Service Impl Module process template
        template_name = service_impl_module.module_template

        output_path = File.join(output_root,
                                service_impl_module.full_path,
                                clazz_model.package_name,
                                "impl",
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}ServiceImpl.java")

        process_template(template_base, template_name, output_path, context)

        # View Module process template
        template_name = view_module.module_template

        output_path = File.join(output_root,
                                view_module.full_path,
                                clazz_model.package_name,
                                "#{clazz_model.feature_name.camelcase}#{clazz_model.class_name}Vo.java")

        process_template(template_base, template_name, output_path, context)

        # 生成新的文件

        puts "add: #{table_name} done"
      end

      ##
      # load template base from template root
      def load_template(template_base)
        # template root directory
        templates_root = File.join(Zen::TEMPLATE_ROOT, template_base)

        if !Dir.exist?(templates_root) && Application.logger.debug?
          Application.logger.debug "JavaStarter: template not exists ."
        end

        # use template root directory
        user_template = File.join(Zen::USER_TEMPLATE_ROOT, template_base)

        if Dir.exist?(user_template)
          templates_root = user_template
          Application.logger.debug "JavaScaffold: user template exists, use this template." if Application.logger.debug?
        end

        templates_root
      end

      require "erb"
      require "fileutils"

      # 定义一个方法来处理模板替换
      # @api private
      def process_template(template_base, template_name, output_path, context)
        template_path = File.join(template_base, template_name)
        puts "template name :#{template_path}" if settings.options.verbose

        # 读取模板文件内容
        template_content = File.read(template_path)

        # 使用ERB进行模板替换
        template = ERB.new(template_content, trim_mode: "-")
        result = template.result(context.get_binding)

        # 生成输出文件的完整路径
        relative_path = template_name.camelcase # 移除模板目录前缀
        output = File.join(output_path, relative_path.sub(/\.erb$/, "")) # 移除.erb扩展名
        # puts "output template name :#{output}"

        # 确保输出目录存在
        FileUtils.mkdir_p(File.dirname(output_path))

        puts "output: file. file:#{output_path}" if settings.options.verbose

        if File.exist?(output_path)

          if settings.options.force
            puts "output: file is exist. overwrite."
            # 生成新的文件
            File.write(output_path, result)
          elsif settings.options.verbose
            puts "output: file is exist."
          end

          nil
        else
          # 生成新的文件
          File.write(output_path, result)
        end

        # puts "output: #{output_path}"
      end

      ##
      # Workspace initialize
      def workspace
        # os home directory
        home = Dir.home

        file_list = [
          File.join(home, "export"),
          File.join(home, "CodeRepo/ownspace"),
          File.join(home, "CodeRepo/funspace"),
          File.join(home, "CodeRepo/acespace"),
          File.join(home, "CodeRepo/workspace"),
          File.join(home, "CodeRepo/workspace/airp"),
          File.join(home, "CodeRepo/workspace/bluekit"),
          File.join(home, "Documents/Work"),
          File.join(home, "Documents/Other"),
          File.join(home, "Documents/Personal"),
          File.join(home, "Documents/Archive"),
          File.join(home, "Software")
        ]

        file_list.each do |file|
          if File.exist?(file)
            puts "#{file} exists"
          else
            FileUtils.mkdir_p file
          end

          puts "workspace init: #{file}"
        end
        # initialize exprot
        exec "ln -s #{File.join(home, "export")} /export"
      end

      ##
      # develop tools install to workspace
      def develop_tools
        java_version = "openjdk@11"

        tools = [java_version,
                 "jenv",
                 "rbenv",
                 "zstd",
                 "maven",
                 "intellij-idea",
                 "visual-studio-code",
                 "iterm2",
                 "dbeaver-community",
                 "cloc",
                 "octosql",
                 "tree",
                 "sqlite",
                 "uv",
                 "zed",
                 "helix",
                 "bat",
                 "zoxide",
                 "fzf",
                 "ripgrep"]

        # install develop tools
        #
        tools.each do |tool|
          puts "start to install  #{tool} ...."
          system("brew install #{tool}")
        end

        # config jenv add openjdk to versions
        #
        exec "jenv add /opt/homebrew/opt/#{java_version}/libexec/openjdk.jdk/Contents/Home"
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
        TEMPLATE_NAME = "starter/ddd_init"

        attr_accessor :project_name, :project_group, :package_name, :output_base

        def initialize(project_name, group_name, package_name, output_root = nil)
          @project_name = project_name
          @group_name = group_name
          @project_package = package_name
          @output_root = output_root
        end

        ##
        # generate project structure
        def generate(template_base, output_root)
          # template root directory
          templates_root = template_base

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
          puts("create: #{project_dir}") if Application.logger.debug?

          FileUtils.mkdir_p(project_dir) unless Dir.exist?(project_dir)
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
          Dir.glob("#{template_root}/**/{*,.*}").each do |path|
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
