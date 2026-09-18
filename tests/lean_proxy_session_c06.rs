mod common;

use openproxy::core::utils::session_manager::resolve_continuation_id;
use uuid::Uuid;

#[tokio::test]
async fn stateless_resolution_survives_large_churn_and_task_cancellation() {
    let stable = resolve_continuation_id("stable", Some("account"), "test-scope", false);
    let mut tasks = Vec::new();
    for worker in 0..32 {
        tasks.push(tokio::spawn(async move {
            for index in 0..3_125 {
                let session = format!("worker-{worker}-session-{index}");
                let id = resolve_continuation_id(&session, Some("account"), "test-scope", false);
                assert!(Uuid::parse_str(&id).is_ok());
                if index % 256 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }));
    }
    for task in tasks.iter().take(16) {
        task.abort();
    }
    for task in tasks {
        let _ = task.await;
    }

    assert_eq!(
        stable,
        resolve_continuation_id("stable", Some("account"), "test-scope", false)
    );
}
