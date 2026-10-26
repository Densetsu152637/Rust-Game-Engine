use crate::{MlError, Model, Result, Tensor};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendStatus {
    pub cpu_available: bool,
    pub gpu_available: bool,
    pub gpu_reason: &'static str,
}
pub fn backend_status() -> BackendStatus {
    BackendStatus {
        cpu_available: true,
        gpu_available: false,
        gpu_reason: "GPU inference is not implemented; tract CPU is the only execution backend",
    }
}
#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfig {
    pub workers: usize,
    pub max_jobs: usize,
}
impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            workers: 1,
            max_jobs: 16,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// Process-wide unique identifier, including across runtime replacement.
pub struct JobId(u64);
static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(0);
fn fresh_job_id() -> Result<JobId> {
    NEXT_JOB_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1)
        })
        .map(JobId)
        .map_err(|_| MlError::new("ml.job.exhausted", "process-wide job identifiers exhausted"))
}
#[derive(Debug)]
pub struct Completion {
    pub id: JobId,
    pub result: Result<Vec<Tensor>>,
}
struct Job {
    model: Arc<Model>,
    inputs: Vec<Tensor>,
    cancelled: Arc<AtomicBool>,
    result: mpsc::Sender<Result<Vec<Tensor>>>,
}
struct Pending {
    cancelled: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Vec<Tensor>>>,
}
/// Bounded outstanding jobs, including unpolled completions. Drop cancels queued jobs
/// and joins workers; a running tract call cannot be forcibly interrupted.
pub struct InferenceRuntime {
    sender: Option<mpsc::SyncSender<Job>>,
    workers: Vec<JoinHandle<()>>,
    jobs: BTreeMap<JobId, Pending>,
    max_jobs: usize,
}
fn cancelled() -> MlError {
    MlError::new("ml.job.cancelled", "inference job cancelled")
}
impl InferenceRuntime {
    pub fn new(config: RuntimeConfig) -> Result<Self> {
        if !(1..=32).contains(&config.workers) || !(1..=1024).contains(&config.max_jobs) {
            return Err(MlError::new(
                "ml.runtime.config",
                "workers must be 1..=32 and max_jobs 1..=1024",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel::<Job>(config.max_jobs);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut runtime = Self {
            sender: Some(sender),
            workers: Vec::new(),
            jobs: BTreeMap::new(),
            max_jobs: config.max_jobs,
        };
        for index in 0..config.workers {
            let receiver = receiver.clone();
            let worker = thread::Builder::new()
                .name(format!("engine-ml-{index}"))
                .spawn(move || {
                    loop {
                        let job = match receiver.lock() {
                            Ok(receiver) => receiver.recv(),
                            Err(_) => return,
                        };
                        let Ok(job) = job else { return };
                        let result = if job.cancelled.load(Ordering::Acquire) {
                            Err(cancelled())
                        } else {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                job.model.run(&job.inputs)
                            }))
                            .unwrap_or_else(|_| {
                                Err(MlError::new("ml.inference.panic", "CPU inference panicked"))
                            })
                        };
                        let _ = job.result.send(result);
                    }
                })
                .map_err(|e| MlError::new("ml.runtime.thread", e.to_string()))?;
            runtime.workers.push(worker);
        }
        Ok(runtime)
    }
    pub fn submit(&mut self, model: Arc<Model>, inputs: Vec<Tensor>) -> Result<JobId> {
        if self.jobs.len() >= self.max_jobs {
            return Err(MlError::new(
                "ml.runtime.full",
                "poll completions before submitting more jobs",
            ));
        }
        model.validate_inputs(&inputs)?;
        let id = fresh_job_id()?;
        let (result, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let job = Job {
            model,
            inputs,
            cancelled: cancelled.clone(),
            result,
        };
        self.sender
            .as_ref()
            .ok_or_else(|| MlError::new("ml.runtime.closed", "runtime is closed"))?
            .try_send(job)
            .map_err(|_| MlError::new("ml.runtime.closed", "worker queue unavailable"))?;
        self.jobs.insert(
            id,
            Pending {
                cancelled,
                result: receiver,
            },
        );
        Ok(id)
    }
    /// Cancellation is idempotent until completion is polled. Capacity remains reserved
    /// until the worker acknowledges the job, including when inference is already running.
    pub fn cancel(&mut self, id: JobId) -> Result<()> {
        let job = self
            .jobs
            .get(&id)
            .ok_or_else(|| MlError::new("ml.job.unknown", "unknown or consumed job"))?;
        job.cancelled.store(true, Ordering::Release);
        Ok(())
    }
    /// Returns one completion without waiting. Each job is delivered exactly once.
    pub fn poll(&mut self) -> Option<Completion> {
        let completed = self
            .jobs
            .iter()
            .find_map(|(id, job)| match job.result.try_recv() {
                Ok(result) => Some((*id, result)),
                Err(mpsc::TryRecvError::Disconnected) => Some((
                    *id,
                    Err(MlError::new("ml.runtime.worker", "worker disconnected")),
                )),
                Err(mpsc::TryRecvError::Empty) => None,
            });
        let (id, mut result) = completed?;
        let job = self.jobs.remove(&id)?;
        if job.cancelled.load(Ordering::Acquire) {
            result = Err(cancelled());
        }
        Some(Completion { id, result })
    }
    pub fn outstanding(&self) -> usize {
        self.jobs.len()
    }
}
impl Drop for InferenceRuntime {
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.cancelled.store(true, Ordering::Release);
        }
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
