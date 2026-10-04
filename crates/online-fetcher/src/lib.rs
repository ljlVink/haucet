pub mod api;

mod remote;

use anyhow::{Context, Result, ensure};
use remote::RemoteFile;
use serde::{Deserialize, Serialize};
use std::io::Read;

const MAX_VERSION_SIZE: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub url: String,
    pub entry_name: String,
    pub content: Vec<u8>,
    pub archive_size: u64,
    pub downloaded_bytes: u64,
    pub range_requests: usize,
}

impl VersionInfo {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.content)
            .trim_start_matches('\u{feff}')
            .trim_end_matches('\0')
            .to_owned()
    }
}

pub fn fetch_version(url: &str) -> Result<VersionInfo> {
    let mut remote = RemoteFile::open(url.trim())?;
    let (entry_name, content) = read_version(&mut remote).map_err(|error| {
        if let Some(reason) = remote.failure() {
            error.context(reason.to_owned())
        } else {
            error
        }
    })?;
    Ok(VersionInfo {
        url: remote.url().to_owned(),
        entry_name,
        content,
        archive_size: remote.len(),
        downloaded_bytes: remote.downloaded_bytes(),
        range_requests: remote.requests(),
    })
}

fn read_version(remote: &mut RemoteFile) -> Result<(String, Vec<u8>)> {
    let mut archive = zip::ZipArchive::new(remote).context("reading remote ZIP directory")?;
    let mut matches = archive
        .file_names()
        .filter(|name| {
            name.rsplit(['/', '\\'])
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("VERSION.mbn"))
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    matches.sort_by_key(|name| (name.contains(['/', '\\']), name.clone()));
    let entry_name = matches
        .into_iter()
        .next()
        .context("VERSION.mbn was not found in the ZIP archive")?;
    let mut entry = archive
        .by_name(&entry_name)
        .with_context(|| format!("opening {entry_name}"))?;
    let expected_size = entry.size();
    ensure!(
        expected_size <= MAX_VERSION_SIZE && entry.compressed_size() <= MAX_VERSION_SIZE,
        "VERSION.mbn exceeds the 1 MiB size limit"
    );
    let mut content = Vec::new();
    entry
        .by_ref()
        .take(MAX_VERSION_SIZE + 1)
        .read_to_end(&mut content)
        .context("decompressing and verifying VERSION.mbn")?;
    ensure!(
        content.len() as u64 == expected_size,
        "VERSION.mbn decompressed size does not match the ZIP directory"
    );
    Ok((entry_name, content))
}
