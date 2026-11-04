//! Work too slow for a frame (making a big list's rows), done on a thread
//! of its own: only the latest job asked for is done, those asked for
//! while one was under way being out of date by then.

use std::sync::mpsc;

use eframe::egui;

/// A thread doing `J`s, giving back `O`s.
pub struct Latest<J, O> {
    jobs: mpsc::Sender<(J, egui::Context)>,
    done: mpsc::Receiver<O>,
}

impl<J: Send + 'static, O: Send + 'static> Latest<J, O> {
    /// A thread called `name` doing jobs with `work`. (It ends with this.)
    pub fn new(name: &str, work: fn(J) -> O) -> Self {
        let (jobs, waiting) = mpsc::channel::<(J, egui::Context)>();
        let (finished, done) = mpsc::channel();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                while let Ok(mut job) = waiting.recv() {
                    while let Ok(newer) = waiting.try_recv() {
                        job = newer;
                    }
                    let (job, ctx) = job;
                    if finished.send(work(job)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            })
            .unwrap_or_else(|e| panic!("starting the {name} thread: {e}"));
        Self { jobs, done }
    }

    /// Asks for `job`, repainting `ctx` when it's done.
    pub fn ask(&self, job: J, ctx: &egui::Context) {
        // (The thread only ends with `self`.)
        let _ = self.jobs.send((job, ctx.clone()));
    }

    /// The latest job done since this was last called, if any.
    pub fn take(&self) -> Option<O> {
        self.done.try_iter().last()
    }

    /// The next job done, waiting up to `timeout` for it.
    #[cfg(test)]
    pub fn wait(&self, timeout: std::time::Duration) -> Option<O> {
        self.done.recv_timeout(timeout).ok()
    }
}
