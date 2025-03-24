# typed: true
# frozen_string_literal: true

require "ruby_enum"

module Zen
  module Components
    class StarterKit
      module Model
        ##
        # Java Types mapping database type to java type
        class JavaTypes
          include Ruby::Enum

          define :VARCHAR, { db_type: "VARCHAR", java_type: "String", jdbc_type: "VARCHAR" }
          define :CHAR, { db_type: "CHAR", java_type: "String", jdbc_type: "CHAR" }
          define :BLOB, { db_type: "BLOB", java_type: "byte[]", jdbc_type: "BLOB" }
          define :TEXT, { db_type: "TEXT", java_type: "String", jdbc_type: "VARCHAR" }
          define :JSON, { db_type: "JSON", java_type: "String", jdbc_type: "VARCHAR" }

          # INT 映射到 JDBC 的 INTEGER
          define :INT, { db_type: "INT", java_type: "Integer", jdbc_type: "INTEGER" }
          define :INTEGER, { db_type: "INTEGER", java_type: "Long", jdbc_type: "INTEGER" }
          define :TINYINT, { db_type: "TINYINT", java_type: "Integer", jdbc_type: "TINYINT" }
          define :SMALLINT, { db_type: "SMALLINT", java_type: "Integer", jdbc_type: "SMALLINT" }
          define :BIGINT, { db_type: "BIGINT", java_type: "Long", jdbc_type: "BIGINT" }

          define :FLOAT, { db_type: "FLOAT", java_type: "Float", jdbc_type: "FLOAT" }
          define :DOUBLE, { db_type: "DOUBLE", java_type: "Double", jdbc_type: "DOUBLE" }
          define :DECIMAL, { db_type: "DECIMAL", java_type: "BigDecimal", jdbc_type: "DECIMAL" }
          define :BOOLEAN, { db_type: "BOOLEAN", java_type: "Boolean", jdbc_type: "BOOLEAN" }

          define :MEDIUMTEXT, { db_type: "MEDIUMTEXT", java_type: "String", jdbc_type: "VARCHAR" }
          define :MEDIUMINT, { db_type: "MEDIUMINT", java_type: "Integer", jdbc_type: "INTEGER" }
          define :BIT, { db_type: "BIT", java_type: "Boolean", jdbc_type: "BIT" }

          define :DATE, { db_type: "DATE", java_type: "Date", jdbc_type: "DATE" }
          define :TIME, { db_type: "TIME", java_type: "Date", jdbc_type: "TIME" }
          define :DATETIME, { db_type: "DATETIME", java_type: "Date", jdbc_type: "TIMESTAMP" }
          define :TIMESTAMP, { db_type: "TIMESTAMP", java_type: "Date", jdbc_type: "TIMESTAMP" }
          define :YEAR, { db_type: "YEAR", java_type: "Date", jdbc_type: "YEAR" }
        end
      end
    end
  end
end
