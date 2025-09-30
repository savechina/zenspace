use anyhow::Result;
use glob;
use heck::{ToLowerCamelCase, ToPascalCase};
use include_dir::{Dir, include_dir};
use std::{collections::HashMap, env, path::PathBuf, sync::LazyLock};
use tera::Value;
use which;

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

///delete file from glob match `path_pattern` files list
pub(crate) fn delete_pattern(path_pattern: &str) {
    // Collect all matching paths.
    let mut file_list: Vec<PathBuf> = Vec::new();

    // use path pattern glob find all matchs files
    for entry in glob::glob(path_pattern).unwrap() {
        if let Ok(path) = entry {
            file_list.push(path);
        }
    }

    // Use fs_extra to remove the collected items.
    if !file_list.is_empty() {
        println!("Deleting the following files: {:?}", file_list);
        fs_extra::remove_items(&file_list)
            .expect(format!("Deleting {} files failed", path_pattern).as_str());
    } else {
        println!("No {} files found to delete.", path_pattern);
    }
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
