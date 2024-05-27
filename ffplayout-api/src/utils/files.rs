use std::{
    io::Write,
    path::{Path, PathBuf},
};

use actix_multipart::Multipart;
use actix_web::{web, HttpResponse};
use futures_util::TryStreamExt as _;
use lazy_static::lazy_static;
use lexical_sort::{natural_lexical_cmp, PathSort};
use rand::{distributions::Alphanumeric, Rng};
use relative_path::RelativePath;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Sqlite};
use tokio::fs;

use simplelog::*;

use crate::utils::{errors::ServiceError, playout_config};
use ffplayout_lib::utils::{file_extension, MediaProbe};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PathObject {
    pub source: String,
    parent: Option<String>,
    parent_folders: Option<Vec<String>>,
    folders: Option<Vec<String>>,
    files: Option<Vec<VideoFile>>,
    #[serde(default)]
    pub folders_only: bool,
}

impl PathObject {
    fn new(source: String, parent: Option<String>) -> Self {
        Self {
            source,
            parent,
            parent_folders: Some(vec![]),
            folders: Some(vec![]),
            files: Some(vec![]),
            folders_only: false,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MoveObject {
    source: String,
    target: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VideoFile {
    name: String,
    duration: f64,
}

lazy_static! {
    pub static ref HOME_DIR: String = home::home_dir()
        .unwrap_or("/home/h1wl3n2og".into()) // any random not existing folder
        .as_os_str()
        .to_string_lossy()
        .to_string();
}

const FOLDER_WHITELIST: &[&str; 6] = &[
    "/media",
    "/mnt",
    "/playlists",
    "/tv-media",
    "/usr/share/ffplayout",
    "/var/lib/ffplayout",
];

/// Normalize absolut path
///
/// This function takes care, that it is not possible to break out from root_path.
pub fn norm_abs_path(
    root_path: &Path,
    input_path: &str,
) -> Result<(PathBuf, String, String), ServiceError> {
    let path_relative = RelativePath::new(&root_path.to_string_lossy())
        .normalize()
        .to_string()
        .replace("../", "");
    let mut source_relative = RelativePath::new(input_path)
        .normalize()
        .to_string()
        .replace("../", "");
    let path_suffix = root_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    if input_path.starts_with(&*root_path.to_string_lossy())
        || source_relative.starts_with(&path_relative)
    {
        source_relative = source_relative
            .strip_prefix(&path_relative)
            .and_then(|s| s.strip_prefix('/'))
            .unwrap_or_default()
            .to_string();
    } else {
        source_relative = source_relative
            .strip_prefix(&path_suffix)
            .and_then(|s| s.strip_prefix('/'))
            .unwrap_or(&source_relative)
            .to_string();
    }

    let path = &root_path.join(&source_relative);

    if !FOLDER_WHITELIST.iter().any(|f| path.starts_with(f))
        && !path.starts_with(&HOME_DIR.to_string())
    {
        return Err(ServiceError::Forbidden(
            "Access forbidden: Folder cannot be opened.".to_string(),
        ));
    }

    Ok((path.to_path_buf(), path_suffix, source_relative))
}

/// File Browser
///
/// Take input path and give file and folder list from it back.
/// Input should be a relative path segment, but when it is a absolut path, the norm_abs_path function
/// will take care, that user can not break out from given storage path in config.
pub async fn browser(
    conn: &Pool<Sqlite>,
    id: i32,
    path_obj: &PathObject,
) -> Result<PathObject, ServiceError> {
    let (config, channel) = playout_config(conn, &id).await?;
    let mut channel_extensions = channel
        .extra_extensions
        .split(',')
        .map(|e| e.to_string())
        .collect::<Vec<String>>();
    let mut parent_folders = vec![];
    let mut extensions = config.storage.extensions;
    extensions.append(&mut channel_extensions);

    let (path, parent, path_component) = norm_abs_path(&config.storage.path, &path_obj.source)?;

    let parent_path = if !path_component.is_empty() {
        path.parent().unwrap()
    } else {
        &config.storage.path
    };

    let mut obj = PathObject::new(path_component, Some(parent));
    obj.folders_only = path_obj.folders_only;

    if path != parent_path && !path_obj.folders_only {
        let mut parents = fs::read_dir(&parent_path).await?;

        while let Some(child) = parents.next_entry().await? {
            if child.metadata().await?.is_dir() {
                parent_folders.push(
                    child
                        .path()
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }

        parent_folders.path_sort(natural_lexical_cmp);

        obj.parent_folders = Some(parent_folders);
    }

    let mut paths_obj = fs::read_dir(path).await?;

    let mut files = vec![];
    let mut folders = vec![];

    while let Some(child) = paths_obj.next_entry().await? {
        let f_meta = child.metadata().await?;

        // ignore hidden files/folders on unix
        if child.path().to_string_lossy().to_string().contains("/.") {
            continue;
        }

        if f_meta.is_dir() {
            folders.push(
                child
                    .path()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            );
        } else if f_meta.is_file() && !path_obj.folders_only {
            if let Some(ext) = file_extension(&child.path()) {
                if extensions.contains(&ext.to_string().to_lowercase()) {
                    files.push(child.path())
                }
            }
        }
    }

    folders.path_sort(natural_lexical_cmp);
    files.path_sort(natural_lexical_cmp);
    let mut media_files = vec![];

    for file in files {
        match MediaProbe::new(file.to_string_lossy().as_ref()) {
            Ok(probe) => {
                let mut duration = 0.0;

                if let Some(dur) = probe.format.duration {
                    duration = dur.parse().unwrap_or_default()
                }

                let video = VideoFile {
                    name: file.file_name().unwrap().to_string_lossy().to_string(),
                    duration,
                };
                media_files.push(video);
            }
            Err(e) => error!("{e:?}"),
        };
    }

    obj.folders = Some(folders);
    obj.files = Some(media_files);

    Ok(obj)
}

pub async fn create_directory(
    conn: &Pool<Sqlite>,
    id: i32,
    path_obj: &PathObject,
) -> Result<HttpResponse, ServiceError> {
    let (config, _) = playout_config(conn, &id).await?;
    let (path, _, _) = norm_abs_path(&config.storage.path, &path_obj.source)?;

    if let Err(e) = fs::create_dir_all(&path).await {
        return Err(ServiceError::BadRequest(e.to_string()));
    }

    info!(
        "create folder: <b><magenta>{}</></b>",
        path.to_string_lossy()
    );

    Ok(HttpResponse::Ok().into())
}

async fn copy_and_delete(source: &PathBuf, target: &PathBuf) -> Result<MoveObject, ServiceError> {
    match fs::copy(&source, &target).await {
        Ok(_) => {
            if let Err(e) = fs::remove_file(source).await {
                error!("{e}");
                return Err(ServiceError::BadRequest(
                    "Removing File not possible!".into(),
                ));
            };

            return Ok(MoveObject {
                source: source
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                target: target
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            });
        }
        Err(e) => {
            error!("{e}");
            Err(ServiceError::BadRequest("Error in file copy!".into()))
        }
    }
}

async fn rename(source: &PathBuf, target: &PathBuf) -> Result<MoveObject, ServiceError> {
    match fs::rename(source, target).await {
        Ok(_) => Ok(MoveObject {
            source: source
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            target: target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        }),
        Err(e) => {
            error!("{e}");
            copy_and_delete(source, target).await
        }
    }
}

pub async fn rename_file(
    conn: &Pool<Sqlite>,
    id: i32,
    move_object: &MoveObject,
) -> Result<MoveObject, ServiceError> {
    let (config, _) = playout_config(conn, &id).await?;
    let (source_path, _, _) = norm_abs_path(&config.storage.path, &move_object.source)?;
    let (mut target_path, _, _) = norm_abs_path(&config.storage.path, &move_object.target)?;

    if !source_path.exists() {
        return Err(ServiceError::BadRequest("Source file not exist!".into()));
    }

    if (source_path.is_dir() || source_path.is_file()) && source_path.parent() == Some(&target_path)
    {
        return rename(&source_path, &target_path).await;
    }

    if target_path.is_dir() {
        target_path = target_path.join(source_path.file_name().unwrap());
    }

    if target_path.is_file() {
        return Err(ServiceError::BadRequest(
            "Target file already exists!".into(),
        ));
    }

    if source_path.is_file() && target_path.parent().is_some() {
        return rename(&source_path, &target_path).await;
    }

    Err(ServiceError::InternalServerError)
}

pub async fn remove_file_or_folder(
    conn: &Pool<Sqlite>,
    id: i32,
    source_path: &str,
) -> Result<(), ServiceError> {
    let (config, _) = playout_config(conn, &id).await?;
    let (source, _, _) = norm_abs_path(&config.storage.path, source_path)?;

    if !source.exists() {
        return Err(ServiceError::BadRequest("Source does not exists!".into()));
    }

    if source.is_dir() {
        match fs::remove_dir(source).await {
            Ok(_) => return Ok(()),
            Err(e) => {
                error!("{e}");
                return Err(ServiceError::BadRequest(
                    "Delete folder failed! (Folder must be empty)".into(),
                ));
            }
        };
    }

    if source.is_file() {
        match fs::remove_file(source).await {
            Ok(_) => return Ok(()),
            Err(e) => {
                error!("{e}");
                return Err(ServiceError::BadRequest("Delete file failed!".into()));
            }
        };
    }

    Err(ServiceError::InternalServerError)
}

async fn valid_path(conn: &Pool<Sqlite>, id: i32, path: &str) -> Result<PathBuf, ServiceError> {
    let (config, _) = playout_config(conn, &id).await?;
    let (test_path, _, _) = norm_abs_path(&config.storage.path, path)?;

    if !test_path.is_dir() {
        return Err(ServiceError::BadRequest("Target folder not exists!".into()));
    }

    Ok(test_path)
}

pub async fn upload(
    conn: &Pool<Sqlite>,
    id: i32,
    _size: u64,
    mut payload: Multipart,
    path: &Path,
    abs_path: bool,
) -> Result<HttpResponse, ServiceError> {
    while let Some(mut field) = payload.try_next().await? {
        let content_disposition = field.content_disposition();
        debug!("{content_disposition}");
        let rand_string: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(20)
            .map(char::from)
            .collect();
        let filename = content_disposition
            .get_filename()
            .map_or_else(|| rand_string.to_string(), sanitize_filename::sanitize);

        let filepath = if abs_path {
            path.to_path_buf()
        } else {
            valid_path(conn, id, &path.to_string_lossy())
                .await?
                .join(filename)
        };
        let filepath_clone = filepath.clone();

        let _file_size = match filepath.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        };

        // INFO: File exist check should be enough because file size and content length are different.
        // The error catching in the loop should normally prevent unfinished files from existing on disk.
        // If this is not enough, a second check can be implemented: is_close(file_size as i64, size as i64, 1000)
        if filepath.is_file() {
            return Err(ServiceError::Conflict("Target already exists!".into()));
        }

        let mut f = web::block(|| std::fs::File::create(filepath_clone)).await??;

        loop {
            match field.try_next().await {
                Ok(Some(chunk)) => {
                    f = web::block(move || f.write_all(&chunk).map(|_| f)).await??;
                }

                Ok(None) => break,

                Err(e) => {
                    if e.to_string().contains("stream is incomplete") {
                        info!("Delete non finished file: {filepath:?}");

                        tokio::fs::remove_file(filepath).await?
                    }

                    return Err(e.into());
                }
            }
        }
    }

    Ok(HttpResponse::Ok().into())
}
