use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
};

use home::home_dir;
use tracing::warn;

use zen_core::errors::ServiceError;

use crate::utils;

pub fn clean_all() -> Result<(), ServiceError> {
    clean_trash()?;
    clean_cache()?;
    clean_logs()?;
    clean_ide()?;
    Ok(())
}

pub fn clean_trash() -> Result<(), ServiceError> {
    println!("Clean Trash ...");

    let home = home_dir()
        .ok_or_else(|| ServiceError::Message("Could not determine home directory".to_string()))?;
    let trash_path = home.join(".Trash/*");

    println!("path:{}", trash_path.display());

    let osascript_command = r#"
           try
               tell application "Finder" to empty trash
           on error number -128
               -- 垃圾桶已空，忽略此錯誤
           end try
       "#;

    if !utils::command_exists("osascript") {
        println!("osascript command is not available.please check your macos version.");
        return Ok(());
    }

    let status = Command::new("osascript")
        .arg("-e")
        .arg(osascript_command)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| {
            ServiceError::Message(format!("Failed to execute osascript command: {}", e))
        })?;

    if status.success() {
        println!("Trash cleanup successful.");
        Ok(())
    } else {
        Err(ServiceError::Message(
            "Trash cleanup failed (exit code != 0)".to_string(),
        ))
    }
}

pub fn clean_cache() -> Result<(), ServiceError> {
    println!("Clean Cache ...");

    let home = home_dir()
        .ok_or_else(|| ServiceError::Message("Could not determine home directory".to_string()))?;

    let run_cleanup_cmd = |cmd_name: &str, args: &[&str]| -> Result<bool, String> {
        if !utils::command_exists(cmd_name) {
            return Ok(false);
        }

        println!("Cleaning {} cache", cmd_name);
        match Command::new(cmd_name)
            .args(args)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
        {
            Ok(status) => {
                if status.success() {
                    println!("{} cache cleanup successful.", cmd_name);
                    Ok(true)
                } else {
                    eprintln!(
                        "{} cache cleanup failed with exit code: {:?}",
                        cmd_name,
                        status.code()
                    );
                    Err(format!(
                        "{} exited with {}",
                        cmd_name,
                        status.code().unwrap_or(-1)
                    ))
                }
            }
            Err(e) => {
                eprintln!("Failed to execute {}: {}", cmd_name, e);
                Err(format!("Failed to execute {}: {}", cmd_name, e))
            }
        }
    };

    if let Err(e) = run_cleanup_cmd("gem", &["cleanup"]) {
        tracing::debug!(error = %e, "gem cleanup skipped");
    }
    if let Err(e) = run_cleanup_cmd("brew", &["cleanup"]) {
        tracing::debug!(error = %e, "brew cleanup skipped");
    }
    if let Err(e) = run_cleanup_cmd(
        "go",
        &["clean", "-cache", "-modcache", "-testcache", "-fuzzcache"],
    ) {
        tracing::debug!(error = %e, "go cleanup skipped");
    }
    if let Err(e) = run_cleanup_cmd("poetry", &["cache", "clear", "--all", "pypi"]) {
        tracing::debug!(error = %e, "poetry cache clear skipped");
    }
    if let Err(e) = run_cleanup_cmd("uv", &["cache", "prune"]) {
        tracing::debug!(error = %e, "uv cache prune skipped");
    }
    if let Err(e) = run_cleanup_cmd("pip", &["cache", "purge"]) {
        tracing::debug!(error = %e, "pip cache purge skipped");
    }

    let cargo_cache_path = home.join(".cargo/registry");
    if cargo_cache_path.exists() {
        println!("Cleaning Cargo cache");
        if let Ok(pattern) = cargo_cache_path.to_str().ok_or("Invalid Unicode in path")
            && let Err(e) = std::panic::catch_unwind(|| utils::delete_pattern(pattern))
        {
            warn!(pattern = %pattern, error = ?e, "panic during Cargo cache cleanup");
        }
    }

    let edge_cache_path = home.join("Library/Caches/Microsoft Edge/Default/Cache");
    if edge_cache_path.exists() {
        println!("Cleaning Microsoft Edge cache");
        if let Ok(pattern) = edge_cache_path.to_str().ok_or("Invalid Unicode in path")
            && let Err(e) = std::panic::catch_unwind(|| utils::delete_pattern(pattern))
        {
            warn!(pattern = %pattern, error = ?e, "panic during Edge cache cleanup");
        }
    }

    let edge_code_cache_path = home.join("Library/Caches/Microsoft Edge/Default/Code Cache");
    if edge_code_cache_path.exists() {
        println!("Cleaning Microsoft Edge Code cache");
        if let Ok(pattern) = edge_code_cache_path
            .to_str()
            .ok_or("Invalid Unicode in path")
            && let Err(e) = std::panic::catch_unwind(|| utils::delete_pattern(pattern))
        {
            warn!(pattern = %pattern, error = ?e, "panic during Edge code cache cleanup");
        }
    }

    let chrome_cache_path = home.join("Library/Caches/Google/Chrome/Default/Cache");
    if chrome_cache_path.exists() {
        println!("Cleaning Google Chrome cache");
        if let Ok(pattern) = chrome_cache_path.to_str().ok_or("Invalid Unicode in path")
            && let Err(e) = std::panic::catch_unwind(|| utils::delete_pattern(pattern))
        {
            warn!(pattern = %pattern, error = ?e, "panic during Chrome cache cleanup");
        }
    }

    let chrome_code_cache_path = home.join("Library/Caches/Google/Chrome/Default/Code Cache");
    if chrome_code_cache_path.exists() {
        println!("Cleaning Google Chrome Code cache");
        if let Ok(pattern) = chrome_code_cache_path
            .to_str()
            .ok_or("Invalid Unicode in path")
            && let Err(e) = std::panic::catch_unwind(|| utils::delete_pattern(pattern))
        {
            warn!(pattern = %pattern, error = ?e, "panic during Chrome code cache cleanup");
        }
    }

    Ok(())
}

