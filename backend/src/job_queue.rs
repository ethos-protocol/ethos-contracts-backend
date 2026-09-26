//! Asynchronous job processing system for long-running operations.
//!
//! Provides a job queue that enables background task execution without
//! blocking API requests. Clients can submit jobs and poll for results.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Job execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

/// A background job with metadata and execution state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateJobRequest {
    pub job_type: String,
    pub payload: serde_json::Value,
}

#[derive(Serialize)]
pub struct JobResponse {
    pub job: Job,
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Job>,
}

/// Shared job queue state managing background tasks.
#[derive(Default)]
pub struct JobQueue {
    inner: RwLock<Inner>,
}

impl JobQueue {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Submit a new job to the queue. Returns the job ID.
    pub fn submit_job(&self, job_type: &str, payload: &serde_json::Value) -> String {
        let job_id = Uuid::new_v4().to_string();
        let job = Job {
            id: job_id.clone(),
            status: JobStatus::Pending,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            result: None,
            error: None,
        };

        self.inner
            .write()
            .expect("job queue lock poisoned")
            .jobs
            .insert(job_id.clone(), job);

        job_id
    }

    /// Get job status by ID.
    pub fn get_job(&self, job_id: &str) -> Option<Job> {
        self.inner
            .read()
            .expect("job queue lock poisoned")
            .jobs
            .get(job_id)
            .cloned()
    }

    /// Mark a job as running.
    pub fn start_job(&self, job_id: &str) -> bool {
        let mut inner = self.inner.write().expect("job queue lock poisoned");
        if let Some(job) = inner.jobs.get_mut(job_id) {
            if job.status == JobStatus::Pending {
                job.status = JobStatus::Running;
                job.started_at = Some(Utc::now());
                return true;
            }
        }
        false
    }

    /// Complete a job with a result.
    pub fn complete_job(&self, job_id: &str, result: String) -> bool {
        let mut inner = self.inner.write().expect("job queue lock poisoned");
        if let Some(job) = inner.jobs.get_mut(job_id) {
            if job.status == JobStatus::Running {
                job.status = JobStatus::Completed;
                job.completed_at = Some(Utc::now());
                job.result = Some(result);
                return true;
            }
        }
        false
    }

    /// Fail a job with an error message.
    pub fn fail_job(&self, job_id: &str, error: String) -> bool {
        let mut inner = self.inner.write().expect("job queue lock poisoned");
        if let Some(job) = inner.jobs.get_mut(job_id) {
            if job.status == JobStatus::Running || job.status == JobStatus::Pending {
                job.status = JobStatus::Failed;
                job.completed_at = Some(Utc::now());
                job.error = Some(error);
                return true;
            }
        }
        false
    }

    pub fn list_jobs(&self) -> Vec<Job> {
        self.inner
            .read()
            .expect("job queue lock poisoned")
            .jobs
            .values()
            .cloned()
            .collect()
    }
}

/// `POST /jobs` - submit a new long-running job.
pub async fn create_job(
    State(queue): State<Arc<JobQueue>>,
    Json(req): Json<CreateJobRequest>,
) -> impl IntoResponse {
    let job_id = queue.submit_job(&req.job_type, &req.payload);
    let job = queue
        .get_job(&job_id)
        .expect("job just created, must exist");
    (StatusCode::CREATED, Json(JobResponse { job }))
}

/// `GET /jobs/:id` - retrieve job status and result.
pub async fn get_job(
    State(queue): State<Arc<JobQueue>>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    match queue.get_job(&job_id) {
        Some(job) => (StatusCode::OK, Json(JobResponse { job })).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /jobs` - list all jobs.
pub async fn list_jobs(State(queue): State<Arc<JobQueue>>) -> impl IntoResponse {
    let jobs = queue.list_jobs();
    Json(serde_json::json!({ "jobs": jobs }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_creates_pending_job() {
        let queue = JobQueue::default();
        let job_id = queue.submit_job("backup", &serde_json::json!({}));

        let job = queue.get_job(&job_id).expect("job should exist");
        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(job.id, job_id);
        assert!(job.started_at.is_none());
    }

    #[test]
    fn start_transitions_pending_to_running() {
        let queue = JobQueue::default();
        let job_id = queue.submit_job("backup", &serde_json::json!({}));

        assert!(queue.start_job(&job_id));
        let job = queue.get_job(&job_id).expect("job should exist");
        assert_eq!(job.status, JobStatus::Running);
        assert!(job.started_at.is_some());
    }

    #[test]
    fn complete_finishes_running_job() {
        let queue = JobQueue::default();
        let job_id = queue.submit_job("backup", &serde_json::json!({}));
        queue.start_job(&job_id);

        assert!(queue.complete_job(&job_id, "backup completed".to_string()));
        let job = queue.get_job(&job_id).expect("job should exist");
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.result, Some("backup completed".to_string()));
        assert!(job.completed_at.is_some());
    }

    #[test]
    fn fail_marks_job_as_failed() {
        let queue = JobQueue::default();
        let job_id = queue.submit_job("backup", &serde_json::json!({}));
        queue.start_job(&job_id);

        assert!(queue.fail_job(&job_id, "disk full".to_string()));
        let job = queue.get_job(&job_id).expect("job should exist");
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error, Some("disk full".to_string()));
    }

    #[test]
    fn cannot_start_running_job() {
        let queue = JobQueue::default();
        let job_id = queue.submit_job("backup", &serde_json::json!({}));
        queue.start_job(&job_id);

        assert!(!queue.start_job(&job_id), "cannot re-start running job");
    }

    #[test]
    fn cannot_complete_pending_job() {
        let queue = JobQueue::default();
        let job_id = queue.submit_job("backup", &serde_json::json!({}));

        assert!(
            !queue.complete_job(&job_id, "done".to_string()),
            "cannot complete pending job"
        );
    }

    #[test]
    fn list_includes_all_submitted_jobs() {
        let queue = JobQueue::default();
        let id1 = queue.submit_job("backup", &serde_json::json!({}));
        let id2 = queue.submit_job("restore", &serde_json::json!({}));

        let jobs = queue.list_jobs();
        assert_eq!(jobs.len(), 2);
        let ids: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }
}
