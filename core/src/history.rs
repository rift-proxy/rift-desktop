//! What the latest checks learned about each server. Kept in memory only: it is gone when Rift
//! exits, it never reaches the disk, the journal, a diagnostics file, a profile or a report, and
//! it is not part of the snapshot (which every event carries). The screen asks for one server's
//! rows when it shows them.
use crate::{failure::FailureCode, selection::Progress};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};

/// Rows kept per server.
pub const KEEP: usize = 20;
/// Servers followed at once, far above what a subscription holds; a longer list drops the oldest
/// servers' rows rather than growing without bound.
const MAX_SERVERS: usize = 5000;

/// What one check learned about one server: its distance, its answer through it and, when a
/// stage failed, why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Sample {
    /// Unix seconds of the first measurement of the check.
    pub at: u64,
    pub endpoint_ms: Option<u64>,
    pub proxy_ms: Option<u64>,
    pub failure_code: Option<FailureCode>,
    /// The check that took it; rows of one check are one row.
    #[serde(skip)]
    cycle: u64,
}

#[derive(Default)]
pub struct History {
    cycle: u64,
    next_check: u64,
    servers: HashMap<String, VecDeque<Sample>>,
}
impl History {
    /// A selection cycle begins: its measurements are new rows. Servers that are no longer in
    /// the list are forgotten.
    pub fn begin_cycle(&mut self, known: impl Fn(&str) -> bool) {
        self.cycle = self.begin_check();
        self.servers.retain(|id, _| known(id));
    }
    /// The row of this check for `id`, made when it is the first thing the check says of it.
    pub fn begin_check(&mut self) -> u64 {
        self.next_check += 1;
        self.next_check
    }
    fn row(&mut self, id: &str, at: u64, cycle: u64) -> &mut Sample {
        if !self.servers.contains_key(id) && self.servers.len() >= MAX_SERVERS {
            // Never reached with a real list; the oldest-keyed server makes room.
            if let Some(oldest) = self.servers.keys().next().cloned() {
                self.servers.remove(&oldest);
            }
        }
        let rows = self.servers.entry(id.to_owned()).or_default();
        if rows.back().is_none_or(|row| row.cycle != cycle) {
            rows.push_back(Sample {
                at,
                endpoint_ms: None,
                proxy_ms: None,
                failure_code: None,
                cycle,
            });
            while rows.len() > KEEP {
                rows.pop_front();
            }
        }
        rows.back_mut().expect("a row was just made")
    }
    /// Takes a measurement of the running cycle.
    pub fn record(&mut self, event: &Progress, at: u64) {
        self.record_check(event, at, self.cycle);
    }
    /// Measurements of an independent recheck keep their own identity while selection runs.
    pub fn record_check(&mut self, event: &Progress, at: u64, cycle: u64) {
        match event {
            Progress::Endpoint(id, measurement) => {
                let row = self.row(id, at, cycle);
                row.endpoint_ms = measurement.latency_ms;
                if measurement.failure_code.is_some() {
                    row.failure_code = measurement.failure_code;
                }
            }
            Progress::Current(id, measurement) | Progress::Proxy(id, measurement) => {
                let row = self.row(id, at, cycle);
                row.proxy_ms = measurement.latency_ms;
                // The last stage speaks: a server whose distance failed but whose answer came
                // is not failing.
                row.failure_code = measurement.failure_code;
            }
            Progress::Median {
                id,
                proxy_ms,
                endpoint_ms,
            } => {
                let row = self.row(id, at, cycle);
                row.proxy_ms = Some(*proxy_ms);
                if endpoint_ms.is_some() {
                    row.endpoint_ms = *endpoint_ms;
                }
            }
            Progress::SelectionFailed(id, code) => {
                self.row(id, at, cycle).failure_code = Some(*code);
            }
            Progress::Selected(..) => {}
        }
    }
    /// The rows of one server, the oldest first.
    pub fn of(&self, id: &str) -> Vec<Sample> {
        self.servers
            .get(id)
            .map(|rows| rows.iter().cloned().collect())
            .unwrap_or_default()
    }
}
