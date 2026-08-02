use jwc_crawler::hardening::{
    CrawlBudget, TargetPolicy, atomic_write_json, atomic_write_json_limited,
};
use serde_json::json;
use std::fs;
use std::os::unix::fs::symlink;
use url::Url;

#[test]
fn request_budgets_are_hard_limits() {
    let mut budget = CrawlBudget::new(2, 1, 3).unwrap();

    budget.record_list_page("jwxx").unwrap();
    budget.record_list_page("jwxx").unwrap();
    assert!(budget.record_list_page("jwxx").is_err());
    budget.record_detail().unwrap();
    assert!(budget.record_detail().is_err());
    assert_eq!(budget.total_http_requests(), 3);
}

#[test]
fn jwc_policy_rejects_unregistered_targets_and_credentials() {
    let policy = TargetPolicy::jwc();

    assert!(
        policy
            .authorize(&Url::parse("https://jwc.seu.edu.cn/jwxx/list.htm").unwrap())
            .is_ok()
    );
    assert!(
        policy
            .authorize(
                &Url::parse("https://jwc.seu.edu.cn/2026/0728/c21676a600001/page.htm").unwrap()
            )
            .is_ok()
    );
    assert!(
        policy
            .authorize(&Url::parse("https://example.com/jwxx/list.htm").unwrap())
            .is_err()
    );
    assert!(
        policy
            .authorize(&Url::parse("https://user:secret@jwc.seu.edu.cn/jwxx/list.htm").unwrap())
            .is_err()
    );
    assert!(
        policy
            .authorize(&Url::parse("https://jwc.seu.edu.cn/../../admin").unwrap())
            .is_err()
    );
    assert!(
        policy
            .authorize_attachment_reference(
                &Url::parse("https://jwc.seu.edu.cn/_upload/file/demo.pdf").unwrap()
            )
            .is_ok()
    );
}

#[test]
fn output_is_replaced_atomically_without_leaving_temp_file() {
    let root = std::env::temp_dir().join(format!("jwc-crawler-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let output = root.join("result.json");
    fs::write(&output, "old").unwrap();

    atomic_write_json(&output, &json!({"status": "complete"})).unwrap();

    let written: serde_json::Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(written["status"], "complete");
    assert!(!output.with_extension("json.tmp").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_output_enforces_streaming_size_limit_and_cleans_temp() {
    let root = std::env::temp_dir().join(format!("jwc-size-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let output = root.join("result.json");

    assert!(atomic_write_json_limited(&output, &"x".repeat(1024), 32).is_err());
    assert!(!output.exists());
    assert!(!output.with_extension("json.tmp").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_output_refuses_precreated_temp_symlink() {
    let root = std::env::temp_dir().join(format!("jwc-symlink-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let output = root.join("result.json");
    let victim = root.join("victim.txt");
    fs::write(&victim, "safe").unwrap();
    symlink(&victim, output.with_extension("json.tmp")).unwrap();

    assert!(atomic_write_json(&output, &json!({"unsafe": true})).is_err());
    assert_eq!(fs::read_to_string(&victim).unwrap(), "safe");
    fs::remove_dir_all(root).unwrap();
}
