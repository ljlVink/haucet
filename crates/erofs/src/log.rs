pub const TAG_INFO: &str = "\x1b[93;1mExtract: \x1b[m";
pub const TAG_ERR: &str = "\x1b[91;1mExtract: \x1b[m";
pub const TAG_WARN: &str = "\x1b[93;1mExtract: \x1b[m";

pub const GREEN2_BOLD: &str = "\x1b[1;92m";
pub const RED2: &str = "\x1b[0;91m";
pub const BROWN: &str = "\x1b[0;33m";
pub const RESET: &str = "\x1b[m";

pub fn logi(msg: &str) {
    println!("{}{}", TAG_INFO, msg);
}

pub fn logw(msg: &str) {
    println!("{}{}", TAG_WARN, msg);
}

pub fn loge(msg: &str) {
    println!("{}{}", TAG_ERR, msg);
}
