use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

pub const MAX_RULES: usize = 200;
pub const MAX_NAME_CHARS: usize = 128;
pub const MAX_PATTERN_CHARS: usize = 4096;
pub const MAX_REPLACEMENT_CHARS: usize = 4096;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MatchType {
    #[default]
    Literal,
    Regex,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum SanitizeRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
    Function,
}

impl SanitizeRole {
    pub const ALL: [Self; 6] = [
        Self::System,
        Self::Developer,
        Self::User,
        Self::Assistant,
        Self::Tool,
        Self::Function,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Function => "function",
        }
    }

    pub(super) fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == text)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SanitizeRule {
    pub id: String,
    pub name: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub match_type: MatchType,
    pub pattern: String,
    #[serde(default)]
    pub replacement: String,
    #[serde(default)]
    pub repeat: bool,
    /// 空列表沿用旧规则的全字段行为；指定角色时只改对应消息的正文。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<SanitizeRole>,
}

impl SanitizeRule {
    pub(super) fn applies_to(&self, role: Option<SanitizeRole>, content: bool) -> bool {
        self.roles.is_empty() || (content && role.is_some_and(|role| self.roles.contains(&role)))
    }
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug)]
pub(super) enum Matcher {
    Literal,
    Regex(Regex),
}

#[derive(Debug)]
pub(super) struct CompiledRule {
    pub rule: SanitizeRule,
    pub matcher: Matcher,
}

#[derive(Debug)]
pub struct RuleSet {
    entries: Vec<SanitizeRule>,
    compiled: Vec<CompiledRule>,
}

impl RuleSet {
    pub fn compile(mut entries: Vec<SanitizeRule>) -> Result<Self, String> {
        if entries.len() > MAX_RULES {
            return Err(format!("脱敏词表最多 {MAX_RULES} 条规则"));
        }
        let mut ids = BTreeSet::new();
        let mut compiled = Vec::with_capacity(entries.len());
        for (index, rule) in entries.iter_mut().enumerate() {
            let prefix = format!("第 {} 条脱敏规则", index + 1);
            if rule.id.is_empty()
                || rule.id.len() > 64
                || !rule
                    .id
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-' || ch == b'_')
            {
                return Err(format!(
                    "{prefix}的 id 必须是 1–64 位字母、数字、连字符或下划线"
                ));
            }
            if !ids.insert(rule.id.clone()) {
                return Err(format!("脱敏规则 id 重复：{}", rule.id));
            }
            rule.name = rule.name.trim().to_string();
            if rule.name.is_empty() || rule.name.chars().count() > MAX_NAME_CHARS {
                return Err(format!("{prefix}的名称必须是 1–{MAX_NAME_CHARS} 个字符"));
            }
            if rule.pattern.trim().is_empty() || rule.pattern.chars().count() > MAX_PATTERN_CHARS {
                return Err(format!(
                    "{prefix}的匹配内容不能为空，且最多 {MAX_PATTERN_CHARS} 个字符"
                ));
            }
            if rule.replacement.chars().count() > MAX_REPLACEMENT_CHARS {
                return Err(format!(
                    "{prefix}的替换内容最多 {MAX_REPLACEMENT_CHARS} 个字符"
                ));
            }
            rule.roles.sort();
            rule.roles.dedup();
            // 重复执行只允许删除，否则用户规则可能不断扩张文本而无法收敛。
            if rule.repeat && (rule.match_type != MatchType::Regex || !rule.replacement.is_empty())
            {
                return Err(format!("{prefix}只有正则删除规则可以启用重复清理"));
            }
            let matcher = match rule.match_type {
                MatchType::Literal => Matcher::Literal,
                MatchType::Regex => Matcher::Regex(
                    RegexBuilder::new(&rule.pattern)
                        .size_limit(1024 * 1024)
                        .build()
                        .map_err(|error| format!("{prefix}的正则表达式无效：{error}"))?,
                ),
            };
            if rule.enabled {
                compiled.push(CompiledRule {
                    rule: rule.clone(),
                    matcher,
                });
            }
        }
        Ok(Self { entries, compiled })
    }

    pub fn entries(&self) -> &[SanitizeRule] {
        &self.entries
    }

    pub(super) fn compiled(&self) -> &[CompiledRule] {
        &self.compiled
    }
}

pub fn default_rules() -> Arc<RuleSet> {
    static RULES: OnceLock<Arc<RuleSet>> = OnceLock::new();
    RULES
        .get_or_init(|| {
            Arc::new(RuleSet::compile(default_entries()).expect("内置脱敏规则必须有效"))
        })
        .clone()
}

