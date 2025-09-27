use std::str::FromStr;
use std::sync::LazyLock;
use std::{env, vec};

use crate::model::starter_model::{JavaField, JavaTypes};
use crate::{errors::ServiceError, model::starter_model::JavaClass};
use heck::{ToLowerCamelCase, ToPascalCase};
use sqlx::MySqlPool;
use sqlx::Row;

///User Root Directory
pub(crate) static POOL: LazyLock<MySqlPool> = LazyLock::new(|| -> MySqlPool {
    // let table_name = "hms_monitor_data";
    let url = env::var("database.url").expect("can't found database url");

    // 创建一个数据库连接
    let pool = MySqlPool::connect_lazy(url.as_str()).unwrap();

    pool
});

pub(crate) async fn fetch_clazz(table_name: Option<String>) -> Vec<JavaClass> {
    //database from env
    let schema_name = env::var("database.schema").unwrap_or("schema_name".to_string());

    let pool = POOL.clone();
    let sql = r#"
        SELECT
          TABLE_NAME,TABLE_COMMENT
        FROM information_schema.TABLES
          WHERE
            TABLE_SCHEMA = ? AND TABLE_NAME = ?
        "#;

    // 查询数据
    let rows = sqlx::query(sql)
        .bind(schema_name)
        .bind(table_name.unwrap())
        .fetch_all(&pool)
        .await
        .unwrap();

    //JavaClass List
    let mut clazz_list = vec![];

    for row in rows {
        let table_name: String = row.get("TABLE_NAME");
        let table_comment: String = row.get("TABLE_COMMENT");

        let field_list = fetch_field(table_name.to_ascii_uppercase()).await.unwrap();

        // dbg!(
        //     "TABLE_NAME: {},  TABLE_COMMENT: {}, Fields: {:?}",
        //     table_name.clone(),
        //     table_comment.clone(),
        //     field_list.clone()
        // );

        let mut clazz = JavaClass::default();
        clazz.table_name = table_name.clone();
        clazz.class_name = table_name.to_pascal_case();
        clazz.class_comment = table_comment;
        clazz.fields = field_list;

        clazz_list.push(clazz);
    }

    //return clazz list. it have fields
    clazz_list
}

pub(crate) async fn fetch_field(table_name: String) -> Result<Vec<JavaField>, ServiceError> {
    //database from env
    let schema_name = env::var("database.schema").unwrap_or("schema_name".to_string());

    // 创建一个数据库连接
    let pool = POOL.clone();

    let sql = r#"
        SELECT
            COLUMN_NAME,
            DATA_TYPE,
            COLUMN_COMMENT
        FROM
            information_schema.COLUMNS
        WHERE
            TABLE_SCHEMA = ? AND TABLE_NAME = ?
        "#;
    // 查询数据
    let rows = sqlx::query(sql)
        .bind(schema_name)
        .bind(table_name.clone())
        .fetch_all(&pool)
        .await
        .unwrap();

    //Java field list  Vec<JavaField>
    let mut field_list = vec![];

    for row in rows {
        let column_name: String = row.get("COLUMN_NAME");
        let data_type: String = row.get("DATA_TYPE");
        let column_comment: String = row.get("COLUMN_COMMENT");
        println!(
            "COLUMN_NAME: {}, DATA_TYPE: {}, COLUMN_COMMENT: {}",
            column_name,
            data_type.to_uppercase(),
            column_comment
        );

        //get JavaTypeMapping from table column type
        let java_type_mapping = JavaTypes::from_str(&data_type.clone())
            .expect("not found mapping column type for support")
            .info();

        //Build per column for JavaField
        let field = JavaField::builder()
            .field_name(column_name.to_lower_camel_case())
            .column_name(column_name.clone())
            .column_type(data_type.to_uppercase())
            .field_comment(column_comment)
            .table_name(table_name.clone())
            .java_type(java_type_mapping.java_type.to_string())
            .jdbc_type(java_type_mapping.jdbc_type.to_string())
            .build();

        // println!("field: {:?}", field.clone());
        // dbg!(field.clone());

        field_list.push(field);
    }

    Ok(field_list)
}
