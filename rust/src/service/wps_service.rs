use crate::errors::ServiceError;
use crate::util;
use chrono::Date;
use chrono::DateTime;
use chrono::Local;

use std::fs;
use std::fs::File;
use std::io::Error;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

pub(crate) fn zstds(from_dir: PathBuf, output_file: PathBuf) -> Result<(), ServiceError> {
    //check command exists
    if !util::command_exists("zstd") {
        println!("zstd command is not available. use `brew install zstd` to install zstd.")
    }

    //check file exists
    if !from_dir.exists() {
        return Err(ServiceError::Io(Error::from(ErrorKind::NotFound)));
    }

    let from_path = from_dir.parent().expect("file parent path not found");
    let from_file = from_dir.file_name().expect("file name not dound");
    let to_path = output_file;

    println!(
        "tar --zstd -cf {} -C {} {}",
        to_path.display(),
        from_path.display(),
        from_file.to_string_lossy()
    );

    let status = Command::new("tar")
        .arg("--zstd")
        .arg("-cf")
        .arg(to_path)
        .arg("-C")
        .arg(from_path)
        .arg(from_file)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .expect("failed to execute tar zstd file");

    if !status.success() {
        return Err(ServiceError::Message(String::from(
            "tar execute to zstd file,status is failed",
        )));
    }

    Ok(())
}

pub(crate) fn archive(
    from_dir: Option<String>,
    output_file: Option<String>,
) -> Result<(), ServiceError> {
    //user home path
    let home = home::home_dir().expect("failed found HOME");

    // user documents path
    let documents = home.join("Documents");

    let file_list = match from_dir {
        None => {
            vec![
                documents.join("Work"),
                documents.join("Other"),
                documents.join("Personal"),
                documents.join("Book"),
            ]
        }
        Some(dir) => vec![PathBuf::from(dir)],
    };

    let archive_path = documents.join("Archive");

    for file in file_list {
        let file_name = file.file_name().unwrap();

        //file not exists ignore
        if !file.exists() {
            eprintln!("zstd compress directory: {} , not found", file.display());

            continue;
        }

        // 获取当前本地时间
        let now_date = Local::now().format("%Y%m%d");

        //archive file name
        let archive_name = format!("{}-{}.tar.zst", file_name.to_string_lossy(), now_date);

        //archive full path
        let archive_file = archive_path.join(archive_name);

        println!("archive {}  to {}", file.display(), archive_file.display());

        //zstd to compress file
        zstds(file, archive_file)?;
    }
    Ok(())
}

pub(crate) fn unixtime(timestamp: Option<i64>, timeunit: String) -> Result<(), ServiceError> {
    let now = Local::now();
    println!("now: {}", now.to_rfc3339());
    println!("local: {}", now);
    Ok(())
}
