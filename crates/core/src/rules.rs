//! Smart-folder rules and filter-rule pipelines.
//!
//! Ports `core/smart_folders.py` (boolean rule tree over article
//! fields) and `core/filters.py` (ordered action pipeline). Rules
//! are stored as JSON in the smart_folders / filter_rules tables;
//! this module evaluates them in memory against an article.

use crate::db::Article;
use serde_json::Value;

/// Article facts a rule can test. `tag` reads article tags (the
/// Python schema stores them in articles.tags; our Article rows do
/// not carry them yet, so callers pass an empty slice).
pub struct RuleArticle<'a> {
    pub article: &'a Article,
    pub feed_title: &'a str,
    pub opened: bool,
    pub updated: bool,
    pub tags: &'a [String],
}

fn as_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        Value::String(s) => matches!(
            s.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => false,
    }
}

fn is_group(node: &Value) -> bool {
    node.get("conditions").is_some()
}

fn match_leaf(cond: &Value, art: &RuleArticle) -> bool {
    let field = cond
        .get("field")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let op = cond
        .get("op")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let value = cond.get("value").cloned().unwrap_or(Value::Null);
    match field.as_str() {
        "read" => art.article.is_read == as_bool(&value),
        "favorite" => art.article.is_favorite == as_bool(&value),
        "opened" => art.opened == as_bool(&value),
        "updated" => art.updated == as_bool(&value),
        "title" | "content" | "description" | "author" | "feed" | "url" | "tag" => {
            let hay = match field.as_str() {
                "title" => art.article.title.clone(),
                "content" => art.article.content.clone().unwrap_or_default(),
                "description" => art.article.description.clone().unwrap_or_default(),
                "author" => art.article.author.clone().unwrap_or_default(),
                "feed" => art.feed_title.to_string(),
                "url" => art.article.url.clone(),
                _ => art.tags.join("\n"),
            };
            let hay = hay.to_lowercase();
            let needle = value.as_str().unwrap_or("").to_lowercase();
            match op.as_str() {
                "equals" => hay == needle,
                "starts_with" => hay.starts_with(&needle),
                "not_contains" => !hay.contains(&needle),
                _ => hay.contains(&needle), // contains (default)
            }
        }
        _ => true, // unknown field: skipped (matches Python's empty SQL -> group-dependent; leaf alone is neutral here)
    }
}

fn match_group(group: &Value, art: &RuleArticle) -> bool {
    let any = group
        .get("match")
        .and_then(|v| v.as_str())
        .unwrap_or("all")
        .eq_ignore_ascii_case("any");
    let conds = group.get("conditions").and_then(|v| v.as_array());
    let Some(conds) = conds else {
        return !any; // empty "all" = true, empty "any" = false
    };
    if conds.is_empty() {
        return !any;
    }
    if any {
        conds
            .iter()
            .filter(|c| known_node(c))
            .any(|c| match_node(c, art))
    } else {
        conds
            .iter()
            .filter(|c| known_node(c))
            .all(|c| match_node(c, art))
    }
}

fn known_node(node: &Value) -> bool {
    is_group(node)
        || matches!(
            node.get("field")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase()
                .as_str(),
            "read"
                | "favorite"
                | "opened"
                | "updated"
                | "title"
                | "content"
                | "description"
                | "author"
                | "feed"
                | "url"
                | "tag"
        )
}

fn match_node(node: &Value, art: &RuleArticle) -> bool {
    if is_group(node) {
        match_group(node, art)
    } else {
        match_leaf(node, art)
    }
}

/// True when the rule tree matches the article (Python `rule_matches`).
pub fn rule_matches(rule: &Value, art: &RuleArticle) -> bool {
    if !rule.is_object() {
        return true; // Python build_where returns 1=1 for non-dict rules
    }
    match_group(rule, art)
}

/// A filter rule row (filter_rules table).
#[derive(Debug, Clone, PartialEq)]
pub struct FilterRule {
    pub id: String,
    pub name: String,
    pub rule: Value,
    pub actions: Value,
    pub enabled: bool,
    pub stop: bool,
    pub position: i64,
}

/// Resolved actions after the pipeline runs (Python
/// `evaluate_pipeline` output shape).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EffectiveActions {
    pub mark_read: bool,
    pub mark_favorite: bool,
    pub delete: bool,
    pub skip_notification: bool,
    pub move_to: Option<String>,
    pub labels: Vec<String>,
}

fn action_str(actions: &Value, key: &str) -> Option<String> {
    actions
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run enabled rules in order against one article; a matching rule
/// with stop=true ends the pipeline (Python semantics).
pub fn evaluate_pipeline(rules: &[FilterRule], art: &RuleArticle) -> EffectiveActions {
    let mut out = EffectiveActions::default();
    for rule in rules.iter().filter(|r| r.enabled) {
        if !rule_matches(&rule.rule, art) {
            continue;
        }
        let a = &rule.actions;
        if as_bool(a.get("mark_read").unwrap_or(&Value::Null)) {
            out.mark_read = true;
        }
        if as_bool(a.get("mark_favorite").unwrap_or(&Value::Null)) {
            out.mark_favorite = true;
        }
        if as_bool(a.get("delete").unwrap_or(&Value::Null)) {
            out.delete = true;
        }
        if as_bool(a.get("skip_notification").unwrap_or(&Value::Null)) {
            out.skip_notification = true;
        }
        if let Some(m) = action_str(a, "move") {
            out.move_to = Some(m);
        }
        if let Some(l) = action_str(a, "label") {
            out.labels.push(l);
        }
        if rule.stop {
            break;
        }
    }
    out
}
