use failure::Fail;
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, failure::Error>;

#[derive(Debug, Fail)]
#[fail(display = "no such directory found: {:?}", _0)]
pub struct NoSuchDirectory(pub PathBuf);
