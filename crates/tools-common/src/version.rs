pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_TAG: Option<&str> = option_env!("HAUCET_GIT_TAG");
pub const COMMIT_HASH: Option<&str> = option_env!("HAUCET_GIT_COMMIT");
pub const VERSION: &str = env!("HAUCET_VERSION");
pub const GIT_DIRTY: bool = cfg!(haucet_git_dirty);
pub const LICENSE_SPDX: &str = env!("CARGO_PKG_LICENSE");
pub const REPOSITORY_URL: &str = "https://github.com/ljlVink/haucet";
