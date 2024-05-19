# typed: true
# frozen_string_literal: true

module Zen
  module Components
    class StarterKit
      module Model
        ##
        # Java Project  Module
        class JavaModule
          # module type java ,java source file root path
          # @return [String]
          SOURCES_ROOT = "src/main/java"

          # Module Type : RESOURCE ,resource file root path
          # @return [String]
          RESOURCES_ROOT = "src/main/resources"

          # module source type
          SOURCE_TYPE = "java"

          # module resource type
          RESOURCE_TYPE = "resource"

          # Project
          # @return [Model::JavaProject]
          attr_accessor :project
          # Module Name
          # @return [String]
          attr_accessor :module_name

          # Module Pacakge
          #  releation to Peoject Base package
          # @return [String]
          attr_accessor :module_package

          # Module Root Path
          attr_accessor :module_path

          # Module template name
          # @return [String]
          attr_accessor :module_template

          # Module type
          # @return [String]  type: java , resource
          # @see [SOURCE_TYPE,RESOURCE_TYPE]
          attr_accessor :module_type

          # Module Spec Suffix
          #
          # @return [String]  Mapper | Entity | Repository | Service | Impl |ServiceProvider
          attr_accessor :module_suffix

          # Module Spec Prefix
          #
          # @return [String]  Wms | Scm
          attr_accessor :module_prefix

          # Module Template Output
          #
          # @return [String]
          attr_accessor :module_output

          # binding
          # @return [Binding]
          def get_binding
            binding
          end

          #
          # module full package path
          # @return[String]
          def full_package
            # full package
            full_package = nil

            case module_type
            when SOURCE_TYPE
              full_package = "#{@project.package_name}.#{module_package}"
            when RESOURCE_TYPE
              # resource package is resource directory
              full_package = module_package
            end
            full_package
          end

          # module file root path
          # @return [String]
          def root_path
            if module_type.equal? SOURCE_TYPE
              @full_path = File.join(module_path.to_s, SOURCES_ROOT)
            elsif module_type.equal? RESOURCE_TYPE
              @full_path = File.join(module_path.to_s, RESOURCES_ROOT)
            end
          end

          # module file full path
          # @return [String]
          def full_path
            if module_type.equal? SOURCE_TYPE
              @full_path = File.join(module_path.to_s, SOURCES_ROOT, full_package.tr(".", "/"))
            elsif module_type.equal? RESOURCE_TYPE
              @full_path = File.join(module_path.to_s, RESOURCES_ROOT, full_package.tr(".", "/"))
            end
          end

          def model(clazz_model)
            @module_model = clazz_model.dup
            @module_model.class_name = "#{clazz_model.class_name}#{module_suffix}"
          end
        end
      end
    end
  end
end
