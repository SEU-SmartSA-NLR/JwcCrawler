use jwc_crawler::worker::{prepare_private_spool, process_ready_job, recover_interrupted_jobs};
use std::fs;

#[test]
fn spool_startup_removes_only_the_stale_shared_rate_temp() {
    let root = std::env::temp_dir().join(format!("jwc-rate-temp-{}", std::process::id()));
    prepare_private_spool(&root).unwrap();
    let stale = root.join(".last_request_at.json.tmp");
    fs::write(&stale, "stale").unwrap();

    prepare_private_spool(&root).unwrap();

    assert!(!stale.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_converts_interrupted_running_job_to_stable_failure() {
    let root = std::env::temp_dir().join(format!("jwc-recovery-{}", std::process::id()));
    prepare_private_spool(&root).unwrap();
    fs::write(root.join("running/interrupted.running"), "{}").unwrap();
    fs::write(root.join("results/interrupted.items.json"), "[]").unwrap();

    assert_eq!(recover_interrupted_jobs(&root).unwrap(), 1);

    assert!(!root.join("running/interrupted.running").exists());
    assert!(!root.join("results/interrupted.items.json").exists());
    let failed = fs::read_to_string(root.join("results/interrupted.failed")).unwrap();
    assert!(failed.contains("WORKER_RESTARTED"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn expired_spool_job_fails_without_creating_items() {
    let root = std::env::temp_dir().join(format!("jwc-spool-test-{}", std::process::id()));
    let requests = root.join("requests");
    fs::create_dir_all(&requests).unwrap();
    let ready = requests.join("expired-job.ready");
    fs::write(
        &ready,
        r#"{
            "schema_version":1,"job_id":"expired-job","source_id":"seu-jwc",
            "created_at":"2026-07-28T12:00:00Z","deadline_at":"2026-07-28T12:00:08Z",
            "date_after":null,"max_pages_per_category":2,"max_detail_fetches":20,
            "max_total_http_requests":40,"with_contents_only":true
        }"#,
    )
    .unwrap();

    process_ready_job(&root, &ready).unwrap();

    assert!(root.join("results/expired-job.failed").is_file());
    assert!(!root.join("results/expired-job.items.json").exists());
    assert!(!ready.exists());
    fs::write(&ready, "{}").unwrap();
    assert!(process_ready_job(&root, &ready).is_err());
    assert!(!ready.exists());
    assert!(root.join("results/expired-job.failed").is_file());
    fs::remove_dir_all(root).unwrap();
}
