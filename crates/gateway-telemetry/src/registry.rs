use std::{
    collections::BTreeMap,
    fmt::Write as _,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
    time::Duration,
};

/// Every metric that has been written to at least once, in first-write
/// order. A static that was never touched is absent from the output, which
/// is also what a scrape of a fresh process should say.
static REGISTRY: RwLock<Vec<&'static dyn Render>> = RwLock::new(Vec::new());

/// One label set, in the order the metric declares it.
///
/// Label values come from bounded sets: a matched route, a status code, a
/// worker name, a provider host. Nothing merchant-controlled becomes a label,
/// because every distinct label set is a row kept for the life of the process.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Labels(Vec<(&'static str, String)>);

impl Labels {
    #[must_use]
    pub fn new(pairs: &[(&'static str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(name, value)| (*name, (*value).to_owned()))
                .collect(),
        )
    }

    #[must_use]
    pub const fn none() -> Self {
        Self(Vec::new())
    }

    fn render(&self, out: &mut String, extra: Option<(&str, &str)>) {
        if self.0.is_empty() && extra.is_none() {
            return;
        }
        out.push('{');
        let mut first = true;
        for (name, value) in &self.0 {
            if !first {
                out.push(',');
            }
            first = false;
            let _ = write!(out, "{name}=\"{}\"", escape(value));
        }
        if let Some((name, value)) = extra {
            if !first {
                out.push(',');
            }
            let _ = write!(out, "{name}=\"{value}\"");
        }
        out.push('}');
    }
}

trait Render: Sync {
    fn render(&self, out: &mut String);
}

/// Registers a metric the first time it is written.
fn register(metric: &'static dyn Render, registered: &AtomicBool) {
    if registered.swap(true, Ordering::AcqRel) {
        return;
    }
    // A poisoned lock means a panic while rendering or registering; metrics
    // must never take the process down, so the poison is ignored and the
    // registration still happens.
    let mut registry = REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.push(metric);
}

/// Renders every metric written so far in the Prometheus text format.
#[must_use]
pub fn render() -> String {
    let registry = REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut out = String::new();
    for metric in registry.iter() {
        metric.render(&mut out);
    }
    out
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn header(out: &mut String, name: &str, help: &str, kind: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

/// Whole microseconds written as seconds, with no floating point: `1500` is
/// `0.0015`, `2000000` is `2`.
fn seconds(micros: u64) -> String {
    let whole = micros / 1_000_000;
    let fraction = micros % 1_000_000;
    if fraction == 0 {
        return whole.to_string();
    }
    let digits = format!("{fraction:06}");
    format!("{whole}.{}", digits.trim_end_matches('0'))
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// A monotonically increasing count per label set.
#[derive(Debug)]
pub struct Counter {
    name: &'static str,
    help: &'static str,
    cells: RwLock<BTreeMap<Labels, Arc<AtomicU64>>>,
    registered: AtomicBool,
}

impl Counter {
    #[must_use]
    pub const fn new(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            help,
            cells: RwLock::new(BTreeMap::new()),
            registered: AtomicBool::new(false),
        }
    }

    pub fn increment(&'static self, labels: &Labels, by: u64) {
        register(self, &self.registered);
        cell(&self.cells, labels, || Arc::new(AtomicU64::new(0))).fetch_add(by, Ordering::Relaxed);
    }
}

impl Render for Counter {
    fn render(&self, out: &mut String) {
        header(out, self.name, self.help, "counter");
        let cells = self
            .cells
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (labels, value) in cells.iter() {
            out.push_str(self.name);
            labels.render(out, None);
            let _ = writeln!(out, " {}", value.load(Ordering::Relaxed));
        }
    }
}

/// A value that goes up and down per label set.
#[derive(Debug)]
pub struct Gauge {
    name: &'static str,
    help: &'static str,
    cells: RwLock<BTreeMap<Labels, Arc<AtomicI64>>>,
    registered: AtomicBool,
}

impl Gauge {
    #[must_use]
    pub const fn new(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            help,
            cells: RwLock::new(BTreeMap::new()),
            registered: AtomicBool::new(false),
        }
    }

    pub fn set(&'static self, labels: &Labels, value: i64) {
        register(self, &self.registered);
        cell(&self.cells, labels, || Arc::new(AtomicI64::new(0))).store(value, Ordering::Relaxed);
    }

    pub fn add(&'static self, labels: &Labels, delta: i64) {
        register(self, &self.registered);
        cell(&self.cells, labels, || Arc::new(AtomicI64::new(0)))
            .fetch_add(delta, Ordering::Relaxed);
    }
}

impl Render for Gauge {
    fn render(&self, out: &mut String) {
        header(out, self.name, self.help, "gauge");
        let cells = self
            .cells
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (labels, value) in cells.iter() {
            out.push_str(self.name);
            labels.render(out, None);
            let _ = writeln!(out, " {}", value.load(Ordering::Relaxed));
        }
    }
}

#[derive(Debug)]
struct HistogramCell {
    buckets: Vec<AtomicU64>,
    count: AtomicU64,
    sum_micros: AtomicU64,
}

/// Cumulative buckets of durations per label set, in microseconds.
#[derive(Debug)]
pub struct Histogram {
    name: &'static str,
    help: &'static str,
    bounds_micros: &'static [u64],
    cells: RwLock<BTreeMap<Labels, Arc<HistogramCell>>>,
    registered: AtomicBool,
}

impl Histogram {
    #[must_use]
    pub const fn new(
        name: &'static str,
        help: &'static str,
        bounds_micros: &'static [u64],
    ) -> Self {
        Self {
            name,
            help,
            bounds_micros,
            cells: RwLock::new(BTreeMap::new()),
            registered: AtomicBool::new(false),
        }
    }

    pub fn observe(&'static self, labels: &Labels, duration: Duration) {
        register(self, &self.registered);
        let observed = micros(duration);
        let cell = cell(&self.cells, labels, || {
            Arc::new(HistogramCell {
                buckets: self
                    .bounds_micros
                    .iter()
                    .map(|_| AtomicU64::new(0))
                    .collect(),
                count: AtomicU64::new(0),
                sum_micros: AtomicU64::new(0),
            })
        });
        for (bound, bucket) in self.bounds_micros.iter().zip(&cell.buckets) {
            if observed <= *bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        cell.count.fetch_add(1, Ordering::Relaxed);
        // Saturating: a sum that wrapped would read as a fast process.
        let mut current = cell.sum_micros.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_add(observed);
            match cell.sum_micros.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }
}

impl Render for Histogram {
    fn render(&self, out: &mut String) {
        header(out, self.name, self.help, "histogram");
        let cells = self
            .cells
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (labels, cell) in cells.iter() {
            for (bound, bucket) in self.bounds_micros.iter().zip(&cell.buckets) {
                let _ = write!(out, "{}_bucket", self.name);
                labels.render(out, Some(("le", &seconds(*bound))));
                let _ = writeln!(out, " {}", bucket.load(Ordering::Relaxed));
            }
            let count = cell.count.load(Ordering::Relaxed);
            let _ = write!(out, "{}_bucket", self.name);
            labels.render(out, Some(("le", "+Inf")));
            let _ = writeln!(out, " {count}");
            let _ = write!(out, "{}_sum", self.name);
            labels.render(out, None);
            let _ = writeln!(out, " {}", seconds(cell.sum_micros.load(Ordering::Relaxed)));
            let _ = write!(out, "{}_count", self.name);
            labels.render(out, None);
            let _ = writeln!(out, " {count}");
        }
    }
}

/// The cell for a label set, created on first use. Reads take the shared
/// lock; only a new label set takes the exclusive one.
fn cell<T>(
    cells: &RwLock<BTreeMap<Labels, Arc<T>>>,
    labels: &Labels,
    create: impl FnOnce() -> Arc<T>,
) -> Arc<T> {
    if let Some(found) = cells
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(labels)
    {
        return Arc::clone(found);
    }
    let mut cells = cells
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(cells.entry(labels.clone()).or_insert_with(create))
}

#[cfg(test)]
mod tests;
