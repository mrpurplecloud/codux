use serde_json::Value;
use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub fn terminal_upload_directory(session_id: &str) -> PathBuf {
    std::env::temp_dir()
        .join("CoduxUploads")
        .join(sanitized_upload_name(session_id))
}

pub fn sanitized_upload_name(value: &str) -> String {
    let name = Path::new(value)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("upload.png");
    let cleaned = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('.')
        .to_string();
    if cleaned.is_empty() {
        "upload.png".to_string()
    } else {
        cleaned
    }
}

pub fn terminal_upload_kind(payload: &Value) -> String {
    let kind = payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("image")
        .trim()
        .to_ascii_lowercase();
    if kind == "file" {
        "file".to_string()
    } else {
        "image".to_string()
    }
}

pub fn terminal_upload_path_input(path: &Path) -> String {
    quote_terminal_path(&path.to_string_lossy())
}

#[cfg(windows)]
pub fn quote_terminal_path(value: &str) -> String {
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '&' | '(' | ')' | '[' | ']' | '{' | '}'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(not(windows))]
pub fn quote_terminal_path(value: &str) -> String {
    if value.chars().any(|ch| {
        ch.is_whitespace()
            || matches!(
                ch,
                '\'' | '"' | '\\' | '$' | '`' | '!' | '&' | '(' | ')' | ';' | '<' | '>' | '|'
            )
    }) {
        format!("'{}'", value.replace('\'', "'\\''"))
    } else {
        value.to_string()
    }
}

pub fn write_unique_upload_file(
    directory: &Path,
    file_name: &str,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    let file_name = sanitized_upload_name(file_name);
    let path = PathBuf::from(&file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("upload");
    let extension = path.extension().and_then(|value| value.to_str());
    let mut index = 0_u64;

    loop {
        let name = match (index, extension) {
            (0, _) => file_name.clone(),
            (_, Some(extension)) if !extension.is_empty() => {
                format!("{stem}-{index}.{extension}")
            }
            _ => format!("{stem}-{index}"),
        };
        let candidate = directory.join(name);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                index = index.checked_add(1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::AlreadyExists, "upload name space exhausted")
                })?;
                continue;
            }
            Err(error) => return Err(error),
        };

        if let Err(write_error) = file.write_all(bytes) {
            drop(file);
            return match std::fs::remove_file(&candidate) {
                Ok(()) => Err(write_error),
                Err(cleanup_error) => Err(io::Error::new(
                    write_error.kind(),
                    format!(
                        "could not write upload: {write_error}; could not remove owned file: {cleanup_error}"
                    ),
                )),
            };
        }
        return Ok(candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn upload_names_are_sanitized() {
        assert_eq!(
            sanitized_upload_name("../unsafe path/$image.png"),
            "_image.png"
        );
        assert_eq!(sanitized_upload_name("..."), "upload.png");
    }

    #[test]
    fn upload_path_quotes_shell_sensitive_paths() {
        assert_eq!(
            quote_terminal_path("/tmp/CoduxUploads/file.txt"),
            "/tmp/CoduxUploads/file.txt"
        );

        #[cfg(not(windows))]
        assert_eq!(
            terminal_upload_path_input(Path::new("/tmp/Codux Uploads/file name.txt")),
            "'/tmp/Codux Uploads/file name.txt'"
        );
    }

    #[test]
    fn concurrent_upload_writes_claim_distinct_paths() {
        let directory = std::env::temp_dir().join(format!(
            "codux-runtime-upload-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).expect("create upload directory");
        let start = Arc::new(Barrier::new(3));
        let handles = [b"first".as_slice(), b"second".as_slice()].map(|bytes| {
            let directory = directory.clone();
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                write_unique_upload_file(&directory, "asset.png", bytes)
                    .expect("write unique upload")
            })
        });

        start.wait();
        let [first, second] = handles.map(|handle| handle.join().expect("join upload writer"));

        assert_ne!(first, second);
        let mut contents = [
            std::fs::read(first).expect("read first upload"),
            std::fs::read(second).expect("read second upload"),
        ];
        contents.sort();
        assert_eq!(contents, [b"first".to_vec(), b"second".to_vec()]);
        std::fs::remove_dir_all(directory).ok();
    }
}
