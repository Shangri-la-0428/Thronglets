use crate::trace::MethodCompliance;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyStrength {
    Hard,
    Soft,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyScope {
    Task,
    Project,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ActivePolicyRule {
    pub id: String,
    pub strength: PolicyStrength,
    pub scope: PolicyScope,
    pub summary: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivePolicySet {
    pub all_rules: Vec<ActivePolicyRule>,
    pub relevant_rules: Vec<ActivePolicyRule>,
}

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "into", "from", "then", "them", "they", "your",
    "their", "should", "must", "mustn", "dont", "don't", "never", "always", "prefer", "keep",
    "avoid", "using", "use", "into", "only", "when", "what", "does", "after", "before", "have",
    "has", "had", "will", "been", "just", "than", "then", "more", "less", "dont", "shouldn",
    "shouldn't", "mustn't", "without", "within", "across", "same", "else", "than", "here", "there",
];

pub fn compile_active_policy(payload: &Value, tool_input: &Value) -> ActivePolicySet {
    let mut all_rules = parse_payload_rules(
        payload
            .get("active_policy")
            .or_else(|| payload.get("activePolicy")),
    );
    all_rules.extend(parse_current_turn_policy_rules(payload));
    all_rules.extend(discover_project_rules(tool_input));
    dedupe_rules(&mut all_rules);

    let hook_context = crate::context::build_hook_context(
        payload
            .get("tool_name")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
        tool_input,
    );
    let relevant_rules = relevant_rules_for_context(&all_rules, &hook_context);

    ActivePolicySet {
        all_rules,
        relevant_rules,
    }
}

fn parse_current_turn_policy_rules(payload: &Value) -> Vec<ActivePolicyRule> {
    let mut rules = parse_payload_rules(
        payload
            .get("current_turn_policy")
            .or_else(|| payload.get("currentTurnPolicy"))
            .or_else(|| payload.get("task_policy"))
            .or_else(|| payload.get("taskPolicy")),
    );

    for key in [
        "current_turn_correction",
        "currentTurnCorrection",
        "task_correction",
        "taskCorrection",
        "explicit_instruction",
        "explicitInstruction",
    ] {
        let Some(summary) = payload.get(key).and_then(|value| value.as_str()) else {
            continue;
        };
        let summary = normalize_summary(summary);
        if summary.is_empty() {
            continue;
        }
        rules.push(ActivePolicyRule {
            id: stable_rule_id("turn", &summary),
            strength: PolicyStrength::Hard,
            scope: PolicyScope::Task,
            summary,
        });
    }

    dedupe_rules(&mut rules);
    rules
}

pub fn method_compliance_from_payload(
    payload: &Value,
    active_policy: &ActivePolicySet,
) -> Option<MethodCompliance> {
    let explicit = payload
        .get("method_compliance")
        .or_else(|| payload.get("methodCompliance"))
        .and_then(|value| value.as_str())
        .and_then(MethodCompliance::parse);
    if explicit.is_some() {
        return explicit;
    }
    if !active_policy.relevant_rules.is_empty() {
        return Some(MethodCompliance::Unknown);
    }
    None
}

fn parse_payload_rules(value: Option<&Value>) -> Vec<ActivePolicyRule> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    let mut rules = Vec::new();
    for entry in entries {
        match entry {
            Value::String(summary) => {
                let summary = normalize_summary(summary);
                if summary.is_empty() {
                    continue;
                }
                rules.push(ActivePolicyRule {
                    id: stable_rule_id("payload", &summary),
                    strength: PolicyStrength::Hard,
                    scope: PolicyScope::Task,
                    summary,
                });
            }
            Value::Object(record) => {
                let summary = record
                    .get("summary")
                    .and_then(|value| value.as_str())
                    .map(normalize_summary)
                    .unwrap_or_default();
                if summary.is_empty() {
                    continue;
                }
                let strength = record
                    .get("strength")
                    .and_then(|value| value.as_str())
                    .and_then(parse_strength)
                    .unwrap_or(PolicyStrength::Hard);
                let scope = record
                    .get("scope")
                    .and_then(|value| value.as_str())
                    .and_then(parse_scope)
                    .unwrap_or(PolicyScope::Task);
                let id = record
                    .get("id")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| stable_rule_id("payload", &summary));
                rules.push(ActivePolicyRule {
                    id,
                    strength,
                    scope,
                    summary,
                });
            }
            _ => {}
        }
    }
    rules
}

