use std::io::Error as IoError;
use std::error;
use std::fmt;
use telegram_bot::Error as TelegramError;

#[derive(Debug)]
pub enum Error {
    Io(IoError),
    Telegram(TelegramError)
}

pub type Result<T> = ::std::result::Result<T, Error>;

impl From<IoError> for Error {
    fn from(err: IoError) -> Error {
        Error::Io(err)
    }
}

impl From<TelegramError> for Error {
    fn from(err: TelegramError) -> Error {
        Error::Telegram(err)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Io(ref err) => err.fmt(f),
            Error::Telegram(ref err) => err.fmt(f)
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::Io(ref err) => err.description(),
            Error::Telegram(ref err) => err.description()
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::Io(ref err) => err.cause(),
            Error::Telegram(ref err) => err.cause()
        }
    }
}
