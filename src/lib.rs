pub mod app;
pub mod attach;
pub mod config;
pub mod git;
pub mod highlight;
pub mod hook;
pub mod init;
pub mod paths;
#[doc(hidden)]
pub mod perf;
pub mod prompt;
pub mod scar;
pub mod session;
pub mod stream;
pub mod ui;
pub mod watcher;

#[cfg(test)]
mod test_support;
