use std::env;

use crate::{errors::ServiceError, model::starter_model::JavaClass};
use sqlx::Executor;
use sqlx::MySqlPool;
use sqlx::Row;
use sqlx::mysql::MySqlRow;
use sqlx::mysql::{MySqlColumn, MySqlDatabaseError};
use sqlx::{Acquire, Column};
use sqlx::{Any, ColumnIndex, Pool};
use sqlx::{Connection, pool};
use sqlx::{Database, MySql};

pub(crate) fn fetch_clazz(table_name: String) -> JavaClass {
    JavaClass::default()
}

pub(crate) async fn fetch_field(table_name: String) -> Result<JavaClass, ServiceError> {
    let url = env::var("database.url").expect("can't found database url");

    // 创建一个数据库连接
    let pool = MySqlPool::connect(url.as_str()).await.unwrap();
    let schema_name = "airp";
    let table_name = "hms_monitor_data";
    let sql = r#"
        SELECT
          TABLE_NAME,TABLE_COMMENT
        FROM information_schema.TABLES
          WHERE
            TABLE_SCHEMA = ? AND TABLE_NAME = ?
        "#;

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
        .bind(table_name)
        .fetch_all(&pool)
        .await
        .unwrap();

    // println!("db: rows {:?}", rows);

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
    }

    Ok(JavaClass::default())
}
