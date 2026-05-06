mod web_research;
mod workspace;

pub use web_research::WebResearchSkill;
pub use workspace::{
    create_skill, validate_skill_dir, SkillCreation, SkillResourceKind, SkillStoreError,
    WorkspaceSkill, WorkspaceSkillCatalog, WorkspaceSkillMetadata,
};
