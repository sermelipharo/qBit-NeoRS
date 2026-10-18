mod fs;
mod opened_file;
pub(crate) mod os;
mod sparse;

pub use fs::{FilesystemStorage, FilesystemStorageFactory};
pub use opened_file::OurFileExt;
pub use os::available_space;
