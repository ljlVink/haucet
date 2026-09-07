# erofs

Embedded Rust EROFS extraction, image building, and verification for Haucet.
The crate replaces the former `erofs-extract` package.

The builder writes compact compressed indexes by default and reuses unused inode
slots for inline file tails. `mkfs-erofs -Elegacy-compress` selects full indexes
for compatibility comparisons. Both modes use streaming LZ4/LZ4HC compression.

Normal extraction writes `*_fs_config`, `*_file_contexts`, and `*_fs_options`;
it does not generate `erofs-metadata.json`. An explicitly supplied `--metadata`
file is still supported for additional inode metadata and arbitrary xattrs.

`haucet erofs repack` preserves the HVB certificate and requires the rebuilt
filesystem to fit within its recorded `image_len`. Space before the certificate
is not necessarily all addressable by the device's filesystem mapping.
`--allow-grow` cannot enlarge this recorded length without replacing the
certificate. Images without HVB retain the normal `--allow-grow` behavior.
