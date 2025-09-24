use anyhow::Result;
use heck::{ToLowerCamelCase, ToPascalCase};
use std::{collections::HashMap, env, path::PathBuf, sync::LazyLock};
use tera::Value;
use which;

use include_dir::{Dir, include_dir};

/// Assets
pub(crate) static ASSETS: Dir = include_dir!("assets");

/// Template
pub(crate) static TEMPLATES: Dir = include_dir!("templates");

///User Root Directory
pub(crate) static USER_ROOT: LazyLock<PathBuf> = LazyLock::new(|| -> PathBuf {
    home::home_dir()
        .map(|home| PathBuf::from(home).join(".zen"))
        .unwrap()
});

/// which `command` exists
pub(crate) fn command_exists(command: &str) -> bool {
    let result = which::which(command);

    let exists = match result {
        Ok(path) => {
            println!("path:{}", path.display());
            true
        }
        Err(_) => false,
    };

    exists
}

/// 定义to_pascal_case过滤器函数
pub(crate) fn to_pascal_case_filter(
    value: &Value,
    _: &HashMap<String, Value>,
) -> Result<Value, tera::Error> {
    if let Some(s) = value.as_str() {
        let pascal_case = s.to_pascal_case();
        Ok(tera::to_value(pascal_case).unwrap())
    } else {
        Err(tera::Error::msg(
            "`to_pascal_case` 过滤器只能用于字符串".to_string(),
        ))
    }
}

/// 定义to_lower_camel_case过滤器函数
pub(crate) fn to_lower_camel_case_filter(
    value: &Value,
    _: &HashMap<String, Value>,
) -> Result<Value, tera::Error> {
    if let Some(s) = value.as_str() {
        let pascal_case = s.to_lower_camel_case();
        Ok(tera::to_value(pascal_case).unwrap())
    } else {
        Err(tera::Error::msg(
            "`to_lower_camel_case` 过滤器只能用于字符串".to_string(),
        ))
    }
}
