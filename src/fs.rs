use std::{fs, io, path::Path};

use anyhow::Context;

pub fn exists(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();

    path.exists()
}

pub fn read_dir(path: impl AsRef<Path>) -> anyhow::Result<fs::ReadDir> {
    let path = path.as_ref();

    fs::read_dir(path)
        .with_context(|| format!("failed to read directory '{path}'", path = path.display()))
}

pub fn read(path: impl AsRef<Path>) -> Option<anyhow::Result<String>> {
    let path = path.as_ref();

    match fs::read_to_string(path) {
        Ok(string) => Some(Ok(string.trim().to_owned())),

        Err(error) if error.kind() == io::ErrorKind::NotFound => None,

        Err(error) => Some(
            Err(error).with_context(|| format!("failed to read '{path}", path = path.display())),
        ),
    }
}

pub fn read_u64(path: impl AsRef<Path>) -> Option<anyhow::Result<u64>> {
    let path = path.as_ref();

    match read(path)? {
        Ok(content) => Some(content.trim().parse().with_context(|| {
            format!(
                "failed to parse contents of '{path}' as a unsigned number",
                path = path.display(),
            )
        })),

        Err(error) => Some(Err(error)),
    }
}

pub fn write(path: impl AsRef<Path>, value: &str) -> anyhow::Result<()> {
    let path = path.as_ref();

    fs::write(path, value).with_context(|| {
        format!(
            "failed to write '{value}' to '{path}'",
            path = path.display(),
        )
    })
}
