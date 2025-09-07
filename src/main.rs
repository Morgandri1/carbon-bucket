use std::convert::Infallible;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use warp::{
    http::{HeaderValue, Response, StatusCode},
    Filter, Rejection, Reply,
};

// Storage location for files
const STORAGE_DIR: &str = "/store";

#[tokio::main]
async fn main() {
    // Create storage directory if it doesn't exist
    if !Path::new(STORAGE_DIR).exists() {
        fs::create_dir_all(STORAGE_DIR).expect("Failed to create storage directory");
        println!("Created storage directory: {}", STORAGE_DIR);
    }

    // Convert to Arc for sharing across routes
    let storage_path = Arc::new(PathBuf::from(STORAGE_DIR));

    // Routes definition
    let upload_route = warp::path("upload")
        .and(warp::post())
        .and(warp::body::content_length_limit(100 * 1024 * 1024)) // 100MB limit
        .and(warp::body::bytes())
        .and(warp::header("filename"))
        .and(with_storage_path(storage_path.clone()))
        .and_then(upload_file);

    let list_route = warp::path("files")
        .and(warp::get())
        .and(with_storage_path(storage_path.clone()))
        .and_then(list_files);

    let download_route = warp::path!("get" / String)
        .and(warp::get())
        .and(with_storage_path(storage_path.clone()))
        .and_then(download_file);

    let delete_route = warp::path!("delete" / String)
        .and(warp::delete())
        .and(with_storage_path(storage_path.clone()))
        .and_then(delete_file);

    // Combine all routes
    let routes = upload_route
        .or(list_route)
        .or(download_route)
        .or(delete_route)
        .with(warp::cors().allow_any_origin())
        .recover(handle_rejection);

    println!("Server started at http://localhost:3030");
    println!("Available endpoints:");
    println!("  POST /upload            - Upload files (requires 'filename' header)");
    println!("  GET  /files             - List all files");
    println!("  GET  /download/:filename - Download a file");
    println!("  DELETE /delete/:filename - Delete a file");

    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;
}

// Helper function to pass storage path to handlers
fn with_storage_path(
    storage_path: Arc<PathBuf>,
) -> impl Filter<Extract = (Arc<PathBuf>,), Error = Infallible> + Clone {
    warp::any().map(move || storage_path.clone())
}

// Handler for uploading files
async fn upload_file(
    bytes: bytes::Bytes,
    filename: String,
    storage_path: Arc<PathBuf>,
) -> Result<impl Reply, Rejection> {
    // Create file path
    let file_path = storage_path.join(&filename);

    // Write data to file
    let mut file = fs::File::create(&file_path).map_err(|e| {
        eprintln!("File creation error: {}", e);
        warp::reject::custom(FileError)
    })?;

    file.write_all(&bytes).map_err(|e| {
        eprintln!("File write error: {}", e);
        warp::reject::custom(FileError)
    })?;

    Ok(warp::reply::with_status(
        format!("Successfully uploaded: {}", filename),
        StatusCode::OK,
    ))
}

// Handler for listing files
async fn list_files(storage_path: Arc<PathBuf>) -> Result<impl Reply, Rejection> {
    let mut entries = fs::read_dir(&*storage_path)
        .map_err(|e| {
            eprintln!("Error reading directory: {}", e);
            warp::reject::custom(FileError)
        })?
        .filter_map(|entry| {
            entry.ok().and_then(|e| {
                if e.path().is_file() {
                    e.file_name().into_string().ok()
                } else {
                    None
                }
            })
        })
        .collect::<Vec<String>>();

    entries.sort();

    // Format as JSON
    let files_json = serde_json::json!({
        "files": entries,
        "count": entries.len(),
    })
    .to_string();

    Ok(warp::reply::json(&serde_json::from_str::<serde_json::Value>(&files_json).unwrap()))
}

// Handler for downloading files
async fn download_file(
    filename: String,
    storage_path: Arc<PathBuf>,
) -> Result<impl Reply, Rejection> {
    let file_path = storage_path.join(&filename);

    if !file_path.exists() {
        return Ok(warp::reply::with_status(
            format!("File '{}' not found", filename),
            StatusCode::NOT_FOUND,
        ).into_response());
    }

    // Read the file
    let mut file = tokio::fs::File::open(&file_path)
        .await
        .map_err(|e| {
            eprintln!("Error opening file: {}", e);
            warp::reject::custom(FileError)
        })?;

    let mut contents = Vec::new();
    file.read_to_end(&mut contents).await.map_err(|e| {
        eprintln!("Error reading file: {}", e);
        warp::reject::custom(FileError)
    })?;

    // Build response
    let mut response = Response::new(contents.into());
    
    // Add Content-Disposition header
    let header_value = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", filename))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    response.headers_mut().insert("Content-Disposition", header_value);

    Ok(response)
}

// Handler for deleting files
async fn delete_file(
    filename: String,
    storage_path: Arc<PathBuf>,
) -> Result<impl Reply, Rejection> {
    let file_path = storage_path.join(&filename);

    if !file_path.exists() {
        return Ok(warp::reply::with_status(
            format!("File '{}' not found", filename),
            StatusCode::NOT_FOUND,
        ));
    }

    fs::remove_file(&file_path).map_err(|e| {
        eprintln!("Error deleting file: {}", e);
        warp::reject::custom(FileError)
    })?;

    Ok(warp::reply::with_status(
        format!("File '{}' deleted successfully", filename),
        StatusCode::OK,
    ))
}

#[derive(Debug)]
struct FileError;

impl warp::reject::Reject for FileError {}

// Error handler
async fn handle_rejection(err: Rejection) -> Result<impl Reply, Infallible> {
    let (code, message) = if err.is_not_found() {
        (StatusCode::NOT_FOUND, "Not Found".to_string())
    } else if let Some(_) = err.find::<FileError>() {
        (StatusCode::INTERNAL_SERVER_ERROR, "File operation error".to_string())
    } else if let Some(_) = err.find::<warp::filters::body::BodyDeserializeError>() {
        (StatusCode::BAD_REQUEST, "Invalid body".to_string())
    } else if let Some(_) = err.find::<warp::reject::PayloadTooLarge>() {
        (StatusCode::BAD_REQUEST, "Payload too large".to_string())
    } else {
        eprintln!("Unhandled error: {:?}", err);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error".to_string(),
        )
    };

    Ok(warp::reply::with_status(message, code))
}