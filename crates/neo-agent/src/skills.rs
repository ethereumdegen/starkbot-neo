//! What an application expects to be operated with (A19).
//!
//! Knowing that degen-paint Studio is installed is enough to *open* it and
//! not enough to *use* it. Measured, not assumed: with the inventory in the
//! prompt the agent reached the right application on the first action, then
//! spent its whole budget on goals like "create a cool logo with a modern,
//! tech-forward design" and left the document at revision 0. Nothing had told
//! it that every capability in that app is a named op behind a command
//! palette, that the Status line is how a step is confirmed, or that lint is
//! what a blind operator checks before exporting.
//!
//! A media app ships that knowledge itself — `dpaint skill` writes exactly
//! this layout, and `GET /api/v1/skill` serves the same bytes — and until now
//! nothing in Starkbot read it. This module is the reader: packs are **data**
//! (06 §1), they advise and never authorise, and a skill is prose for the
//! model, not a script.
//!
//! ```text
//! <data_dir>/packs/<pack>/
//!   desktop/apps/<app-id>.json   { "skills": ["degen-paint"], … }
//!   skills/<name>.md
//! ```
//!
//! A skill is loaded only when the application it belongs to is actually
//! installed. A prompt carrying instructions for an app this machine cannot
//! open is pure cost, and worse, an invitation to try.

use std::path::{Path, PathBuf};

/// How to operate one installed application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppSkill {
    /// The id the `app` tool takes, from the hint file's name.
    pub app: String,
    /// The human name of the application, for the prompt's heading.
    pub name: String,
    /// The skill document, verbatim.
    pub body: String,
}

/// Where packs live. One directory, so a pack can be dropped in or deleted
/// without a registry to keep in step.
#[must_use]
pub fn packs_dir() -> Option<PathBuf> {
    neo_core::paths::data_dir().map(|dir| dir.join("packs"))
}

/// Every skill whose application this machine can open.
#[must_use]
pub fn installed_skills() -> Vec<AppSkill> {
    packs_dir().map(|dir| read_packs(&dir)).unwrap_or_default()
}

/// Read every pack under `root`.
fn read_packs(root: &Path) -> Vec<AppSkill> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut skills: Vec<AppSkill> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .flat_map(|entry| read_pack(&entry.path()))
        .collect();
    // Deterministic order: the prompt is cached by the provider, and a list
    // that reshuffles per turn defeats that for no benefit.
    skills.sort_by(|left, right| left.app.cmp(&right.app));
    skills.dedup_by(|left, right| left.app == right.app);
    skills
}

/// One pack: its app hints, and the skills those hints name.
fn read_pack(pack: &Path) -> Vec<AppSkill> {
    let Ok(hints) = std::fs::read_dir(pack.join("desktop/apps")) else {
        return Vec::new();
    };
    hints
        .flatten()
        .filter_map(|hint| {
            let text = std::fs::read_to_string(hint.path()).ok()?;
            let value: serde_json::Value = serde_json::from_str(&text).ok()?;
            // The hint names the app; the machine decides whether it is here.
            // `app_id` is the Linux/Wayland spelling, `bundle_id` the macOS
            // one, and A36 makes them the same string for a GTK or Tauri app.
            let app = value
                .get("app_id")
                .or_else(|| value.get("bundle_id"))
                .and_then(serde_json::Value::as_str)?;
            let installed = neo_ax::lookup(app)?;
            let mut parts: Vec<String> = value
                .get("skills")?
                .as_array()?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .flat_map(|name| {
                    // What the app can do, and then how its own authors say
                    // a goal for it should be worded. The second half is not
                    // decoration: the first run with only the prose skill
                    // invented op ids that do not exist (`add.circle` for
                    // `vector.object.add-ellipse`) and spent its budget
                    // searching for them.
                    [
                        std::fs::read_to_string(pack.join("skills").join(format!("{name}.md")))
                            .ok()
                            .map(|body| strip_front_matter(&body)),
                        goal_rules(&pack.join("goals").join(format!("{name}.json"))),
                    ]
                })
                .flatten()
                .collect();
            // Pack-level, so it is read once and shared by every app in it.
            if let Ok(vocabulary) = std::fs::read_to_string(pack.join("vocabulary.md")) {
                parts.push(strip_front_matter(&vocabulary));
            }
            let body = parts.join("\n\n");
            (!body.trim().is_empty()).then(|| AppSkill {
                app: installed.id,
                name: value
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&installed.name)
                    .to_owned(),
                body,
            })
        })
        .collect()
}

/// The pack's rules for wording a goal for this app (06 §4.2).
///
/// Only the rules are taken. The templates beside them are whole worked
/// briefs, and pasting those into every prompt would spend more context
/// teaching one app's example than the task itself is worth.
fn goal_rules(path: &Path) -> Option<String> {
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let rules: Vec<String> = value
        .get("rules")?
        .as_array()?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(|rule| format!("- {rule}"))
        .collect();
    (!rules.is_empty()).then(|| format!("### Writing a goal for this app\n\n{}", rules.join("\n")))
}

/// Drop the YAML front matter a skill file carries for its own tooling.
///
/// It is metadata about the document, not instruction for the operator, and
/// leaving it in spends tokens teaching the model a version number.
fn strip_front_matter(body: &str) -> String {
    let trimmed = body.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return body.trim().to_owned();
    };
    match rest.split_once("\n---") {
        Some((_, after)) => after
            .trim_start_matches(['-', '\n', '\r'])
            .trim()
            .to_owned(),
        None => body.trim().to_owned(),
    }
}

/// The prompt section, or nothing at all when no installed app ships a skill.
#[must_use]
pub fn render(skills: &[AppSkill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nHow to operate the applications on this machine that describe \
         themselves. This is the application's own description, not a script: \
         follow it, and read back what the application says before deciding \
         the next step.",
    );
    for skill in skills {
        out.push_str(&format!(
            "\n\n## {} (`{}`)\n\n{}",
            skill.name, skill.app, skill.body
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_is_not_sent_to_the_model() {
        let body = "---\ndescription: Operating degen-paint\nversion: 1.0.0\n---\n\n# degen-paint\n\nEvery capability is an op.";
        let stripped = strip_front_matter(body);

        assert!(stripped.starts_with("# degen-paint"), "{stripped}");
        assert!(!stripped.contains("version: 1.0.0"));
    }

    /// A document with no front matter must survive unchanged — the marker is
    /// optional, and a naive split would eat the first heading.
    #[test]
    fn a_skill_without_front_matter_is_untouched() {
        assert_eq!(
            strip_front_matter("# degen-paint\n\nops."),
            "# degen-paint\n\nops."
        );
    }

    /// The empty machine adds nothing at all: an empty heading would tell the
    /// model there are skills and then show it none.
    #[test]
    fn no_skills_means_no_section() {
        assert_eq!(render(&[]), "");
    }

    #[test]
    fn a_skill_is_rendered_under_the_id_the_app_tool_takes() {
        let section = render(&[AppSkill {
            app: "dev.degenpaint.studio".to_owned(),
            name: "degen-paint Studio".to_owned(),
            body: "Everything runs through the command palette.".to_owned(),
        }]);

        assert!(section.contains("dev.degenpaint.studio"));
        assert!(section.contains("command palette"));
    }
}
