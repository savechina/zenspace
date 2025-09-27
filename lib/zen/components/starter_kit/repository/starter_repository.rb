# typed:true
# frozen_string_literal: true

module Zen
  module Components
    require_relative "../model/java_class"
    require_relative "../model/java_field"
    require "active_support/all"

    class StarterKit
      module Repository
        class StarterRepository
          include Import["persistence.db"]

          JavaField = StarterKit::Model::JavaField

          def fetch_all_tables
            # db.create_table :posts do
            #   primary_key :id
            #   column :title, String
            #   String :content
            #   index :title
            # end

            # puts "#{db.tables}"
            db.tables
          end

          def fetch_table_schema(table_name)
            db.schema(table_name)
          end

          # ##
          # # fetch all class from database by table
          # # @return [Array[StarterKit::JavaClass]]
          # def fetch_all_class
          #   all_class = []
          #   all_table = db.tables

          #   all_table.each do |table_name|
          #     fields = fetch_all_fields(table_name)

          #     java_class = StarterKit::Model::JavaClass.new(
          #       package_name: "nil",
          #       table_name: table_name.to_s,
          #       class_name: table_name.to_s.camelcase,
          #       fields:
          #     )

          #     all_class << java_class
          #   end

          #   all_class
          # end

          # ##
          # # fetch all field from database by <table_name>
          # # @return [Array[StarterKit::JavaField]]
          # def fetch_all_fields(table_name)
          #   fields = []

          #   db.schema(table_name).each do |col, value|
          #     puts "#{col},#{value}"

          #     col_type = value[:db_type].to_s.downcase.start_with?("varchar") ? "VARCHAR" : value[:db_type]

          #     java_field = StarterKit::Model::JavaField.new(
          #       field_name: col.to_s.camelcase(:lower),
          #       field_type: value[:type].to_s,
          #       comment: "",
          #       column_name: col.to_s,
          #       column_type: col_type
          #     )

          #     fields << java_field
          #   end

          #   fields
          # end

          ##
          # fetch all class from database by table
          # @return [Array[StarterKit::JavaClass]]
          def fetch_all_class(table_name)
            all_class = []

            # puts "fetch_all_class:#{table_name}"

            if table_name.nil?

              all_table = db.tables

              all_table.each do |table_name|
                fields = fetch_all_fields(table_name)

                java_class = StarterKit::Model::JavaClass.new(package_name: "nil",
                                                              table_name: table_name.to_s,
                                                              class_name: table_name.to_s.camelcase,
                                                              fields:)

                all_class << java_class
              end
            else

              schema_name = ENV.fetch("database.schema", nil)

              db_type = ENV.fetch("database.type", "sqlite3")

              table_comment = nil

              # puts "#{db_type},#{"mysql".casecmp(db_type)}"

              if "mysql".casecmp(db_type) == 0

                sql = <<-SQL
                  SELECT
                    TABLE_NAME,TABLE_COMMENT
                  FROM information_schema.TABLES
                    WHERE
                      TABLE_SCHEMA = '#{schema_name}'
                    AND TABLE_NAME = '#{table_name}'
                SQL

                table_row = db.fetch(sql).first

                table_comment = table_row[:TABLE_COMMENT]
              end

              fields = fetch_all_fields(table_name)

              java_class = StarterKit::Model::JavaClass.new
              java_class.package_name = "nil"
              java_class.table_name = table_name.to_s
              java_class.class_name = table_name.to_s.camelcase
              java_class.fields = fields
              java_class.class_comment = table_comment

              all_class << java_class
            end

            all_class
          end

          ##
          # fetch all field from database by <table_name>
          # @return [Array[StarterKit::JavaField]]
          def fetch_all_fields(table_name)
            fields = []

            schema_name = ENV.fetch("database.schema", nil)

            db_type = ENV.fetch("database.type", "sqlite3")

            table_comment = nil
            is_mysql = false

            # puts "#{db_type},#{"mysql".casecmp(db_type)}"

            is_mysql = true if "mysql".casecmp(db_type) == 0

            if is_mysql
              sql = <<-SQL
              SELECT
                COLUMN_NAME, DATA_TYPE, COLUMN_COMMENT
              FROM information_schema.COLUMNS
              WHERE
                TABLE_SCHEMA = '#{schema_name}'
               AND TABLE_NAME = '#{table_name}'
              SQL

              # database: MySQL
              db.fetch(sql) do |row|
                column_type = row[:DATA_TYPE]

                column_name = row[:COLUMN_NAME]

                comment = row[:COLUMN_COMMENT]

                java_field = StarterKit::Model::JavaField.new

                java_field.field_name = column_name.to_s.camelcase(:lower)
                java_field.java_type = StarterKit::Model::JavaTypes.value(column_type.to_s.upcase.to_sym)[:java_type]
                java_field.field_comment = comment
                java_field.column_name = column_name.to_s
                java_field.column_type = column_type.to_s.upcase
                java_field.jdbc_type = StarterKit::Model::JavaTypes.value(column_type.to_s.upcase.to_sym)[:jdbc_type]

                fields << java_field
              end

            else
              # database: Sequel adapter
              db.schema(table_name).each do |col, value|
                column_type = value[:db_type].to_s.upcase
                column_type = "VARCHAR" if column_type.start_with?("VARCHAR")
                # puts "#{column_type},#{value}"

                java_field = StarterKit::Model::JavaField.new

                java_field.field_name = col.to_s.camelcase(:lower)
                java_field.java_type = StarterKit::Model::JavaTypes.value(column_type.to_s.upcase.to_sym)[:java_type]
                java_field.column_name = col.to_s
                java_field.field_comment = nil
                java_field.column_type = column_type
                java_field.jdbc_type = StarterKit::Model::JavaTypes.value(column_type.to_s.upcase.to_sym)[:jdbc_type]

                fields << java_field
              end

            end

            fields
          end
        end
      end
    end
  end
end
