# typed: true
# frozen_string_literal: true

module Zen
  module Components
    require "ruby_enum"

    class StarterKit
      module Model
        ##
        # Java Types mapping database type to java type
        class JavaTypes
          include Ruby::Enum

          define :VARCHAR, { db_type: "VARCHAR", java_type: "String", jdbc_type: "VARCHAR" }
        end
      end
    end
  end
end
