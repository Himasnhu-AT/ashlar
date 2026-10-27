//! What installed integrations bring the chat besides their tools (feature `integrations`): their
//! skills (the skill folders in their bundles and `skill://` resources), named
//! `<integration>:<name>`, and their MCP prompts as `/<integration>:<prompt>` commands
//! ([`SlashAction::External`], run with [`run_prompt`]).

use std::sync::Arc;

use ashlar_integrations::Integrations;

use crate::skills::{Fetch, SkillContent, SkillEntry, Skills};
use crate::slash::{SlashAction, SlashCommand};

/// Add the skills of the enabled integrations to `skills`. Returns how many.
pub fn add_skills(skills: &mut Skills, ix: &Integrations) -> usize {
    let mut n = 0;
    for (id, dir) in ix.skill_dirs() {
        n += skills.add_dir(&dir, &id, &id);
    }
    for r in ix.skill_resources() {
        let (ix2, id, uri) = (ix.clone(), r.integration.clone(), r.resource.uri.clone());
        let fetch: Fetch = Arc::new(move || ix2.read_resource(&id, &uri));
        let description = if r.resource.description.trim().is_empty() { r.resource.name.clone() } else { r.resource.description.clone() };
        if skills.add(SkillEntry { name: r.name, description, source: r.integration, content: SkillContent::Fetch(fetch) }) {
            n += 1;
        }
    }
    n
}

/// The running integrations' prompts as commands (`/earshot:transcribe <file>`).
pub fn slash_commands(ix: &Integrations) -> Vec<SlashCommand> {
    ix.prompts()
        .into_iter()
        .map(|p| SlashCommand {
            name: p.name.clone(),
            description: if p.prompt.description.trim().is_empty() {
                p.prompt.title.clone().unwrap_or_else(|| format!("{} prompt", p.integration))
            } else {
                p.prompt.description.clone()
            },
            args: p.prompt.args_hint(),
            action: SlashAction::External { source: p.integration, name: p.name },
        })
        .collect()
}

/// The text of an integration's prompt with what was typed after the command, to send as the
/// person's message. Blocks on the integration: run it off the UI thread.
pub fn run_prompt(ix: &Integrations, name: &str, args: &str) -> Result<String, String> {
    ix.get_prompt(name, args)
}
