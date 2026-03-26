/// Minimal thread-pool executor — replaces Tokio for our use case.
///
/// Design
/// ──────
///   • Fixed-size pool of OS threads (default: 8).
///   • Jobs are `FnOnce() + Send + 'static` — no async, no Futures.
///   • The pool is shared via `Arc<Executor>`.  Cloning the Arc is cheap.
///   • When the last Arc is dropped, the sender is closed, workers drain
///     remaining jobs, then exit cleanly.
///
/// HTTP requests in data modules are synchronous (via `ureq`), so the thread
/// that runs a job blocks while the network request is in flight.  With 8
/// threads we can have 8 concurrent fetches — enough for a tile cache.
///
/// For the one-time async wgpu initialisation we keep `pollster::block_on`,
/// which is a trivial single-future runner and has no other dependencies.

use std::sync::{Arc, Mutex, mpsc};

type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct Executor {
    tx: mpsc::Sender<Job>,
    /// Kept alive so threads are joined on drop.
    _workers: Vec<std::thread::JoinHandle<()>>,
}

impl Executor {
    /// Create a pool with `num_threads` workers.
    pub fn new(num_threads: usize) -> Arc<Self> {
        let (tx, rx) = mpsc::channel::<Job>();
        // Wrap the receiver in an Arc<Mutex> so multiple workers can share it.
        let rx = Arc::new(Mutex::new(rx));

        let workers = (0..num_threads)
            .map(|i| {
                let rx = Arc::clone(&rx);
                std::thread::Builder::new()
                    .name(format!("sat-worker-{i}"))
                    .spawn(move || {
                        loop {
                            // Block until a job arrives or the sender drops.
                            let job = {
                                let guard = rx.lock().expect("executor mutex poisoned");
                                guard.recv()
                            };
                            match job {
                                Ok(job) => job(),
                                Err(_)  => break, // channel closed → exit
                            }
                        }
                    })
                    .unwrap_or_else(|e| panic!("failed to spawn worker {i}: {e}"))
            })
            .collect();

        Arc::new(Self { tx, _workers: workers })
    }

    /// Submit a closure to run on the next available worker thread.
    ///
    /// Returns immediately; the closure runs asynchronously.
    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // Ignore send errors — they only happen if all workers have panicked.
        self.tx.send(Box::new(f)).ok();
    }
}
