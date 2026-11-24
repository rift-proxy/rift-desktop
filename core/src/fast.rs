//! The fast check: one answer from each server of the pool and from the server in use, and a
//! second look at the ones that changed. It switches nothing; the report says what the full check
//! that may follow has to know.
use crate::{
    failure::FailureCode,
    jitter::Jitter,
    model::Server,
    pool::{self, Health, Judgement, Tolerance},
    selection::{probe_in_use, proxy_once, wait_turn, Measurement, Probe, Progress},
    Error, Result,
};
use futures::{stream, StreamExt};
use tokio_util::sync::CancellationToken;

/// A server the fast check asks about, with the delay it had when the pool was built (none: it was
/// never measured, and gets one from this check).
pub struct Watched<'a> {
    pub server: &'a Server,
    pub base_ms: Option<u64>,
}
/// What the check made of one server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    pub id: String,
    pub judgement: Judgement,
}
/// What a fast check found.
#[derive(Debug)]
pub struct Report {
    /// The servers in the order their verdicts came.
    pub readings: Vec<Reading>,
    /// The server in use did not answer, nor to a second try. Nobody else was asked: the check that
    /// replaces it asks the pool.
    pub current_lost: bool,
}
impl Report {
    /// Everything answers as it did.
    pub fn all_fine(&self) -> bool {
        !self.current_lost
            && self
                .readings
                .iter()
                .all(|r| r.judgement.health == Health::Fine)
    }
    /// The servers that did not answer.
    pub fn lost(&self) -> Vec<&str> {
        self.readings
            .iter()
            .filter(|r| r.judgement.health == Health::Lost)
            .map(|r| r.id.as_str())
            .collect()
    }
}
/// A verdict with what the table is to be told on the way to it.
struct Seen {
    id: String,
    judgement: Judgement,
    events: Vec<Progress>,
}

/// The server in use is asked first, with the second try a timeout gets (a miss there costs a
/// switch away from a connection that works). When it does not answer the check ends. Then the
/// others, in a random order, at most `concurrency` at once, each after a pause of its own: a burst
/// to the same few servers at the same moment every minute is easy to recognise.
///
/// An answer within the tolerance of the delay the server had is enough. Silence or a slower
/// answer is looked at twice more, and the middle of the three decides (see `pool::judge`).
#[allow(clippy::too_many_arguments)]
pub async fn run_fast<P: Probe + ?Sized>(
    probe: &P,
    watched: &[Watched<'_>],
    current: Option<&str>,
    tolerance: Tolerance,
    concurrency: usize,
    jitter: &Jitter,
    cancel: &CancellationToken,
    mut emit: impl FnMut(Progress) + Send,
) -> Result<Report> {
    if concurrency == 0 {
        // i18n-ignore: a guard against a caller bug, no input reaches it
        return Err(Error::Invalid("Concurrency must be positive".into()));
    }
    let mut report = Report {
        readings: vec![],
        current_lost: false,
    };
    if let Some(in_use) = current.and_then(|id| watched.iter().find(|w| w.server.id == id)) {
        let first = probe_in_use(probe, in_use.server, cancel).await?;
        if first.latency_ms.is_none() {
            emit(Progress::Current(in_use.server.id.clone(), first));
            report.readings.push(Reading {
                id: in_use.server.id.clone(),
                judgement: Judgement {
                    health: Health::Lost,
                    median_ms: None,
                },
            });
            report.current_lost = true;
            return Ok(report);
        }
        let seen = look_again(probe, in_use, first, true, tolerance, cancel).await?;
        say(&mut report, &mut emit, seen);
    }
    let mut others: Vec<&Watched> = watched
        .iter()
        .filter(|w| Some(w.server.id.as_str()) != current)
        .collect();
    jitter.shuffle(&mut others);
    let total = others.len();
    let turn = tokio::sync::Mutex::new(());
    let mut asked = Vec::with_capacity(total);
    for watched in others {
        let turn = &turn;
        asked.push(async move {
            wait_turn(turn, jitter, total, cancel).await?;
            let first = Measurement::from_result(
                proxy_once(probe, watched.server, cancel).await?,
                FailureCode::ProxyTransport,
            );
            look_again(probe, watched, first, false, tolerance, cancel).await
        });
    }
    let answers = stream::iter(asked).buffer_unordered(concurrency);
    tokio::pin!(answers);
    loop {
        tokio::select! { biased;
            _=cancel.cancelled()=>return Err(Error::Cancelled),
            next=answers.next()=>match next {
                Some(Ok(seen))=>say(&mut report, &mut emit, seen),
                Some(Err(error))=>return Err(error),
                None=>return Ok(report),
            }
        }
    }
}
fn say(report: &mut Report, emit: &mut (impl FnMut(Progress) + Send), seen: Seen) {
    for event in seen.events {
        emit(event);
    }
    report.readings.push(Reading {
        id: seen.id,
        judgement: seen.judgement,
    });
}
/// Judges a server by its `first` answer, and by two more when that is not enough. The table is
/// told once, with the verdict: a first answer that two more are to confirm or overrule is not
/// shown, or it would mark a server failed (and count a miss against it) that the check finds
/// working.
async fn look_again<P: Probe + ?Sized>(
    probe: &P,
    watched: &Watched<'_>,
    first: Measurement,
    in_use: bool,
    tolerance: Tolerance,
    cancel: &CancellationToken,
) -> Result<Seen> {
    let id = watched.server.id.clone();
    let tell = |measurement: Measurement| {
        if in_use {
            Progress::Current(id.clone(), measurement)
        } else {
            Progress::Proxy(id.clone(), measurement)
        }
    };
    if !pool::needs_confirmation(watched.base_ms, first.latency_ms, tolerance) {
        let judgement = pool::judge(watched.base_ms, &[first.latency_ms], tolerance);
        return Ok(Seen {
            judgement,
            events: vec![tell(first)],
            id,
        });
    }
    let mut samples = vec![first.latency_ms];
    let mut failure = first.failure_code;
    for _ in 0..2 {
        let again = Measurement::from_result(
            proxy_once(probe, watched.server, cancel).await?,
            FailureCode::ProxyTransport,
        );
        failure = again.failure_code.or(failure);
        samples.push(again.latency_ms);
    }
    let judgement = pool::judge(watched.base_ms, &samples, tolerance);
    // What the table is left with is what was decided: the middle of the answers, or the failure.
    let verdict = match judgement.median_ms {
        Some(proxy_ms) => Measurement {
            latency_ms: Some(proxy_ms),
            failure_code: None,
        },
        None => Measurement {
            latency_ms: None,
            failure_code: failure.or(Some(FailureCode::ProxyTransport)),
        },
    };
    Ok(Seen {
        judgement,
        events: vec![tell(verdict)],
        id,
    })
}
