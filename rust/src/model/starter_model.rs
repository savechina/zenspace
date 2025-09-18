use serde::{Deserialize, Serialize};
use strum::{Display, EnumCount, EnumIter, EnumString};

#[derive(Debug, Clone, Default, Getters, Setters, Serialize, Deserialize)]
pub(crate) struct Project {
    ///Project Name
    pub(crate) project_name: String,
    ///Group Name
    pub(crate) group_name: String,
    /// project package name
    pub(crate) package_name: String,
    /// project arch type,value : ddd and mvc
    pub(crate) arch_type: String,
}

/// Represents a field in a Java object and its corresponding database column mapping.
#[derive(Debug, Clone, Default, Getters, Setters, Serialize, Deserialize)]
pub struct JavaField {
    /// The name of the field in the Java class.
    pub field_name: String,

    /// The comment or documentation for the field.
    pub field_comment: String,

    /// The Java type of the field (e.g., "String", "Integer").
    pub java_type: String,

    /// The name of the corresponding column in the database table.
    pub column_name: String,

    /// The name of the database table.
    pub table_name: String,

    /// The database type of the column (e.g., "VARCHAR", "INT").
    pub column_type: String,

    /// The JDBC type mapping for the column (e.g., "VARCHAR", "INTEGER").
    pub jdbc_type: String,
}

/// Represents a Java class and its database table mapping.
#[derive(Debug, Clone, Default, Getters, Setters, Serialize, Deserialize)]
pub struct JavaClass {
    /// The package name of the Java class (e.g., "com.example.model").
    pub package_name: String,

    /// The name of the class itself (e.g., "User").
    pub class_name: String,

    /// The comment or documentation for the class.
    pub class_comment: String,

    /// The name of the database table this class maps to.
    pub table_name: String,

    /// A list of fields belonging to this class.
    pub fields: Vec<JavaField>,

    /// A feature name or module name associated with the class.
    pub feature_name: String,
}

#[derive(Clone)]
pub struct JavaModule {
    // Constants
    // pub const SOURCES_ROOT: &'static str = "src/main/java";
    // pub const RESOURCES_ROOT: &'static str = "src/main/resources";
    // pub const SOURCE_TYPE: &'static str = "java";
    // pub const RESOURCE_TYPE: &'static str = "resource";

    // Fields (corresponding to Ruby attr_accessor)
    pub project: Option<Project>, // Using Option to handle uninitialized state
    pub module_name: Option<String>,
    pub module_package: Option<String>,
    pub module_path: Option<String>,
    pub module_template: Option<String>,
    pub module_type: Option<String>,
    pub module_suffix: Option<String>,
    pub module_prefix: Option<String>,
    pub module_output: Option<String>,
    pub module_model: Option<JavaClass>,
    full_path: Option<String>, // Internal field to cache full_path
}

/// JavaTypeMapping struct to hold the associated data for each Java type.
#[derive(Debug, Clone)]
pub struct JavaTypeMapping {
    /// database type
    pub db_type: &'static str,
    /// Java type
    pub java_type: &'static str,
    /// JDBC type
    pub jdbc_type: &'static str,
}

// The enum representing the different Java types.
// We derive `Debug` to allow easy printing of the enum variants.
#[derive(Debug, EnumString, EnumIter, EnumCount, Display)]
pub enum JavaTypes {
    Varchar,
    Char,
    Blob,
    Text,
    Json,
    Int,
    Integer,
    Tinyint,
    Smallint,
    Bigint,
    Float,
    Double,
    Decimal,
    Boolean,
    Mediumtext,
    Mediumint,
    Bit,
    Date,
    Time,
    Datetime,
    Timestamp,
    Year,
}

impl JavaTypes {
    // This method returns the associated info struct for each enum variant.
    pub fn info(&self) -> JavaTypeMapping {
        match self {
            JavaTypes::Varchar => JavaTypeMapping {
                db_type: "VARCHAR",
                java_type: "String",
                jdbc_type: "VARCHAR",
            },
            JavaTypes::Char => JavaTypeMapping {
                db_type: "CHAR",
                java_type: "String",
                jdbc_type: "CHAR",
            },
            JavaTypes::Blob => JavaTypeMapping {
                db_type: "BLOB",
                java_type: "byte[]",
                jdbc_type: "BLOB",
            },
            JavaTypes::Text => JavaTypeMapping {
                db_type: "TEXT",
                java_type: "String",
                jdbc_type: "VARCHAR",
            },
            JavaTypes::Json => JavaTypeMapping {
                db_type: "JSON",
                java_type: "String",
                jdbc_type: "VARCHAR",
            },
            JavaTypes::Int => JavaTypeMapping {
                db_type: "INT",
                java_type: "Integer",
                jdbc_type: "INTEGER",
            },
            JavaTypes::Integer => JavaTypeMapping {
                db_type: "INTEGER",
                java_type: "Long",
                jdbc_type: "INTEGER",
            },
            JavaTypes::Tinyint => JavaTypeMapping {
                db_type: "TINYINT",
                java_type: "Integer",
                jdbc_type: "TINYINT",
            },
            JavaTypes::Smallint => JavaTypeMapping {
                db_type: "SMALLINT",
                java_type: "Integer",
                jdbc_type: "SMALLINT",
            },
            JavaTypes::Bigint => JavaTypeMapping {
                db_type: "BIGINT",
                java_type: "Long",
                jdbc_type: "BIGINT",
            },
            JavaTypes::Float => JavaTypeMapping {
                db_type: "FLOAT",
                java_type: "Float",
                jdbc_type: "FLOAT",
            },
            JavaTypes::Double => JavaTypeMapping {
                db_type: "DOUBLE",
                java_type: "Double",
                jdbc_type: "DOUBLE",
            },
            JavaTypes::Decimal => JavaTypeMapping {
                db_type: "DECIMAL",
                java_type: "BigDecimal",
                jdbc_type: "DECIMAL",
            },
            JavaTypes::Boolean => JavaTypeMapping {
                db_type: "BOOLEAN",
                java_type: "Boolean",
                jdbc_type: "BOOLEAN",
            },
            JavaTypes::Mediumtext => JavaTypeMapping {
                db_type: "MEDIUMTEXT",
                java_type: "String",
                jdbc_type: "VARCHAR",
            },
            JavaTypes::Mediumint => JavaTypeMapping {
                db_type: "MEDIUMINT",
                java_type: "Integer",
                jdbc_type: "INTEGER",
            },
            JavaTypes::Bit => JavaTypeMapping {
                db_type: "BIT",
                java_type: "Boolean",
                jdbc_type: "BIT",
            },
            JavaTypes::Date => JavaTypeMapping {
                db_type: "DATE",
                java_type: "Date",
                jdbc_type: "DATE",
            },
            JavaTypes::Time => JavaTypeMapping {
                db_type: "TIME",
                java_type: "Date",
                jdbc_type: "TIME",
            },
            JavaTypes::Datetime => JavaTypeMapping {
                db_type: "DATETIME",
                java_type: "Date",
                jdbc_type: "TIMESTAMP",
            },
            JavaTypes::Timestamp => JavaTypeMapping {
                db_type: "TIMESTAMP",
                java_type: "Date",
                jdbc_type: "TIMESTAMP",
            },
            JavaTypes::Year => JavaTypeMapping {
                db_type: "YEAR",
                java_type: "Date",
                jdbc_type: "YEAR",
            },
        }
    }
}