pub fn clean_logs() -> Result<(), ServiceError> {
    println!("Clean Logs ...");
    let home = home_dir()
        .ok_or_else(|| ServiceError::Message("Could not determine home directory".to_string()))?;

    let delete_if_exists = |path: PathBuf, description: &str| {
        if path.exists() {
            println!("Deleting {}", description);
            if let Ok(pattern) = path.to_str().ok_or("Invalid Unicode in path")
                && let Err(e) = std::panic::catch_unwind(|| utils::delete_pattern(pattern))
            {
                warn!(pattern = %pattern, error = ?e, "panic during log cleanup");
            }
        }
    };

    delete_if_exists(home.join("*.hprof"), "Java heap dumps");
    delete_if_exists(
        home.join("Library/Logs/JetBrains/*/"),
        "application log files from JetBrains",
    );
    delete_if_exists(
        home.join("Library/Logs/Notion/*"),
        "application log files from Notion",
    );
    delete_if_exists(
        home.join("Library/Logs/Zed/*"),
        "application log files from Zed",
    );
    delete_if_exists(
        home.join("Library/Logs/Arduino IDE/*"),
        "application log files from Arduino IDE",
    );
    delete_if_exists(
        home.join("Library/Logs/DiagnosticReports/*"),
        "application log files from DiagnosticReports",
    );
    delete_if_exists(
        home.join("Library/Logs/iPhone Updater Logs/*"),
        "application log files from iPhone Updater Logs",
    );
    delete_if_exists(
        PathBuf::from("/var/logs/*.log*"),
        "application log files from /var/logs/*.log Logs",
    );

    println!("Logs cleanup successful.");
    Ok(())
}

pub fn clean_ide() -> Result<(), ServiceError> {
    println!("Clean IDE project file ...");

    println!("Clearing all files from IDE:");
    let current_dir = env::current_dir()
        .map_err(|e| ServiceError::Message(format!("Failed to get current directory: {}", e)))?;

    let delete_if_exists = |path: PathBuf, description: &str| {
        if path.exists() {
            println!("Deleting {}", description);
            if let Ok(pattern) = path.to_str().ok_or("Invalid Unicode in path")
                && let Err(e) = std::panic::catch_unwind(|| utils::delete_pattern(pattern))
            {
                warn!(pattern = %pattern, error = ?e, "panic during IDE cleanup");
            }
        }
    };

    delete_if_exists(current_dir.join(".idea"), "IDEA config files");
    delete_if_exists(current_dir.join("**/.settings"), "Eclipse settings files");
    delete_if_exists(
        current_dir.join("**/.flattened-pom.xml"),
        "Maven flattened POM files",
    );
    delete_if_exists(current_dir.join("**/.project"), "Eclipse project files");
    delete_if_exists(current_dir.join("**/.factorypath"), "Factory path files");
    delete_if_exists(current_dir.join("**/.classpath"), "Classpath files");

    println!("IDE project file cleanup successful.");
    Ok(())
}