fn parse_strength(value: &str) -> Option<PolicyStrength> {
    match value.trim() {
        "hard" => Some(PolicyStrength::Hard),
        "soft" => Some(PolicyStrength::Soft),
        _ => None,
    }
}

fn parse_scope(value: &str) -> Option<PolicyScope> {
    match value.trim() {
        "task" => Some(PolicyScope::Task),
        "project" => Some(PolicyScope::Project),
        _ => None,
    }
}

fn discover_project_rules(tool_input: &Value) -> Vec<ActivePolicyRule> {
    let mut candidates = guidance_candidates(tool_input);
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }

    for candidate in candidates {
        if let Some(agents_path) = nearest_agents_file(&candidate)
            && let Ok(content) = std::fs::read_to_string(&agents_path)
        {
            return parse_project_rules(&content);
        }
    }
    Vec::new()
}

fn guidance_candidates(tool_input: &Value) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for key in ["file_path", "path"] {
        if let Some(raw) = tool_input.get(key).and_then(|value| value.as_str()) {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let path = PathBuf::from(raw);
            if path.is_absolute() {
                if let Some(parent) = path.parent() {
                    candidates.push(parent.to_path_buf());
                }
            } else if let Ok(cwd) = std::env::current_dir() {
                let joined = cwd.join(path);
                if let Some(parent) = joined.parent() {
                    candidates.push(parent.to_path_buf());
                }
            }
        }
    }
    candidates
}

fn nearest_agents_file(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join("AGENTS.md");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn parse_project_rules(markdown: &str) -> Vec<ActivePolicyRule> {
    let mut rules = Vec::new();
    let mut in_code = false;
    for raw_line in markdown.lines() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("<!--")
        {
            continue;
        }
        let normalized = normalize_markdown_rule(trimmed);
        if normalized.is_empty() {
            continue;
        }
        let Some((strength, summary)) = classify_project_rule(&normalized) else {
            continue;
        };
        rules.push(ActivePolicyRule {
            id: stable_rule_id("project", &summary),
            strength,
            scope: PolicyScope::Project,
            summary,
        });
    }
    rules
}

fn normalize_markdown_rule(line: &str) -> String {
    let without_prefix = line
        .trim_start_matches(|c: char| c == '-' || c == '*' || c.is_ascii_digit() || c == '.' || c == ')')
        .trim();
    normalize_summary(without_prefix)
}

fn classify_project_rule(line: &str) -> Option<(PolicyStrength, String)> {
    let lowered = line.to_lowercase();
    let strength = if lowered.contains("must not")
        || lowered.contains("do not")
        || lowered.contains("don't")
        || lowered.contains("never")
        || lowered.contains("must ")
        || lowered.contains("always")
    {
        PolicyStrength::Hard
    } else if lowered.contains("prefer")
        || lowered.contains("should")
        || lowered.contains("avoid")
        || lowered.contains("keep ")
        || lowered.contains("default to")
    {
        PolicyStrength::Soft
    } else {
        return None;
    };
    Some((strength, line.to_string()))
}

fn relevant_rules_for_context(rules: &[ActivePolicyRule], hook_context: &str) -> Vec<ActivePolicyRule> {
    let context_tokens = tokenize(hook_context);
    let mut relevant = Vec::new();
    for rule in rules {
        if rule.scope == PolicyScope::Task {
            relevant.push(rule.clone());
            continue;
        }
        let rule_tokens = tokenize(&rule.summary);
        if rule_tokens.is_empty() || context_tokens.is_empty() {
            continue;
        }
        let overlap = rule_tokens.intersection(&context_tokens).count();
        let denom = rule_tokens.len().min(context_tokens.len()).max(1);
        let score = overlap as f32 / denom as f32;
        if score >= 0.2 || overlap >= 2 {
            relevant.push(rule.clone());
        }
    }
    relevant.sort_by(|a, b| {
        strength_rank(b.strength)
            .cmp(&strength_rank(a.strength))
            .then_with(|| a.summary.len().cmp(&b.summary.len()))
    });
    relevant.truncate(3);
    relevant
}

