//! Agent tools for creating, updating, and deleting personal skills at runtime.
//! Skills are written to `<data_dir>/skills/<name>/SKILL.md` (Personal source).

mod crud;
mod helpers;
mod read_ops;
mod write_ops;

pub use {
    crud::{CreateSkillTool, DeleteSkillTool, UpdateSkillTool},
    read_ops::ReadSkillTool,
    write_ops::{PatchSkillTool, WriteSkillFilesTool},
};

/// Cap on the size of a single skill body (SKILL.md).
const MAX_SKILL_BODY_BYTES: usize = chelix_call_bus::MAX_SKILL_FILE_BYTES as usize;

#[cfg(test)]
mod tests;
