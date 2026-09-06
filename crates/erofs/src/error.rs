use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

impl Error {
    pub fn errno(code: i32) -> Error {
        Error(code)
    }

    pub fn eopnotsupp() -> Error {
        Error(-libc::EOPNOTSUPP)
    }

    pub fn efscorrupted() -> Error {
        Error(-crate::platform::errno::EUCLEAN)
    }

    pub fn enodata() -> Error {
        Error(-libc::ENODATA)
    }

    pub fn eio() -> Error {
        Error(-libc::EIO)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error(-crate::platform::io_error_code(&e))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.0;
        if code < 0 {
            write!(f, "{}", crate::platform::error_message(-code))
        } else {
            write!(f, "errno {}", code)
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