fn dedupe_rules(rules: &mut Vec<ActivePolicyRule>) {
    let mut seen = HashSet::new();
    rules.retain(|rule| seen.insert(rule.id.clone()));
}

fn strength_rank(value: PolicyStrength) -> u8 {
    match value {
        PolicyStrength::Hard => 2,
        PolicyStrength::Soft => 1,
    }
}

fn stable_rule_id(prefix: &str, summary: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(summary.as_bytes());
    let digest = hasher.finalize();
    format!("{prefix}:{:02x}{:02x}{:02x}{:02x}", digest[0], digest[1], digest[2], digest[3])
}

fn normalize_summary(summary: &str) -> String {
    summary.trim().replace('`', "").replace(char::is_whitespace, " ")
}

fn tokenize(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter_map(|token| {
            let lowered = token.trim().to_lowercase();
            if lowered.len() < 3 || STOPWORDS.contains(&lowered.as_str()) {
                return None;
            }
            Some(lowered)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_payload_rules_and_defaults_to_hard_task_scope() {
        let rules = parse_payload_rules(Some(&json!([
            "reuse existing shared components",
            {
                "id": "project:no-dup-ui",
                "strength": "soft",
                "scope": "project",
                "summary": "prefer shared components over duplicate page UI"
            }
        ])));

        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].strength, PolicyStrength::Hard);
        assert_eq!(rules[0].scope, PolicyScope::Task);
        assert_eq!(rules[1].id, "project:no-dup-ui");
        assert_eq!(rules[1].strength, PolicyStrength::Soft);
        assert_eq!(rules[1].scope, PolicyScope::Project);
    }

    #[test]
    fn derives_project_rules_from_agents_markdown() {
        let rules = parse_project_rules(
            r#"
# Project
- Do not introduce Docker on this VPS.
- Prefer shared components over duplicate page UI.
- Keep changes reversible when possible.
```bash
do not parse code fences
```
"#,
        );
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].strength, PolicyStrength::Hard);
        assert!(rules.iter().any(|rule| rule.summary.contains("shared components")));
    }

    #[test]
    fn relevant_rules_keep_task_scope_and_filter_project_scope() {
        let rules = vec![
            ActivePolicyRule {
                id: "task:components".into(),
                strength: PolicyStrength::Hard,
                scope: PolicyScope::Task,
                summary: "reuse existing shared components".into(),
            },
            ActivePolicyRule {
                id: "project:docker".into(),
                strength: PolicyStrength::Hard,
                scope: PolicyScope::Project,
                summary: "do not introduce Docker on this VPS".into(),
            },
        ];
        let relevant = relevant_rules_for_context(&rules, "edit file: src/components/Card.tsx");
        assert_eq!(relevant.len(), 1);
        assert_eq!(relevant[0].id, "task:components");
    }

    #[test]
    fn method_compliance_defaults_to_unknown_when_policy_is_active() {
        let set = ActivePolicySet {
            all_rules: vec![ActivePolicyRule {
                id: "task:components".into(),
                strength: PolicyStrength::Hard,
                scope: PolicyScope::Task,
                summary: "reuse existing shared components".into(),
            }],
            relevant_rules: vec![ActivePolicyRule {
                id: "task:components".into(),
                strength: PolicyStrength::Hard,
                scope: PolicyScope::Task,
                summary: "reuse existing shared components".into(),
            }],
        };
        let compliance = method_compliance_from_payload(&json!({}), &set);
        assert_eq!(compliance, Some(MethodCompliance::Unknown));
    }

    #[test]
    fn current_turn_correction_compiles_into_hard_task_rule() {
        let set = compile_active_policy(
            &json!({
                "currentTurnCorrection": "reuse existing shared components instead of hand-writing duplicate page UI",
                "tool_input": {
                    "file_path": "/repo/src/app/dashboard/page.tsx"
                }
            }),
            &json!({
                "file_path": "/repo/src/app/dashboard/page.tsx"
            }),
        );

        assert!(
            set.all_rules.iter().any(|rule|
                rule.scope == PolicyScope::Task
                    && rule.strength == PolicyStrength::Hard
                    && rule.summary.contains("reuse existing shared components")
            ),
            "{set:#?}"
        );
        assert!(
            set.relevant_rules.iter().any(|rule| rule.summary.contains("reuse existing shared components")),
            "{set:#?}"
        );
    }
}
