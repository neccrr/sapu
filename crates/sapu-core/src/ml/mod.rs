//! Sapu's own model: a small classifier that learns which files are safe to
//! remove, trained on this Mac from rules, a hand-labelled seed set and your
//! feedback. No network, no LLM.
//!
//! Pipeline: [`features`] (path → sparse vector) → [`model`] (logistic
//! regression) → [`dataset`] (where labels come from). [`predict`] turns a
//! prediction into a [`Judgement`] for items the rules don't recognise.

pub mod dataset;
pub mod features;
pub mod model;

pub use dataset::{feedback_path, model_path, record_feedback};
pub use model::{Metrics, Model};

use crate::judge::{Action, Category, Judge, Judgement, Source, Verdict};
use crate::tree::{Node, Tree};
use features::Ctx;
use model::{argmax, CLASSES};
use std::path::Path;
use tracing::info;

/// Minimum confidence before the model's opinion is shown at all.
pub const MIN_CONFIDENCE: f32 = 0.85;

/// Train on seed + your scan (via the rules) + your feedback, then save.
pub fn train(tree: &Tree, judge: &Judge) -> Model {
    let ctx = Ctx { home: judge.home(), now: judge.now() };
    let seed = dataset::seed_examples(&ctx);
    let rules = dataset::distill(tree, judge, &ctx);
    let feedback = dataset::feedback_examples(&ctx);
    let (n_seed, n_rules, n_feedback) = (seed.len(), rules.len(), feedback.len());

    let mut all = seed;
    all.extend(rules);
    all.extend(feedback);

    let mut model = Model::train(&all, &model::TrainConfig::default());
    model.metrics.from_seed = n_seed;
    model.metrics.from_rules = n_rules;
    model.metrics.from_feedback = n_feedback;
    info!(
        examples = model.metrics.examples,
        seed = n_seed,
        rules = n_rules,
        feedback = n_feedback,
        holdout_accuracy = model.metrics.holdout_accuracy,
        ms = model.metrics.train_ms,
        "model trained"
    );
    if let Err(e) = model.save(&model_path()) {
        tracing::warn!(error = %e, "could not save model");
    }
    model
}

pub fn load() -> Option<Model> {
    Model::load(&model_path()).ok()
}

/// Folders that hold an app's whole data (`~/Library/Application Support/Slack`).
/// The model may suggest things *inside* them (Slack's Cache), never the folder itself.
fn is_app_data_root(path: &Path, home: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(home) else { return false };
    let comps: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    let containers =
        ["Application Support", "Containers", "Group Containers", "Preferences", "Saved Application State"];
    comps.len() <= 3
        && comps.first().is_some_and(|c| c == "Library")
        && comps.get(1).is_some_and(|c| containers.contains(&c.as_str()))
}

/// The model's verdict for an item, if it's confident enough.
/// Never returns Keep (that's the rules' job) and never acts on its own.
///
/// Guards against confident-but-wrong calls:
/// - app data folders themselves are off-limits (look inside them instead);
/// - the evidence must include something about the item itself (its name or
///   file type), not just where it lives, so "it's in Application Support"
///   alone can never justify deleting something.
pub fn predict(model: &Model, judge: &Judge, path: &Path, node: &Node) -> Option<Judgement> {
    if is_app_data_root(path, judge.home()) {
        return None;
    }
    let ctx = Ctx { home: judge.home(), now: judge.now() };
    let f = features::extract(path, node, &ctx);
    let p = model.predict(&f);
    let k = argmax(&p);
    let verdict = CLASSES[k];
    if verdict == Verdict::Keep || p[k] < MIN_CONFIDENCE {
        return None;
    }
    let evidence = model.explain(&f, k);
    let about_itself = evidence.iter().take(4).any(|(n, _)| n.starts_with("name:") || n.starts_with("ext:"));
    if !about_itself {
        return None;
    }
    let reasons: Vec<String> = evidence.into_iter().filter_map(|(name, _)| features::describe(name)).take(3).collect();
    let because = if reasons.is_empty() { String::new() } else { format!(": {}", reasons.join(", ")) };
    Some(Judgement {
        verdict,
        category: Category::AiGuess,
        reason: format!("Sapu AI is {:.0}% sure{}.", (p[k] * 100.0).min(99.0), because),
        action: Action::TrashItem,
        look_inside: false,
        source: Source::Model { confidence: p[k] },
    })
}

/// Rules first; the model only fills the gaps.
pub fn judge_with(judge: &Judge, model: Option<&Model>, path: &Path, node: &Node) -> Option<Judgement> {
    judge.judge(path, node).or_else(|| model.and_then(|m| predict(m, judge, path, node)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Kind;

    fn node(name: &str, kind: Kind, mb: u64, age_days: u64, now: u64) -> Node {
        Node {
            name: name.into(),
            kind,
            size: mb * 1_000_000,
            files: 50,
            incomplete: false,
            modified: now - age_days * 86_400,
            logical: mb * 1_000_000,
            children: vec![],
        }
    }

    #[test]
    fn learns_from_seed_and_generalises() {
        let now = 2_000_000_000;
        let home = std::path::PathBuf::from("/Users/test");
        let judge = Judge::with(home.clone(), now);
        let ctx = Ctx { home: &home, now };
        let model = Model::train(&dataset::seed_examples(&ctx), &model::TrainConfig::default());

        // An app the seed never mentions, hiding a cache the same way.
        let p = home.join("Library/Application Support/SomeNewChatApp/Cache");
        let jd = predict(&model, &judge, &p, &node("Cache", Kind::Dir, 300, 2, now));
        assert_eq!(jd.as_ref().map(|j| j.verdict), Some(Verdict::Safe), "{jd:?}");

        // The app's own data folder is never suggested, however confident.
        let p = home.join("Library/Application Support/SomeNewChatApp");
        assert!(predict(&model, &judge, &p, &node("SomeNewChatApp", Kind::Dir, 900, 2, now)).is_none());

        // A recent document should not be suggested.
        let p = home.join("Documents/Lab notes.docx");
        assert!(predict(&model, &judge, &p, &node("Lab notes.docx", Kind::File, 2, 3, now)).is_none());
    }
}
