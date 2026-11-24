//! Startup milestones for the Windows performance harness.
//! The harness starts a stopwatch, launches Rift with [`READY_FILE_ENV`] naming a file and
//! stops when the file appears: the frontend reports its first usable screen. The file
//! also carries internal milestones (ms since `main`) that show where startup went.
use crate::{persistence, Result};
use serde_json::{Map, Value};
use std::{
    path::Path,
    sync::{Mutex, PoisonError},
    time::Instant,
};

/// Set only by the performance harness; normal launches never write the file.
pub const READY_FILE_ENV: &str = "RIFT_READY_FILE";

pub struct Milestones {
    start: Instant,
    marks: Mutex<Vec<(&'static str, u64)>>,
}
impl Default for Milestones {
    fn default() -> Self {
        Self::new()
    }
}
impl Milestones {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            marks: Mutex::new(Vec::new()),
        }
    }
    /// Records `name` once; later marks with the same name (a reloaded page) are ignored.
    pub fn mark(&self, name: &'static str) {
        let elapsed = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut marks = self.marks.lock().unwrap_or_else(PoisonError::into_inner);
        if !marks.iter().any(|(n, _)| *n == name) {
            marks.push((name, elapsed));
        }
    }
    /// `{"milestones_ms": {"main": 0, …}}`, written atomically; values give the order.
    pub fn write(&self, path: &Path) -> Result<()> {
        let marks = self.marks.lock().unwrap_or_else(PoisonError::into_inner);
        let milestones: Map<String, Value> = marks
            .iter()
            .map(|(name, ms)| ((*name).to_owned(), Value::from(*ms)))
            .collect();
        let body = serde_json::json!({ "milestones_ms": milestones });
        persistence::atomic_write(path, &serde_json::to_vec_pretty(&body)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_mark_wins_and_time_does_not_go_back() {
        let milestones = Milestones::new();
        milestones.mark("main");
        std::thread::sleep(std::time::Duration::from_millis(5));
        milestones.mark("ui_ready");
        milestones.mark("main");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ready.json");
        milestones.write(&path).unwrap();
        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let marks = value["milestones_ms"].as_object().unwrap();
        assert_eq!(marks.len(), 2, "a repeated mark is ignored");
        assert!(marks["ui_ready"].as_u64().unwrap() >= 5);
        // "main" keeps its first time, before the 5 ms pause, not the repeated one after it.
        assert!(marks["main"].as_u64().unwrap() < marks["ui_ready"].as_u64().unwrap());
    }

    #[test]
    fn write_fails_without_a_parent_directory() {
        assert!(Milestones::new().write(Path::new("/")).is_err());
    }
}
