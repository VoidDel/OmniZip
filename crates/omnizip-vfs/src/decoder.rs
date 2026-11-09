//! Decoding for a mounted archive (`docs/12-mount.md` §4, §5), after DwarFS's block cache: a
//! pool of worker threads, each with a reader of its own, decodes entries into the
//! [`ContentCache`] while file-system calls only wait for them.
//!
//! * A read that misses the cache asks for its entry and waits on the entry's [`Partial`]. It
//!   never decodes on the file system's thread, nor holds a reader another call needs; its wait
//!   ends with an error when the decoder stops making progress.
//! * A job is one decoding pass: one entry (ZIP), a solid block (7z) or the whole stream
//!   (tar.gz), delivering every entry it is asked for on the way. Reads of an entry already
//!   being decoded share that pass.
//! * Passes a read waits for run before passes queued ahead of reads (read-ahead, warm-up), and
//!   a read of an entry queued ahead moves its pass forward.
//! * Workers start when there is work and stop after a while without. Each keeps a clone of the
//!   archive reader ([`ArchiveReader::try_clone`]), so independent passes decode side by side;
//!   for an engine that cannot clone its reader, one worker decodes on the shared reader.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{self, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use omnizip_core::{
    ArchiveReader, CancelToken, Entry, EntryData, EntryId, Error, ErrorCode, ExtractSink,
    Interaction, JobCtx, NullProgress, SecurityPolicy,
};

use crate::{
    FsError, FsResult,
    cache::{Content, ContentBuilder, ContentCache, Partial, Progress},
};

/// The archive reader every part of a volume can reach.
pub(crate) type SharedReader = Arc<Mutex<Box<dyn ArchiveReader>>>;

/// A worker with nothing to do for this long stops; its reader waits for the next one.
const IDLE_EXIT: Duration = Duration::from_secs(30);

fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One decoding pass.
struct Job {
    id: u64,
    targets: Vec<(EntryId, Arc<Partial>)>,
    progress: Arc<Progress>,
}

/// An entry queued or being decoded, and the pass that decodes it.
struct Pending {
    partial: Arc<Partial>,
    job: u64,
}

#[derive(Default)]
struct Queue {
    /// Passes that reads wait for.
    demand: VecDeque<Job>,
    /// Passes ahead of reads: read-ahead and warm-up.
    ahead: VecDeque<Job>,
    workers: usize,
    /// Workers waiting for a pass.
    idle: usize,
}

impl Queue {
    /// Every pass still queued.
    fn take_all(&mut self) -> Vec<Job> {
        self.demand.drain(..).chain(self.ahead.drain(..)).collect()
    }
}

/// Settings of a [`Pool`].
pub(crate) struct PoolOptions {
    /// Passes decoded at once, at most.
    pub workers: usize,
    /// How long a read waits on a pass that makes no progress.
    pub stall: Duration,
    pub interaction: Arc<dyn Interaction>,
    /// Cancelled when the volume goes away.
    pub cancel: CancelToken,
    pub policy: SecurityPolicy,
}

struct Shared {
    primary: SharedReader,
    /// Readers of workers that stopped, for the next ones.
    spare: Mutex<Vec<Box<dyn ArchiveReader>>>,
    /// The engine clones its reader; otherwise the one worker decodes on `primary`.
    clones: bool,
    max_workers: usize,
    cache: Arc<ContentCache>,
    pending: Mutex<HashMap<EntryId, Pending>>,
    next_job: AtomicU64,
    queue: Mutex<Queue>,
    work: Condvar,
    stall: Duration,
    interaction: Arc<dyn Interaction>,
    cancel: CancelToken,
    policy: SecurityPolicy,
    /// For the expansion-ratio limit.
    archive_size: u64,
}

/// The decoding workers of one volume.
pub(crate) struct Pool {
    s: Arc<Shared>,
}

impl Drop for Pool {
    /// Wakes idle workers to see that the volume is gone (its cancel token is set by then).
    fn drop(&mut self) {
        let _q = lock(&self.s.queue);
        self.s.work.notify_all();
    }
}

impl Pool {
    pub fn new(primary: SharedReader, cache: Arc<ContentCache>, o: PoolOptions) -> Self {
        let (first, archive_size) = {
            let r = lock(&primary);
            (r.try_clone(), r.info().archive_size)
        };
        let clones = first.is_some();
        Self {
            s: Arc::new(Shared {
                primary,
                spare: Mutex::new(first.into_iter().collect()),
                clones,
                max_workers: if clones { o.workers.max(1) } else { 1 },
                cache,
                pending: Mutex::default(),
                next_job: AtomicU64::new(1),
                queue: Mutex::default(),
                work: Condvar::new(),
                stall: o.stall,
                interaction: o.interaction,
                cancel: o.cancel,
                policy: o.policy,
                archive_size,
            }),
        }
    }

