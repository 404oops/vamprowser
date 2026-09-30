//! Serialized background work with a single replaceable pending job.
//! Bursts cannot grow a queue, and an older write cannot finish after a
//! newer one. Flush only when shutting down or before exporting data.

use std::sync::{Arc, Condvar, Mutex};

struct State<T> {
    pending: Option<T>,
    active: bool,
    closed: bool,
    work: Option<Box<dyn FnMut(T) + Send>>,
}

pub(crate) struct LatestWorker<T> {
    shared: Arc<(Mutex<State<T>>, Condvar)>,
    name: String,
}

impl<T: Send + 'static> LatestWorker<T> {
    pub(crate) fn new(name: &str, work: impl FnMut(T) + Send + 'static) -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(State {
                    pending: None,
                    active: false,
                    closed: false,
                    work: Some(Box::new(work)),
                }),
                Condvar::new(),
            )),
            name: name.to_owned(),
        }
    }

    pub(crate) fn submit(&self, job: T) {
        let (lock, changed) = &*self.shared;
        let mut state = lock.lock().expect("background queue");
        state.pending = Some(job);
        let Some(mut work) = state.work.take() else {
            changed.notify_all();
            return;
        };
        drop(state);
        // No thread or reserved stack until the first job actually needs it.
        let worker = self.shared.clone();
        std::thread::Builder::new()
            .name(self.name.clone())
            .spawn(move || {
                let (lock, changed) = &*worker;
                loop {
                    let job = {
                        let mut state = lock.lock().expect("background queue");
                        while state.pending.is_none() && !state.closed {
                            state = changed.wait(state).expect("background queue");
                        }
                        let Some(job) = state.pending.take() else {
                            break;
                        };
                        state.active = true;
                        job
                    };
                    // A failed job must not strand flush waiters or stop
                    // subsequent writes. I/O errors are handled by the job;
                    // this covers an unexpected panic in background work.
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(job))).is_err()
                    {
                        eprintln!(
                            "Background job panicked; continuing with the latest pending job"
                        );
                    }
                    let mut state = lock.lock().expect("background queue");
                    state.active = false;
                    changed.notify_all();
                }
            })
            .expect("background worker");
    }

    pub(crate) fn flush(&self) {
        let (lock, changed) = &*self.shared;
        let mut state = lock.lock().expect("background queue");
        while state.active || state.pending.is_some() {
            state = changed.wait(state).expect("background queue");
        }
    }
}

impl<T> Drop for LatestWorker<T> {
    fn drop(&mut self) {
        let (lock, changed) = &*self.shared;
        let mut state = lock.lock().expect("background queue");
        state.closed = true;
        changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::LatestWorker;
    use std::sync::{Arc, Mutex, mpsc};

    #[test]
    fn bursts_keep_only_the_latest_pending_job_and_flush_waits_for_it() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let saved = output.clone();
        let (started, receive_started) = mpsc::channel();
        let (release, wait_release) = mpsc::channel();
        let worker = LatestWorker::new("test-latest", move |job| {
            if job == 1 {
                started.send(()).unwrap();
                wait_release.recv().unwrap();
            }
            saved.lock().unwrap().push(job);
        });
        worker.submit(1);
        receive_started.recv().unwrap();
        worker.submit(2);
        worker.submit(3);
        release.send(()).unwrap();
        worker.flush();
        assert_eq!(*output.lock().unwrap(), [1, 3]);
        worker.submit(4);
        worker.flush();
        assert_eq!(*output.lock().unwrap(), [1, 3, 4]);
    }

    #[test]
    fn a_panicking_job_does_not_strand_flush_or_later_jobs() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let saved = output.clone();
        let worker = LatestWorker::new("test-panic", move |job| {
            if job == 1 {
                panic!("failed job");
            }
            saved.lock().unwrap().push(job);
        });
        worker.submit(1);
        worker.flush();
        worker.submit(2);
        worker.flush();
        assert_eq!(*output.lock().unwrap(), [2]);
    }
}
