//! Gestor de descargas con hilos: progreso, pausa, reanudación y cancelado.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

const CHUNK: usize = 256 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobStatus {
    Queued,
    Downloading,
    Paused,
    Done,
    Error,
    Cancelled,
}

impl JobStatus {
    pub fn label(&self) -> String {
        let lang = &crate::cur_lang();
        crate::tr(
            lang,
            match self {
                JobStatus::Queued => "en cola",
                JobStatus::Downloading => "descargando",
                JobStatus::Paused => "pausada",
                JobStatus::Done => "hecha",
                JobStatus::Error => "error",
                JobStatus::Cancelled => "cancelada",
            },
        )
    }
}

pub struct JobSnapshot {
    pub id: String,
    pub name: String,
    pub done: u64,
    pub total: u64,
    pub status: JobStatus,
    pub error: String,
    pub dest: PathBuf,
}

struct JobInner {
    id: String,
    name: String,
    url: String,
    dest: PathBuf,
    total: Mutex<u64>,
    done: Mutex<u64>,
    status: Mutex<JobStatus>,
    error: Mutex<String>,
    paused: AtomicBool,
    cancelled: AtomicBool,
}

pub struct DownloadManager {
    jobs: Mutex<HashMap<String, Arc<JobInner>>>,
    order: Mutex<Vec<String>>,
    client: reqwest::blocking::Client,
    next: Mutex<u64>,
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            order: Mutex::new(vec![]),
            client: reqwest::blocking::Client::builder()
                .timeout(None)
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
                .build()
                .expect("http client"),
            next: Mutex::new(0),
        }
    }

    pub fn add(&self, this: &Arc<Self>, name: String, url: String, dest: PathBuf) -> String {
        let mut n = self.next.lock().unwrap();
        *n += 1;
        let id = format!("j{n}");
        let job = Arc::new(JobInner {
            id: id.clone(),
            name,
            url,
            dest,
            total: Mutex::new(0),
            done: Mutex::new(0),
            status: Mutex::new(JobStatus::Queued),
            error: Mutex::new(String::new()),
            paused: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        });
        self.jobs.lock().unwrap().insert(id.clone(), job.clone());
        self.order.lock().unwrap().push(id.clone());
        let mgr = Arc::clone(this);
        std::thread::spawn(move || Self::work(&mgr, job));
        id
    }

    pub fn pause(&self, id: &str) {
        if let Some(j) = self.jobs.lock().unwrap().get(id) {
            if *j.status.lock().unwrap() == JobStatus::Downloading {
                j.paused.store(true, Ordering::SeqCst);
                *j.status.lock().unwrap() = JobStatus::Paused;
            }
        }
    }

    /// Relanza el worker de un trabajo pausado/con error (reanuda por Range).
    pub fn resume_with(this: &Arc<Self>, id: &str) {
        let job = this.jobs.lock().unwrap().get(id).cloned();
        if let Some(j) = job {
            let st = *j.status.lock().unwrap();
            if st == JobStatus::Paused || st == JobStatus::Error {
                j.paused.store(false, Ordering::SeqCst);
                j.cancelled.store(false, Ordering::SeqCst);
                *j.status.lock().unwrap() = JobStatus::Queued;
                let mgr = Arc::clone(this);
                std::thread::spawn(move || Self::work(&mgr, j));
            }
        }
    }

    pub fn cancel(&self, id: &str) {
        if let Some(j) = self.jobs.lock().unwrap().get(id) {
            j.cancelled.store(true, Ordering::SeqCst);
            j.paused.store(false, Ordering::SeqCst);
            let st = *j.status.lock().unwrap();
            if matches!(
                st,
                JobStatus::Queued | JobStatus::Downloading | JobStatus::Paused | JobStatus::Error
            ) {
                *j.status.lock().unwrap() = JobStatus::Cancelled;
            }
        }
    }

    pub fn snapshot(&self) -> Vec<JobSnapshot> {
        let jobs = self.jobs.lock().unwrap();
        let order = self.order.lock().unwrap();
        order
            .iter()
            .filter_map(|id| jobs.get(id))
            .map(|j| JobSnapshot {
                id: j.id.clone(),
                name: j.name.clone(),
                done: *j.done.lock().unwrap(),
                total: *j.total.lock().unwrap(),
                status: *j.status.lock().unwrap(),
                error: j.error.lock().unwrap().clone(),
                dest: j.dest.clone(),
            })
            .collect()
    }

    fn work(mgr: &Arc<Self>, job: Arc<JobInner>) {
        if job.cancelled.load(Ordering::SeqCst) {
            return;
        }
        *job.status.lock().unwrap() = JobStatus::Downloading;
        match Self::fetch(&mgr.client, &job) {
            Ok(()) => {
                *job.status.lock().unwrap() = JobStatus::Done;
            }
            Err(FetchFail::Paused) => {
                if job.cancelled.load(Ordering::SeqCst) {
                    *job.status.lock().unwrap() = JobStatus::Cancelled;
                } else {
                    *job.status.lock().unwrap() = JobStatus::Paused;
                }
            }
            Err(FetchFail::Cancelled) => {
                *job.status.lock().unwrap() = JobStatus::Cancelled;
            }
            Err(FetchFail::Error(e)) => {
                *job.error.lock().unwrap() = e;
                *job.status.lock().unwrap() = JobStatus::Error;
            }
        }
    }

    fn fetch(client: &reqwest::blocking::Client, job: &Arc<JobInner>) -> Result<(), FetchFail> {
        use std::io::Read;
        if let Some(p) = job.dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| FetchFail::Error(e.to_string()))?;
        }
        let mut pb = job.dest.clone().into_os_string();
        pb.push(".part");
        let part = PathBuf::from(pb);
        let have: u64 = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let mut req = client.get(&job.url);
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let mut resp = req.send().map_err(|e| FetchFail::Error(e.to_string()))?;
        if job.cancelled.load(Ordering::SeqCst) {
            return Err(FetchFail::Cancelled);
        }
        if !resp.status().is_success() && resp.status().as_u16() != 206 {
            return Err(FetchFail::Error(format!("HTTP {}", resp.status())));
        }
        let resumed = resp.status().as_u16() == 206 && have > 0;
        let (mut done, total) = if resumed {
            (have, have + resp.content_length().unwrap_or(0))
        } else {
            (0u64, resp.content_length().unwrap_or(0))
        };
        *job.total.lock().unwrap() = total;
        *job.done.lock().unwrap() = done;
        let mut out = if resumed {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&part)
                .map_err(|e| FetchFail::Error(e.to_string()))?
        } else {
            std::fs::File::create(&part).map_err(|e| FetchFail::Error(e.to_string()))?
        };
        let mut buf = vec![0u8; CHUNK];
        loop {
            if job.cancelled.load(Ordering::SeqCst) {
                return Err(FetchFail::Cancelled);
            }
            if job.paused.load(Ordering::SeqCst) {
                return Err(FetchFail::Paused);
            }
            match resp.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    use std::io::Write;
                    out.write_all(&buf[..n])
                        .map_err(|e| FetchFail::Error(e.to_string()))?;
                    done += n as u64;
                    *job.done.lock().unwrap() = done;
                }
                Err(e) => return Err(FetchFail::Error(e.to_string())),
            }
        }
        if job.cancelled.load(Ordering::SeqCst) {
            return Err(FetchFail::Cancelled);
        }
        if job.paused.load(Ordering::SeqCst) {
            return Err(FetchFail::Paused);
        }
        std::fs::rename(&part, &job.dest).map_err(|e| FetchFail::Error(e.to_string()))?;
        Ok(())
    }
}

enum FetchFail {
    Paused,
    Cancelled,
    Error(String),
}
