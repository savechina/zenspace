//! E7: the exported wiki skill must be discoverable by zen's own
//! SkillLoader — the FR-039 neutral plane means internal skills and the
//! exported KB view share one discovery contract.

use std::fs;

#[test]
fn exported_wiki_skill_is_discoverable_and_loadable() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().to_path_buf();

    let pages = vec![zen_vault::WikiSkillPage {
        rel_path: "topics/zen.md".to_string(),
        title: "Zen".to_string(),
        description: "daily practice".to_string(),
    }];
    let rendered = zen_vault::render_wiki_skill_md(&pages);
    let skill_dir = skills_dir.join(zen_vault::WIKI_SKILL_NAME);
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join(zen_vault::skill_file_name()), rendered).unwrap();

    let loader = zen_agents::skill_loader::SkillLoader::new_from_dir(skills_dir);
    let names = loader.list_skills().unwrap();
    assert!(
        names.iter().any(|n| n == zen_vault::WIKI_SKILL_NAME),
        "exported skill must be discovered, got {names:?}"
    );
    let skill = loader.load_skill(zen_vault::WIKI_SKILL_NAME).unwrap();
    assert_eq!(skill.name, zen_vault::WIKI_SKILL_NAME);
    assert!(
        skill.description.contains("Use when"),
        "the rendered description must survive the loader's frontmatter parse"
    );
    assert!(
        skill.body.contains("topics/zen.md"),
        "the page index must survive into the loaded body"
    );
}
