module Zen
  module Components
    require_relative "../model/java_class"
    require_relative "../model/java_field"
    require "active_support/all"

    class StarterKit
      module Repository
        class StarterRepository
          include Import["persistence.db"]

          def fetch_all_tables
            db.create_table :posts do
              primary_key :id
              column :title, String
              String :content
              index :title
            end

            puts "#{db.tables}"
            db.tables
          end

          def fetch_table_schema(table_name)
            db.schema(table_name)
          end

          def fetch_all_class
            all_class = []
            all_table = db.tables
            all_table.each do |table_name|
              fields = []

              db.schema(table_name).each do |col, value|
                puts "#{col},#{value[:db_type]}"

                java_field = StarterKit::JavaField.new(field_name: col.to_s.camelcase(:lower),
                                                       field_type: value[:type].to_s,
                                                       comment: "",
                                                       column_name: col.to_s,
                                                       column_type: value[:db_type])
                fields << java_field
              end

              java_class = StarterKit::JavaClass.new(package_name: "nil",
                                                     table_name: table_name.to_s,
                                                     class_name: table_name.to_s.camelcase,
                                                     fields:)

              all_class << java_class
            end

            all_class
          end
        end
      end
    end
  end
end
