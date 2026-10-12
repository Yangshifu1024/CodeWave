//! Test-only, per-target fault/barrier hooks. Never compiled into production.
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

type Hook = Box<dyn FnOnce() -> io::Result<()> + Send>;
static HOOKS: OnceLock<Mutex<HashMap<(PathBuf, &'static str), Hook>>> = OnceLock::new();

pub struct Guard(PathBuf, &'static str);
impl Drop for Guard {
    fn drop(&mut self) {
        HOOKS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .remove(&(self.0.clone(), self.1));
    }
}
pub fn install(
    path: &Path,
    point: &'static str,
    hook: impl FnOnce() -> io::Result<()> + Send + 'static,
) -> Guard {
    let path = crate::tools::pathutil::canonical_best_effort(path);
    HOOKS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert((path.clone(), point), Box::new(hook));
    Guard(path, point)
}
pub fn check(path: &Path, point: &'static str) -> io::Result<()> {
    let path = crate::tools::pathutil::canonical_best_effort(path);
    let hook = HOOKS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(&(path, point));
    match hook {
        Some(hook) => hook(),
        None => Ok(()),
    }
}
