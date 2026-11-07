//! Work too slow for a frame (making a big list's rows, a ruleset's
//! playlists), done on a thread of its own. Jobs have keys: of those asked
//! for under one key, only the latest is done, the ones before it being
//! out of date by then.

use std::sync::mpsc;

use eframe::egui;

/// A thread doing `J`s, giving back `O`s, by key `K`.
pub struct Latest<K, J, O> {
    jobs: mpsc::Sender<(K, J, egui::Context)>,
    done: mpsc::Receiver<(K, O)>,
}

impl<K: PartialEq + Clone + Send + 'static, J: Send + 'static, O: Send + 'static> Latest<K, J, O> {
    /// A thread called `name` doing jobs with `work`. (It ends with this.)
    pub fn new(name: &str, work: fn(J) -> O) -> Self {
        let (jobs, asked) = mpsc::channel::<(K, J, egui::Context)>();
        let (finished, done) = mpsc::channel();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                // The jobs waiting, oldest first, one for each key.
                let mut waiting: Vec<(K, J, egui::Context)> = Vec::new();
                loop {
                    if waiting.is_empty() {
                        let Ok(job) = asked.recv() else { return };
                        waiting.push(job);
                    }
                    for job in asked.try_iter() {
                        match waiting.iter_mut().find(|(key, ..)| *key == job.0) {
                            Some(older) => *older = job,
                            None => waiting.push(job),
                        }
                    }
                    let (key, job, ctx) = waiting.remove(0);
                    if finished.send((key, work(job))).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            })
            .unwrap_or_else(|e| panic!("starting the {name} thread: {e}"));
        Self { jobs, done }
    }

    /// Asks for `job` under `key`, repainting `ctx` when it's done.
    pub fn ask(&self, key: K, job: J, ctx: &egui::Context) {
        // (The thread only ends with `self`.)
        let _ = self.jobs.send((key, job, ctx.clone()));
    }

    /// The jobs done since this was last called, oldest first.
    pub fn take(&self) -> impl Iterator<Item = (K, O)> + '_ {
        self.done.try_iter()
    }

    /// The next job done, waiting up to `timeout` for it.
    #[cfg(test)]
    pub fn wait(&self, timeout: std::time::Duration) -> Option<(K, O)> {
        self.done.recv_timeout(timeout).ok()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn only_the_latest_job_under_a_key_is_done() {
        let ctx = egui::Context::default();
        // (The first keeps the thread busy while the rest are asked for.)
        let latest: Latest<&str, u64, u64> = Latest::new("test", |n| {
            std::thread::sleep(Duration::from_millis(n));
            n
        });
        latest.ask("busy", 300, &ctx);
        std::thread::sleep(Duration::from_millis(50));
        for (key, n) in [("a", 1), ("a", 2), ("b", 3), ("a", 4)] {
            latest.ask(key, n, &ctx);
        }
        let done: Vec<_> = (0..3).map(|_| latest.wait(Duration::from_secs(5)).expect("the jobs done")).collect();
        assert_eq!(done, [("busy", 300), ("a", 4), ("b", 3)], "a's latest, in a's place, then b");
        assert!(latest.wait(Duration::from_millis(100)).is_none(), "nothing else done");
    }
}