    /// Passes decoded at once, at most.
    pub fn max_workers(&self) -> usize {
        self.s.max_workers
    }

    /// Whether decoding uses the volume's shared reader (the engine cannot clone it), which a
    /// pass then holds for as long as it runs.
    pub fn decodes_on_shared_reader(&self) -> bool {
        !self.s.clones
    }

    /// The decode of an entry queued or under way; a read asking for it moves a pass queued
    /// ahead of reads forward.
    pub fn pending(&self, id: EntryId) -> Option<Arc<Partial>> {
        let (partial, job) = {
            let pending = lock(&self.s.pending);
            let p = pending.get(&id)?;
            (p.partial.clone(), p.job)
        };
        self.s.promote(job);
        Some(partial)
    }

    /// Queues `passes` (targets, and whether they go ahead of reads) for a read of `want`, which
    /// they must hold; entries cached or queued already are left out of them. The decode of
    /// `want`: under way already, finished when it was cached meanwhile, or queued now.
    pub fn request(&self, want: EntryId, passes: Vec<(Vec<EntryId>, bool)>) -> Arc<Partial> {
        let s = &self.s;
        let (mut demand, mut ahead) = (Vec::new(), Vec::new());
        let partial = {
            let mut pending = lock(&s.pending);
            if let Some(p) = pending.get(&want) {
                let (partial, job) = (p.partial.clone(), p.job);
                drop(pending);
                s.promote(job);
                return partial;
            }
            if let Some(c) = s.cache.get(want) {
                return Arc::new(Partial::finished(c));
            }
            for (targets, is_ahead) in passes {
                if let Some(job) = s.job(&mut pending, targets) {
                    match is_ahead {
                        true => ahead.push(job),
                        false => demand.push(job),
                    }
                }
            }
            if !pending.contains_key(&want)
                && let Some(job) = s.job(&mut pending, [want])
            {
                demand.push(job);
            }
            pending.get(&want).map(|p| p.partial.clone())
        };
        s.enqueue(demand, ahead);
        partial.unwrap_or_else(|| {
            // Cached between the look above and the pass being set up.
            let p = Partial::default();
            p.end(s.cache.get(want).ok_or(FsError::Io));
            Arc::new(p)
        })
    }

    /// Queues `passes` ahead of reads; the decode of every entry they hold that is not cached:
    /// queued now or already.
    pub fn ahead(&self, passes: Vec<Vec<EntryId>>) -> Vec<Arc<Partial>> {
        let s = &self.s;
        let mut jobs = Vec::new();
        let mut out = Vec::new();
        {
            let mut pending = lock(&s.pending);
            for targets in passes {
                let existing: Vec<_> = targets
                    .iter()
                    .filter_map(|id| pending.get(id).map(|p| p.partial.clone()))
                    .collect();
                out.extend(existing);
                if let Some(job) = s.job(&mut pending, targets) {
                    out.extend(job.targets.iter().map(|(_, p)| p.clone()));
                    jobs.push(job);
                }
            }
        }
        s.enqueue(Vec::new(), jobs);
        out
    }
}

impl Shared {
    fn ctx(&self) -> JobCtx {
        JobCtx::new(Arc::new(NullProgress), self.interaction.clone())
            .with_cancel(self.cancel.clone())
            .with_policy(self.policy.clone())
    }

    /// A pass over those of `targets` that are neither cached nor queued, registered as
    /// pending; `None` when that leaves nothing.
    fn job(
        &self,
        pending: &mut HashMap<EntryId, Pending>,
        targets: impl IntoIterator<Item = EntryId>,
    ) -> Option<Job> {
        let id = self.next_job.fetch_add(1, Ordering::Relaxed);
        let progress = Arc::new(Progress::default());
        let mut out = Vec::new();
        for e in targets {
            if pending.contains_key(&e) || self.cache.contains(e) {
                continue;
            }
            let p = Arc::new(Partial::new(self.stall, progress.clone()));
            pending.insert(
                e,
                Pending {
                    partial: p.clone(),
                    job: id,
                },
            );
            out.push((e, p));
        }
        (!out.is_empty()).then_some(Job {
            id,
            targets: out,
            progress,
        })
    }

    /// Moves pass `job` from the ahead queue to the end of the demand queue.
    fn promote(&self, job: u64) {
        let mut q = lock(&self.queue);
        if let Some(i) = q.ahead.iter().position(|j| j.id == job)
            && let Some(j) = q.ahead.remove(i)
        {
            q.demand.push_back(j);
        }
    }

