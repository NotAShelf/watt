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

pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Option<String>> {
    let path = path.as_ref();

    match fs::read_to_string(path) {
        Ok(string) => Ok(Some(string)),

        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),

        Err(error) => {
            Err(error).with_context(|| format!("failed to read '{path}", path = path.display()))
        }
    }
}

pub fn read_u64(path: impl AsRef<Path>) -> anyhow::Result<u64> {
    let path = path.as_ref();

    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read '{path}'", path = path.display()))?;

    Ok(content.trim().parse().with_context(|| {
        format!(
            "failed to parse contents of '{path}' as a unsigned number",
            path = path.display(),
        )
    })?)
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
