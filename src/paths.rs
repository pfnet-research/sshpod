use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .context("failed to determine home directory; set HOME")
}
