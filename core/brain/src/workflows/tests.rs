//! Tests ported from the workflows registry/review queue behaviours pinned in
//! `brain/tests/test_workflows.py` (the loading and review-queue half).

use super::*;
use std::path::Path;

fn write_workflow(dir: &Path, name: &str, owners: &[&str], tags: &[&str], review_status: &str) {
    let body = format!(
        "name: {name}\ndescription: {name} description\ntags:\n{}\nowner_agents:\n{}\ntools:\n  - shell\nsteps:\n  - step one\n  - step two\nsuccess_criteria:\n  - it worked\ncreated_by: preset\nreview_status: {review_status}\n",
        tags.iter().map(|t| format!("  - {t}")).collect::<Vec<_>>().join("\n"),
        owners.iter().map(|o| format!("  - {o}")).collect::<Vec<_>>().join("\n"),
    );
    std::fs::write(dir.join(format!("{}.yaml", slugify(name))), body).unwrap();
}

#[test]
fn workflows_load_from_shared_and_personal_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("workflows");
    let personal = dir.path().join("personal").join("workflows");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(&personal).unwrap();
    write_workflow(&shared, "DeployFlow", &["general"], &["deploy"], "approved");
    write_workflow(&personal, "PersonalFlow", &["coder"], &["code"], "approved");
    // A non-approved draft must not surface.
    write_workflow(&shared, "DraftFlow", &["general"], &[], "pending");

    let registry = WorkflowRegistry::new(dir.path());
    let names: Vec<String> = registry
        .list_workflows()
        .iter()
        .map(|w| w.name.clone())
        .collect();
    assert!(names.contains(&"DeployFlow".to_string()));
    assert!(names.contains(&"PersonalFlow".to_string()));
    assert!(!names.contains(&"DraftFlow".to_string()));
}

#[test]
fn find_is_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("workflows");
    std::fs::create_dir_all(&shared).unwrap();
    write_workflow(&shared, "DeployFlow", &[], &[], "approved");

    let registry = WorkflowRegistry::new(dir.path());
    assert!(registry.find("deployflow").is_some());
    assert!(registry.find("DEPLOYFLOW").is_some());
    assert!(registry.find("nope").is_none());
}

#[test]
fn find_for_agent_matches_owner_or_tag() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("workflows");
    std::fs::create_dir_all(&shared).unwrap();
    write_workflow(&shared, "DeployFlow", &["sysadmin"], &["deploy"], "approved");
    write_workflow(&shared, "CodeFlow", &["coder"], &["code"], "approved");

    let registry = WorkflowRegistry::new(dir.path());
    let for_sysadmin = registry.find_for_agent("SysAdmin", &[]);
    assert_eq!(for_sysadmin.len(), 1);
    assert_eq!(for_sysadmin[0].name, "DeployFlow");

    let for_coder = registry.find_for_agent("Other", &["code"]);
    assert_eq!(for_coder.len(), 1);
    assert_eq!(for_coder[0].name, "CodeFlow");

    assert!(registry.find_for_agent("Nobody", &[]).is_empty());
}

#[test]
fn render_catalog_lists_and_hints() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("workflows");
    std::fs::create_dir_all(&shared).unwrap();
    write_workflow(&shared, "DeployFlow", &["sysadmin"], &["deploy"], "approved");

    let registry = WorkflowRegistry::new(dir.path());
    let catalog = registry.render_catalog("SysAdmin", &[], 4);
    assert!(catalog.contains("[Workflow Library]"));
    assert!(catalog.contains("- DeployFlow: DeployFlow description"));
    assert!(catalog.contains("Tools: shell"));
    assert!(catalog.contains("Steps: step one -> step two"));
    assert!(catalog.contains("Prefer an existing shared workflow"));
}

#[test]
fn review_queue_round_trips_approve_and_reject() {
    let dir = tempfile::tempdir().unwrap();
    let queue = ReviewQueue::new(dir.path());

    let workflow = WorkflowDefinition {
        name: "DraftFlow".to_string(),
        description: "Temporary workflow".to_string(),
        owner_agents: vec!["general".to_string()],
        tools: vec!["chrome_browser".to_string()],
        steps: vec!["open page".to_string(), "extract result".to_string()],
        created_by: "agent".to_string(),
        review_status: "pending".to_string(),
        ..Default::default()
    };
    let item = queue.submit_workflow(workflow, "general");
    assert!(item.review_id.starts_with("wf-"));

    let pending = queue.list_reviews("pending");
    assert_eq!(pending.len(), 1);

    let approved = queue.approve_workflow(&item.review_id).unwrap();
    assert_eq!(approved.status, "approved");
    assert_eq!(approved.workflow.review_status, "approved");

    // Approval publishes to the shared library.
    let registry = WorkflowRegistry::new(dir.path());
    assert!(registry.find("DraftFlow").is_some());

    let rejected = queue.reject_workflow(&item.review_id, "needs work").unwrap();
    assert_eq!(rejected.status, "rejected");
    assert_eq!(rejected.reviewer_notes, "needs work");
}

#[test]
fn review_queue_rejects_unknown_ids() {
    let dir = tempfile::tempdir().unwrap();
    let queue = ReviewQueue::new(dir.path());
    assert!(queue.approve_workflow("wf-nope").is_err());
}
