//! Tests ported from the skills registry half of the python test suite.

use super::*;

fn write_skill(dir: &Path, folder: &str, frontmatter: &str, body: &str) {
    let skill_dir = dir.join("skills").join(folder);
    std::fs::create_dir_all(&skill_dir).unwrap();
    let content = if frontmatter.is_empty() {
        body.to_string()
    } else {
        format!("---\n{frontmatter}\n---\n{body}")
    };
    std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
}

fn agent(name: &str, tags: &[&str]) -> (String, Vec<String>) {
    (
        name.to_string(),
        tags.iter().map(|t| t.to_string()).collect(),
    )
}

#[test]
fn skills_load_from_skill_md_folders() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "code-review",
        "name: Code Review\ndescription: How to review code\ntags:\n  - code\nowner_agents:\n  - coder\nrecommended_workflows:\n  - review-flow\n",
        "Do the review checklist.",
    );

    let registry = SkillRegistry::new(dir.path());
    let skills = registry.list_skills();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "Code Review");
    assert_eq!(skills[0].body, "Do the review checklist.");
    assert!(registry.find("code review").is_some());
}

#[test]
fn frontmatter_less_skills_fall_back_to_the_folder_name() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(dir.path(), "notes-skill", "", "# Just the body\n");
    let registry = SkillRegistry::new(dir.path());
    let skills = registry.list_skills();
    assert_eq!(skills[0].name, "notes-skill");
}

#[test]
fn find_for_agent_matches_owner_or_tag() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "code-review",
        "name: Code Review\nowner_agents:\n  - coder\ntags:\n  - code\n",
        "body",
    );
    write_skill(
        dir.path(),
        "travel-planner",
        "name: Travel Planner\nowner_agents:\n  - general\n",
        "body",
    );

    let registry = SkillRegistry::new(dir.path());
    let (name, tags) = agent("Coder", &[]);
    assert_eq!(registry.find_for_agent(&name, &tags).len(), 1);
    let (name, tags) = agent("SomeoneElse", &["code"]);
    assert_eq!(registry.find_for_agent(&name, &tags).len(), 1);
    let (name, tags) = agent("Nobody", &[]);
    assert!(registry.find_for_agent(&name, &tags).is_empty());
}

#[test]
fn render_catalog_lists_and_hints() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "code-review",
        "name: Code Review\ndescription: How to review code\nowner_agents:\n  - coder\nrecommended_workflows:\n  - review-flow\n  - second\n  - third\n  - fourth\n  - fifth\n",
        "body",
    );

    let registry = SkillRegistry::new(dir.path());
    let (name, tags) = agent("Coder", &[]);
    let catalog = registry.render_catalog(&name, &tags, 4);
    assert!(catalog.contains("[Skill Library]"), "{catalog}");
    assert!(catalog.contains("- Code Review: How to review code"));
    assert!(catalog.contains("Recommended workflows: review-flow, second, third, fourth"));
    assert!(catalog.contains("Skills are reusable guidance packs"));
}

#[test]
fn empty_skill_library_renders_no_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SkillRegistry::new(dir.path());
    let (name, tags) = agent("Coder", &[]);
    assert!(registry.render_catalog(&name, &tags, 4).is_empty());
}
