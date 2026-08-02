use chrono::{DateTime, Utc};
use jwc_crawler::worker::{WorkerJob, WorkerStatus};

#[test]
fn worker_job_accepts_only_versioned_bounded_fields() {
    let payload = r#"{
        "schema_version": 1,
        "job_id": "job-demo-1",
        "source_id": "seu-jwc",
        "created_at": "2026-07-28T12:00:00Z",
        "deadline_at": "2026-07-28T12:00:08Z",
        "date_after": "2026-07-01",
        "max_pages_per_category": 2,
        "max_detail_fetches": 20,
        "max_total_http_requests": 40,
        "with_contents_only": true
    }"#;

    let job: WorkerJob = serde_json::from_str(payload).unwrap();

    assert_eq!(job.schema_version, 1);
    assert_eq!(job.job_id, "job-demo-1");
    assert_eq!(job.max_pages_per_category, 2);
    let before_deadline = DateTime::parse_from_rfc3339("2026-07-28T12:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    assert!(job.validate(before_deadline).is_ok());
}

#[test]
fn worker_job_rejects_unknown_fields_expiry_and_unbounded_values() {
    let unknown = r#"{
        "schema_version":1,"job_id":"x","source_id":"seu-jwc",
        "created_at":"2026-07-28T12:00:00Z","deadline_at":"2026-07-28T12:00:08Z",
        "date_after":null,"max_pages_per_category":2,"max_detail_fetches":20,
        "max_total_http_requests":40,"with_contents_only":true,
        "url":"https://evil.example"
    }"#;
    assert!(serde_json::from_str::<WorkerJob>(unknown).is_err());

    let expired = serde_json::from_str::<WorkerJob>(
        &unknown.replace(",\n        \"url\":\"https://evil.example\"", ""),
    )
    .unwrap();
    let now = DateTime::parse_from_rfc3339("2026-07-28T12:00:09Z")
        .unwrap()
        .with_timezone(&Utc);
    assert!(expired.validate(now).is_err());
}

#[test]
fn worker_status_serializes_stable_codes_without_raw_errors() {
    let status = WorkerStatus::failed("job-demo-1", "seu-jwc", "CRAWL_DEADLINE_EXCEEDED");
    let value = serde_json::to_value(status).unwrap();

    assert_eq!(value["status"], "failed");
    assert_eq!(value["warning_codes"][0], "CRAWL_DEADLINE_EXCEEDED");
    assert!(value.get("message").is_none());
    assert!(value.get("error").is_none());
}