    /// Queues passes and makes sure workers are there for them.
    fn enqueue(self: &Arc<Self>, demand: Vec<Job>, ahead: Vec<Job>) {
        if demand.is_empty() && ahead.is_empty() {
            return;
        }
        let mut q = lock(&self.queue);
        q.demand.extend(demand);
        q.ahead.extend(ahead);
        let queued = q.demand.len() + q.ahead.len();
        for _ in 0..q.idle.min(queued) {
            self.work.notify_one();
        }
        let more = queued
            .saturating_sub(q.idle)
            .min(self.max_workers.saturating_sub(q.workers));
        for _ in 0..more {
            let s = self.clone();
            q.workers += 1;
            let spawned = std::thread::Builder::new()
                .name("omnizip-decode".into())
                .spawn(move || worker(s));
            if spawned.is_err() {
                q.workers -= 1;
                break;
            }
        }
        if q.workers == 0 {
            // No thread to decode on: fail the reads rather than leave them waiting.
            tracing::error!("mount: no decoding thread could be started");
            let left: Vec<Job> = q.take_all();
            drop(q);
            for j in left {
                self.fail(j.targets, FsError::Io);
            }
        }
    }

    /// Ends the decodes of `targets` not delivered yet with `err`, and forgets them all.
    fn fail(&self, targets: Vec<(EntryId, Arc<Partial>)>, err: FsError) {
        for (id, p) in targets {
            p.end(Err(err));
            self.retire(id, &p);
        }
    }

    /// Forgets the decode of `id` once it has ended (its readers keep `p`).
    fn retire(&self, id: EntryId, p: &Arc<Partial>) {
        let mut pending = lock(&self.pending);
        if pending.get(&id).is_some_and(|x| Arc::ptr_eq(&x.partial, p)) {
            pending.remove(&id);
        }
    }

    /// Runs one pass on the worker's reader (or the shared one).
    fn run(&self, job: Job, reader: &mut Option<Box<dyn ArchiveReader>>) {
        for (_, p) in &job.targets {
            p.start();
        }
        let partials: HashMap<EntryId, Arc<Partial>> = job.targets.iter().cloned().collect();
        let ctx = self.ctx();
        // A decoder may panic on corrupt data: the readers then get an error instead of waiting
        // for an end that never comes.
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let mut sink = CacheSink {
                cache: &self.cache,
                targets: partials.keys().copied().collect(),
                partials: &partials,
                pool: Some(self),
                progress: &job.progress,
                archive_size: self.archive_size,
            };
            match reader.as_mut() {
                Some(r) => decode(r.as_mut(), &mut sink, &ctx),
                None => decode(lock(&self.primary).as_mut(), &mut sink, &ctx),
            }
        }));
        // The sink ends each entry with its content; whatever stopped short ends here.
        let err = match outcome {
            Ok(Ok(())) => FsError::Io,
            Ok(Err(e)) => e,
            Err(_) => {
                tracing::error!("mount: the decoder panicked");
                // Its state is unknown: the next pass gets a fresh clone.
                *reader = None;
                FsError::Io
            }
        };
        self.fail(job.targets, err);
    }
}

/// A worker: runs passes, the ones reads wait for first, until the volume goes away or there
/// has been nothing to do for a while.
fn worker(s: Arc<Shared>) {
    let mut reader: Option<Box<dyn ArchiveReader>> = None;
    let mut q = lock(&s.queue);
    loop {
        if s.cancel.is_cancelled() {
            break;
        }
        if let Some(job) = q.demand.pop_front().or_else(|| q.ahead.pop_front()) {
            drop(q);
            if s.clones && reader.is_none() {
                reader = lock(&s.spare)
                    .pop()
                    .or_else(|| lock(&s.primary).try_clone());
            }
            s.run(job, &mut reader);
            q = lock(&s.queue);
            continue;
        }
        q.idle += 1;
        let (g, waited) = s
            .work
            .wait_timeout(q, IDLE_EXIT)
            .unwrap_or_else(|e| e.into_inner());
        q = g;
        q.idle -= 1;
        if waited.timed_out() && q.demand.is_empty() && q.ahead.is_empty() {
            break;
        }
    }
    q.workers -= 1;
    let left: Vec<Job> = if s.cancel.is_cancelled() {
        q.take_all()
    } else {
        Vec::new()
    };
    drop(q);
    for j in left {
        s.fail(j.targets, FsError::Interrupted);
    }
    if let Some(r) = reader {
        lock(&s.spare).push(r);
    }
}

