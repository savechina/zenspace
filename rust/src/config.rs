use std::io::Write;

use anyhow::Result;
use config::{Config, Environment, File, FileFormat};
use dotenvy;
use getset::Getters;
use include_dir::{Dir, include_dir};
use serde::{Serialize, de::DeserializeOwned};
use serde_derive::Deserialize;

use crate::errors::ZenError;

use crate::util;

/// Configs
static CONFIGS: Dir = include_dir!("config");

struct Configuration {}

#[derive(Debug, Deserialize, Getters, Clone)]
struct StarterConfig {
    #[getset(get = "pub", set = "pub")]
    tools: Vec<String>,
}

pub(crate) fn load() -> Result<Config, ZenError> {
    // Load the environment variables from a .env file
    dotenvy::dotenv().ok();

    let template_path = util::TEMPLATES.path();
    println!("template path: {:?}", template_path);

    // 遍历所有文件（包括子目录中的文件）
    for file in util::TEMPLATES.dirs() {
        println!("Recursive File path: {:?}", file.path());

        file.dirs().for_each(|f| {
            println!("Recursive File path: {:?}", f.path());
        });

        file.files().for_each(|f| {
            println!("Recursive File path: {:?}", f.path());
        });
    }

    let f = util::TEMPLATES.get_entry("starter/ddd_init").unwrap();

    // for entry in f {
    //     println!("f : {:?}", entry);
    // }

    let config_path = CONFIGS.path();
    println!("config path: {:?}", config_path);

    // 遍历所有文件（包括子目录中的文件）
    for file in CONFIGS.files() {
        println!("Recursive File path: {:?}", file.path());

        // file.files().for_each(|f| {
        //     println!("Recursive File path: {:?}", f.path());
        // });
    }

    //Get a config file "config.toml"
    let config_file = CONFIGS.get_file("config.toml").unwrap();

    let contents = config_file.contents_utf8().expect("UTF-8 content");

    // Build the configuration using the config crate
    // This allows you to load configuration from multiple sources such as files and environment variables
    // The configuration is built in a way that it can be easily extended and modified
    let mut builder = Config::builder()
        // Start off by merging in the "application" configuration file
        // .add_source(File::with_name("config/config"))
        .add_source(File::from_str(contents, FileFormat::Toml))
        // Add in the current environment file
        // Default to 'development' env
        // This allows you to have different configuration files for different environments
        // For example, `config/application-dev.toml` for development, `config/application-prod.toml` for production` to use the production configuration
        // .add_source(
        //     File::with_name(&format!("config/application-{profile_active}")).required(false),
        // )
        // Add in the environment variables
        // This allows you to override configuration values using environment variables
        // Environment variables can be set using the `APP_` prefix
        // For example, you can set `ZEN_DEBUG=1` to enable debug mode
        .add_source(Environment::with_prefix("zen"))
        // Set the default values for the configuration
        // This allows you to provide default values for the configuration fields
        // If a field is not set in the configuration file or environment variables, it will use
        // the default value specified here
        // .set_override("database.url", "sqlite://memory:")?
        .set_default("debug", false)?
        .set_default("database.pool_size", 10)?;

    //user config path .zen/config/config.toml
    let config_path = util::USER_ROOT.join("config/config.toml");

    if config_path.exists() {
        let base_name = config_path.to_str().unwrap();
        builder = builder.add_source(File::with_name(base_name));
    }

    // let mut temp_file = tempfile::Builder::new()
    //     .suffix(".toml")
    //     .tempfile()
    //     .map_err(|e| ZenError::Message(format!("Failed to create temp file: {}", e)))?;
    // temp_file
    //     .write_all(contents.as_bytes())
    //     .map_err(|e| ZenError::Message(format!("Failed to write temp file: {}", e)))?;

    // let path = temp_file.into_temp_path();
    // // Convert TempPath to PathBuf for config::File
    // let path_buf = path.to_path_buf();

    // println!("setting path : {:?}", path_buf);

    let setting = builder.build().map_err(|e| ZenError::Config(e)).unwrap();

    println!("setting : {:?}", setting);
    let tools = setting.get_array("starter.tool").unwrap();

    println!("tools : {:?}", tools);
    Ok(setting)
}