fn default_entries() -> Vec<SanitizeRule> {
    let rewrites = [
        (
            "claude-code-identity",
            "Claude Code",
            "You are Claude Code, Anthropic's official CLI for Claude",
            "You are Claude Code, Anthropic's official CLI tool for Claude",
        ),
        (
            "main-branch",
            "Main branch",
            "Main branch (you will usually use this for PRs)",
            "Default branch (you will usually use this for PRs)",
        ),
        // ⚠️ `codex-cli-identity` 已移到下方**正则**段（issue #472）：
        // 旧版是 literal 规则，把「…Codex CLI, …」改写成「…Codex CLI tool, …」——
        // 但 WorkBuddy 国内版对**两个写法都逐字拦截**，改写等于把用户从一种
        // 400 送进另一种 400（实测两条消息的最小复现，其余头完全一致）。
        (
            "claude-code-feedback",
            "Claude Code feedback",
            "To give feedback, users should report the issue at https://github.com/anthropics/claude-code/issues",
            "To provide feedback, users should report the issue at https://github.com/anthropics/claude-code/issues",
        ),
        ("error-11128", "11128", "11128", "11-128"),
    ];
    let mut entries: Vec<_> = rewrites
        .into_iter()
        .map(|(id, name, pattern, replacement)| SanitizeRule {
            id: id.to_string(),
            name: name.to_string(),
            enabled: true,
            match_type: MatchType::Literal,
            pattern: pattern.to_string(),
            replacement: replacement.to_string(),
            repeat: false,
            roles: Vec::new(),
        })
        .collect();
    // 键值删除必须先于裸键名改写；cc_ 删除重复执行以清理新暴露的相邻边界。
    for (id, name, pattern, replacement, repeat) in [
        (
            "billing-header-value",
            "x-anthropic-billing-header",
            r"(?i)x\u{200b}-anthropic-billing-header:[^;\n]*;?[\t\n\f\r ]*",
            "",
            false,
        ),
        (
            "cc-parameters",
            "cc_*=",
            r"(?i)(?-u:\b)cc_[a-z0-9_]+=[^;\n]*;?[\t\n\f\r ]*",
            "",
            true,
        ),
        (
            "billing-header-name",
            "x-anthropic-billing-header",
            r"(?i)x\u{200b}-anthropic-billing-header",
            "x-anthropic-billing-hdr",
            false,
        ),
    ] {
        entries.push(SanitizeRule {
            id: id.to_string(),
            name: name.to_string(),
            enabled: true,
            match_type: MatchType::Regex,
            pattern: pattern.to_string(),
            replacement: replacement.to_string(),
            repeat,
            roles: Vec::new(),
        });
    }
    // 身份句与 DSH 提示词指纹：上游按「连续文本」逐字匹配（不是语义审核），
    // 所以每条都只做**最小改写**，够让指纹失配即可，原文其余部分逐字保留 ——
    // 客户端自带的项目规范、工具说明不能因为清洗提示词而失真。
    for (id, name, pattern, replacement) in [
        (
            "agent-identity",
            "Agent identity",
            r"(?i)\bYou are\s+(?:an?\s+|the\s+)?(?:[\w.-]+\s+){0,3}?(?:agent|assistant|model|AI|LLM)?[\s,]*(?:powered by|built on|built by|created by|developed by|made by|trained by)[^\r\n]*?(?:\.\B|(?mR:$))",
            "You are a helpful AI assistant.",
        ),
        (
            "dsh-gui-description",
            "DeepSeek Harness GUI",
            r"(?i)You are interacting with the user through the DeepSeek[ \t]+Harness[ \t]+Web[ \t]+GUI whose URL the runtime context names\.",
            "You are interacting with the user through the DeepSeek Harness Web GUI whose URL is named in the runtime context.",
        ),
        (
            "dsh-file-reference",
            "DeepSeek Harness file reference",
            r"(?i)Tokens prefixed with @ are paths the user explicitly referenced\.",
            "Tokens prefixed with @ are paths that the user explicitly referenced.",
        ),
        (
            "dsh-file-reference-values",
            "DeepSeek Harness file reference (Values)",
            r"(?i)Values prefixed with @",
            "Values beginning with @",
        ),
        (
            "dsh-search-fetch",
            "DeepSeek Harness search/fetch",
            r"(?i)Searching and fetching are distinct operations",
            "Searching and fetching are separate operations",
        ),
        (
            "dsh-brand",
            "DeepSeek Harness",
            r"(?i)\bDeepSeek[ \t]+Harness\b",
            "DeepSeek",
        ),
    ] {
        entries.push(SanitizeRule {
            id: id.to_string(),
            name: name.to_string(),
            enabled: true,
            match_type: MatchType::Regex,
            pattern: pattern.to_string(),
            replacement: replacement.to_string(),
            repeat: false,
            roles: vec![
                SanitizeRole::System,
                SanitizeRole::Developer,
                SanitizeRole::Assistant,
            ],
        });
    }
    // issue #472：WorkBuddy 国内版对 Codex CLI 的**两种**首句都逐字拦截
    // （`…Codex CLI, …` 与 `…Codex CLI tool, …`）。旧规则只在两种写法之间
    // 换（literal 改写），等于没改。这里用正则同时覆盖两种，并替换成**中性**
    // 文本 —— 任何上游都不该从正文里认出客户端身份，这正是脱敏规则的目的。
    // 不匹配 `…Codex CLI.`（无逗号后缀的短句）：那条实测 200，动它反而
    // 会把能过的请求改坏。
    //
    // 单独 push 而不是进上面的循环：循环里的规则只申报 System / Developer /
    // Assistant 三个角色，而上游的拦截按**文本**扫描、不区分角色（同样的
    // 指纹出现在 user 内容里一样被拦）。原 literal 规则就是全角色（空 roles），
    // 这里保持一致。
    entries.push(SanitizeRule {
        id: "codex-cli-identity".to_string(),
        name: "Codex CLI".to_string(),
        enabled: true,
        match_type: MatchType::Regex,
        pattern: r"(?i)You are a coding agent running in the Codex CLI(?: tool)?, a terminal-based coding assistant\.".to_string(),
        replacement: "You are a helpful AI assistant.".to_string(),
        repeat: false,
        roles: Vec::new(),
    });
    entries
}

