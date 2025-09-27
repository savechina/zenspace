use crate::errors::ServiceError;
use crate::util;
use chrono::Date;
use chrono::DateTime;
use chrono::Local;
use chrono::TimeZone;
use chrono::Utc;

use std::fs;
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

pub(crate) fn dotfiles(restore: bool) -> Result<(), ServiceError> {
    //user home path
    let home = home::home_dir().expect("failed found HOME");

    // user documents path
    let documents = home.join("Documents");
    let archive_path = documents.join("Archive");

    let dotpath = "dotfiles3";

    let file_list = vec![
        ".zshrc",
        ".zshenv",
        ".zprofile",
        ".profile",
        ".gitconfig",
        ".ssh",
        ".m2/setting.xml",
        ".rbenv/version",
        ".pyenv/version",
        ".vimrc",
        ".vim",
        ".config/gem",
        ".config/gh",
        ".config/pip",
    ];

    if restore {
        println!("restore:");
    } else {
        println!("backup:");

        for file in file_list {
            let from_path = home.join(file);
            let to_path = archive_path.join(dotpath).join(file);
            let to_dir = to_path.parent().expect("not found parent direcotry");

            println!(
                "from:{},to:{},is_file:{},{}",
                from_path.display(),
                to_path.display(),
                to_path.is_file(),
                to_dir.display()
            );

            if !to_dir.exists() {
                println!("create:{}", to_dir.display());
                fs::create_dir_all(to_dir).expect("failed to create dir");
            }

            if from_path.exists() {
                if from_path.is_file() {
                    fs::copy(from_path, to_path).unwrap();
                }

                // todo: copy directory
            }
        }
    }
    Ok(())
}

pub(crate) fn unixtime(timestamp: Option<i64>, timeunit: String) -> Result<(), ServiceError> {
    //parse datatime from timestamp and timeunit
    let now = if let Some(t) = timestamp {
        match timeunit.to_ascii_lowercase().as_str() {
            //second
            "s" => Local.timestamp_opt(t, 0).unwrap(),
            //millis second
            "ms" => Local.timestamp_millis_opt(t).unwrap(),
            //micros second
            "us" => Local.timestamp_micros(t).unwrap(),
            //nanos second
            "ns" => Local.timestamp_nanos(t),
            //second default
            _ => Local.timestamp_opt(t, 0).unwrap(), // default case
        }
    } else {
        //default now datetime
        Local::now()
    };

    // let now = Local::now();
    println!("now: {}", now.to_rfc3339());
    println!("local: {}", now);
    println!("timestamp: {}", now.timestamp());
    println!("timestamp millis: {}", now.timestamp_millis());
    println!("timestamp micros: {}", now.timestamp_micros());
    println!("timestamp nanos: {}", now.timestamp_nanos());

    let now = now.to_utc();
    println!("UTC: {}", now.to_rfc3339());
    println!("UTC: {}", now);
    Ok(())
}