/// Decodes what `sink` wants into the cache.
pub(crate) fn decode(
    reader: &mut dyn ArchiveReader,
    sink: &mut CacheSink<'_>,
    ctx: &JobCtx,
) -> FsResult<()> {
    match reader.extract_to(sink, ctx) {
        Ok(()) => Ok(()),
        Err(e) => {
            if e.code() != ErrorCode::Cancelled {
                tracing::warn!(code = e.code().id(), "mount: decoding failed");
            }
            Err(e.into())
        }
    }
}

/// Receives decoded entries into the cache.
pub(crate) struct CacheSink<'a> {
    pub cache: &'a ContentCache,
    /// Entries still to come.
    pub targets: HashSet<EntryId>,
    /// The decodes their readers wait on.
    pub partials: &'a HashMap<EntryId, Arc<Partial>>,
    /// Forgets each decode as it ends.
    pool: Option<&'a Shared>,
    pub progress: &'a Progress,
    /// For the expansion-ratio limit.
    pub archive_size: u64,
}

impl<'a> CacheSink<'a> {
    /// A sink decoding `targets` with nobody waiting on them.
    pub fn detached(
        cache: &'a ContentCache,
        targets: HashSet<EntryId>,
        partials: &'a HashMap<EntryId, Arc<Partial>>,
        progress: &'a Progress,
        archive_size: u64,
    ) -> Self {
        Self {
            cache,
            targets,
            partials,
            pool: None,
            progress,
            archive_size,
        }
    }
}

/// Copies an entry in chunks of up to 1 MiB (no larger than the entry), checking for
/// cancellation between them and counting what is produced.
fn copy(
    r: &mut dyn io::Read,
    w: &mut ContentBuilder,
    size: Option<u64>,
    progress: &Progress,
    ctx: &JobCtx,
) -> io::Result<()> {
    let chunk = size.map_or(1 << 20, |s| (s + 1).clamp(8 << 10, 1 << 20));
    let mut buf = vec![0; chunk as usize];
    loop {
        if ctx.check().is_err() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        match r.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                w.write_all(&buf[..n])?;
                progress.add(n as u64);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

impl ExtractSink for CacheSink<'_> {
    fn wants(&self, entry: &Entry) -> bool {
        self.targets.contains(&entry.id)
    }

    fn wanted(&self) -> Option<Vec<EntryId>> {
        Some(self.targets.iter().copied().collect())
    }

    fn entry(
        &mut self,
        entry: &Entry,
        data: EntryData<'_>,
        ctx: &JobCtx,
    ) -> omnizip_core::Result<()> {
        ctx.check()?;
        self.targets.remove(&entry.id);
        let cache = self.cache;
        let partial = self.partials.get(&entry.id);
        let limits = cache.limits();
        // Never more than declared, and never past the expansion ratio extraction would ask
        // about (bomb guard: the declared size comes from the archive too, and a mounted volume
        // is read by thumbnailers and virus scanners as soon as it is browsed). Unknown sizes
        // are also bounded by the disk budget.
        let declared = if entry.size_known {
            entry.size
        } else {
            limits.disk
        };
        let cap = declared.min(ctx.policy.max_unpacked(self.archive_size));
        let oversize = |e: io::Error| {
            if ContentBuilder::is_oversize(&e) {
                Error::new(ErrorCode::CompressionBomb).with("path", &entry.path)
            } else {
                Error::from(e)
            }
        };
        // A large entry is spooled from its first byte, which its readers get as it comes.
        let stream = partial.filter(|_| entry.size_known && entry.size > limits.inline_max);
        let content = match data {
            EntryData::Stream(r) => {
                let mut b = match stream {
                    Some(p) => cache.streaming_builder(cap, p.clone()),
                    None => cache.builder(cap),
                };
                let size = entry.size_known.then_some(entry.size);
                if let Err(e) = copy(r, &mut b, size, self.progress, ctx) {
                    ctx.check()?;
                    return Err(oversize(e));
                }
                b.finish()?
            }
            EntryData::WriteTo(w) => {
                let path = tempfile::Builder::new()
                    .prefix("entry-")
                    .tempfile_in(cache.dir())?
                    .into_temp_path();
                self.progress.writing(Some(&path));
                let written = w(&path);
                self.progress.writing(None);
                written?;
                let content =
                    ContentBuilder::adopt(path, cap, limits.inline_max).map_err(oversize)?;
                self.progress.add(content.len());
                content
            }
            EntryData::None => Content::Mem(Arc::from(&[][..])),
        };
        let content = cache.insert(entry.id, content);
        if let Some(p) = partial {
            p.end(Ok(content));
            if let Some(pool) = self.pool {
                pool.retire(entry.id, p);
            }
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.targets.is_empty()
    }
}