#[cfg(test)]
mod codex_identity_tests {
    use super::*;

    /// 用内置规则跑一遍 system 文本，返回改写结果。
    fn rewritten(text: &str) -> String {
        let rules = RuleSet::compile(default_entries()).expect("内置规则必须有效");
        let body = serde_json::json!({
            "messages": [{ "role": "system", "content": text }]
        });
        match crate::server::core::sanitize::sanitize_body(&body, &rules) {
            Some((next, _hits)) => next["messages"][0]["content"]
                .as_str()
                .expect("改写后仍是字符串")
                .to_string(),
            None => text.to_string(),
        }
    }

    /// issue #472 的核心断言：**两种**被拦写法经脱敏后都不得再出现。
    ///
    /// 旧规则只在两种写法之间换（`…CLI, …` → `…CLI tool, …`），而国内版上游对
    /// 两个都逐字拦截 —— 这条用例对两种输入都断言「改写后不含任一被拦指纹」，
    /// 旧规则在第二个断言上必红。
    #[test]
    fn both_blocked_codex_identity_variants_are_neutralised() {
        let blocked = [
            "You are a coding agent running in the Codex CLI, a terminal-based coding assistant.",
            "You are a coding agent running in the Codex CLI tool, a terminal-based coding assistant.",
        ];
        for text in blocked {
            let out = rewritten(text);
            assert!(
                !out.contains("Codex CLI"),
                "改写后不得再含被拦指纹。输入：{text}\n输出：{out}"
            );
            assert!(
                out.contains("helpful AI assistant"),
                "应当替换成中性文本。输入：{text}\n输出：{out}"
            );
        }
    }

    /// 大小写混合也要能匹配（正则带 `(?i)`）—— 客户端或中间层可能改写大小写。
    #[test]
    fn the_match_is_case_insensitive() {
        let out = rewritten(
            "YOU ARE A CODING AGENT RUNNING IN THE CODEX CLI TOOL, A TERMINAL-BASED CODING ASSISTANT.",
        );
        assert!(!out.contains("CODEX"), "实际：{out}");
    }

    /// 短句 `…Codex CLI.`（无逗号后缀）实测 **200**，不能动它 ——
    /// 改了反而把能过的请求改坏。
    #[test]
    fn the_short_variant_is_left_alone() {
        let text = "You are a coding agent running in the Codex CLI.";
        assert_eq!(rewritten(text), text, "短句不该被改写");
    }

    /// 完全无关的文本不该被这条规则碰到（`sanitize_body` 返回 None）。
    #[test]
    fn an_unrelated_text_is_untouched() {
        let text = "You are a helpful assistant.";
        assert_eq!(rewritten(text), text);
    }

    /// 长会话里首句只出现一次也生效（issue 的真实场景是 252 条消息）。
    #[test]
    fn it_works_inside_a_long_conversation() {
        let messages: Vec<serde_json::Value> = (0..250)
            .map(|index| {
                serde_json::json!({
                    "role": if index % 2 == 0 { "user" } else { "assistant" },
                    "content": format!("消息 {index}"),
                })
            })
            .chain(std::iter::once(serde_json::json!({
                "role": "user",
                "content": "You are a coding agent running in the Codex CLI, a terminal-based coding assistant. 请继续",
            })))
            .collect();
        let rules = RuleSet::compile(default_entries()).expect("内置规则必须有效");
        let body = serde_json::json!({ "messages": messages });
        let (next, hits) = crate::server::core::sanitize::sanitize_body(&body, &rules)
            .expect("长会话里应当有命中");
        let last = next["messages"].as_array().unwrap().last().unwrap();
        let content = last["content"].as_str().unwrap();
        assert!(!content.contains("Codex CLI"), "实际：{content}");
        assert_eq!(hits.len(), 1, "只应命中一次：{hits:?}");
    }
}
