# Third-Party Notices

This file records the material shipped with, or used as a direct source for,
Haucet. It is not a substitute for the referenced license texts.

## Bundled Runtime Components

| Component | Location | License | Source and notes |
| --- | --- | --- | --- |
| HarmonyOS Sans SC Regular | `assets/HarmonyOS_Sans_SC_Regular.ttf` | HarmonyOS Sans Fonts License Agreement | Copyright 2021 Huawei Device Co., Ltd. The agreement permits bundling unmodified copies with non-font software, requires a prominent usage notice, forbids standalone redistribution, and must accompany copies. See `LICENSES/HarmonyOS-Sans.txt`. |

## Adapted Code References

- `crates/hm-fastboot` is a modified fork of
  [boardswarm/fastboot-rs](https://github.com/boardswarm/fastboot-rs), whose
  upstream code is available under MIT or Apache-2.0. The fork as distributed
  here is GPL-3.0-only; the upstream MIT notice is retained in
  `LICENSES/MIT-fastboot-rs.txt`.
- `crates/erofs` contains the in-process Rust EROFS extraction and image-building
  implementations, including the standalone `extract-erofs` and `mkfs-erofs`
  binaries. Its filesystem-format behavior and extraction/repacking workflows are
  a Rust port informed by [sekaiacg/erofs-tools](https://github.com/sekaiacg/erofs-tools)
  and upstream [erofs-utils](https://github.com/erofs/erofs-utils). Upstream
  notices are retained with the adapted code, including erofs-utils components
  offered under GPL-2.0-or-later or MIT. The GPL-2.0 license text is retained in
  `LICENSES/GPL-2.0.txt`; the MIT text and format-definition notices are in
  `LICENSES/MIT-erofs-utils.txt`. Legacy erofs-utils binaries and their Cygwin runtime in
  `bin/`, when distributed, retain their upstream licenses; the Rust workflows
  no longer execute those binaries.
- `crates/ext4-extract` provides in-process Rust ext2/ext4 extraction. Filesystem parsing
  uses [ext4-view](https://github.com/nicholasbishop/ext4-view-rs), licensed
  under MIT or Apache-2.0, for file extraction and
  [ext4-rs](https://github.com/FauxFaux/ext4-rs), licensed under MIT, for inode
  extended attributes. This distribution uses ext4-view under Apache-2.0. See
  `LICENSES/GPL-2.0.txt`, `LICENSES/Apache-2.0.txt`, and
  `LICENSES/MIT-ext4-rs.txt`.
- HarmonyOS update package behavior was adapted from
  [OpenHarmony update_packaging_tools](https://gitcode.com/openharmony/update_packaging_tools),
  and HVB behavior was adapted from
  [OpenHarmony startup_hvb](https://gitcode.com/openharmony/startup_hvb), both
  under Apache-2.0. See `LICENSES/Apache-2.0.txt`.
- Ramdisk behavior is based on
  [ljlVink/ramdisk-tools](https://github.com/ljlVink/ramdisk-tools), with
  patch-layout reference to [Magisk](https://github.com/topjohnwu/Magisk).
  Both are GPL-3.0 projects.
- Image-flashing behavior references
  [Potato.ImageFlasher](https://github.com/kitsuned/Potato.ImageFlasher), and
  NVE behavior references [hisi-nve](https://github.com/R0rt1z2/hisi-nve).
  Both are GPL-3.0 projects.
- OEMINFO block parsing and payload-classification behavior references
  [ud3v0id/huawei-oeminfo-tool](https://github.com/ud3v0id/huawei-oeminfo-tool),
  Copyright (c) 2025 ud3v0id and licensed under MIT. See
  `LICENSES/MIT-huawei-oeminfo-tool.txt`.
