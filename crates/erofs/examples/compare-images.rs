use anyhow::{Context, ensure};
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(args.len() == 2, "usage: compare-images ORIGINAL REBUILT");
    let original = erofs::image_inventory(Path::new(&args[0])).context("checking original")?;
    let rebuilt = erofs::image_inventory(Path::new(&args[1])).context("checking rebuilt")?;
    for (path, digest) in original
        .sha256
        .iter()
        .filter(|(path, digest)| rebuilt.sha256.get(*path) != Some(*digest))
        .take(20)
    {
        eprintln!(
            "content differs {path}: original={digest}, rebuilt={:?}, original link={:?}, rebuilt link={:?}",
            rebuilt.sha256.get(path),
            original.metadata.entries[path].symlink,
            rebuilt
                .metadata
                .entries
                .get(path)
                .and_then(|entry| entry.symlink.as_ref())
        );
    }
    ensure!(
        original.sha256 == rebuilt.sha256,
        "file content digests or paths differ"
    );
    ensure!(
        original.metadata.entries.len() == rebuilt.metadata.entries.len(),
        "entry counts differ"
    );
    for (path, entry) in &original.metadata.entries {
        let mut expected = entry.clone();
        let mut actual = rebuilt
            .metadata
            .entries
            .get(path)
            .with_context(|| format!("missing {path}"))?
            .clone();
        expected.original_nid = None;
        actual.original_nid = None;
        ensure!(
            expected == actual,
            "metadata differs at {path}:\nexpected {expected:?}\nactual {actual:?}"
        );
    }
    ensure!(
        original.metadata.uuid == rebuilt.metadata.uuid,
        "filesystem UUID differs"
    );
    ensure!(
        original.metadata.volume_label == rebuilt.metadata.volume_label,
        "volume label differs"
    );
    let link_groups = |metadata: &erofs::metadata::MetadataManifest| {
        let mut groups = std::collections::BTreeMap::<u64, Vec<&str>>::new();
        for (path, entry) in &metadata.entries {
            if let Some(nid) = entry.original_nid {
                groups.entry(nid).or_default().push(path);
            }
        }
        let mut groups: Vec<_> = groups
            .into_values()
            .filter(|paths| paths.len() > 1)
            .map(|paths| paths.into_iter().map(str::to_owned).collect::<Vec<_>>())
            .collect();
        groups.sort();
        groups
    };
    ensure!(
        link_groups(&original.metadata) == link_groups(&rebuilt.metadata),
        "hardlink relationships differ"
    );
    println!(
        "Matched {} paths and {} file/symlink SHA-256 digests, inode metadata, xattrs, hardlinks, UUID and volume label",
        original.metadata.entries.len(),
        original.sha256.len()
    );
    Ok(())
}
