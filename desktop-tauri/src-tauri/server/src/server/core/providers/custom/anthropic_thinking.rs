//! Anthropic 上游的**思考参数规范化**（issue #526）。
//!
//! ── 问题 ────────────────────────────────────────────────────
//! 自定义提供商绑定思考等级时，`forward.rs` 会往请求体里注入 `reasoning_effort`
//! —— 那是 **OpenAI** 的字段名。而 `protocol=anthropic` 的上游要的是
//! `output_config.effort`，并且新一代模型（Opus 5 / Sonnet 5 / Fable 5）
//! **明确拒收**旧格式 `thinking: {type: "enabled", budget_tokens: N}`：
//!
//! ```text
//! 400 "thinking.type.enabled" is not supported for this model.
//!     Use "thinking.type.adaptive" and "output_config.effort" to control thinking behavior.
//! ```
//! 症状是该模型**全部请求 400**，而直连上游完全正常 —— 因为客户端本身发的是
//! `adaptive`，是网关把它折成了旧格式。
//!
//! ── 边界：绝不为了修 400 去动 messages ─────────────────────────
//! 历史里的 `thinking` 块带 `signature`，剥掉会换来另一个 400
//! （`messages.1.content.0.thinking.signature: Field required`）。所以本模块
//! **只碰顶层 generation 参数**，`messages` 一个字节都不动。
//!
//! ── 为什么「旧格式 → adaptive」要按模型白名单 ─────────────────
//! 更早的模型（Sonnet 4 那代）**正常接受** `enabled` + `budget_tokens`。
//! 无条件改写会把它们弄坏 —— 那等于用修 A 家的方式砸了 B 家。而我们只在
//! 确认需要 adaptive 的世代上改写，名单见 [`adaptive_only`]。

use serde_json::{Map, Value};

/// 只接受 `thinking.type = "adaptive"` 的模型世代。
///
/// ── 名单为什么这么短 ────────────────────────────────────────
/// 这份名单只放**已知会拒收 `enabled`** 的世代。issue #526 的报告里是
/// `claude-opus-5-5`；同代及更高的 Opus / Sonnet / Fable 5 一并纳入。
/// 更早的世代（4.x 及以下）继续接受 `enabled`，**不在名单里就不改写**。
///
/// 判断用「包含」而不是精确相等：上游模型名带版本后缀（`claude-opus-5-5`、
/// `claude-opus-5.5`、`claude-sonnet-5-20260101` 都有），逐个列全会漏。
pub fn adaptive_only(model: &str) -> bool {
    let lowered = model.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return false;
    }
    ["claude-opus-5", "claude-sonnet-5", "claude-fable-5"]
        .iter()
        .any(|family| lowered.contains(family))
}

/// 把顶层 generation 参数规范化成 Anthropic 现在要的形状。
///
/// 做四件事（都只碰顶层键）：
/// 1. **删掉 `reasoning_effort`** —— 这是 OpenAI 的字段名，Anthropic 端点不认；
/// 2. 该模型只吃 `adaptive` 时，把 `thinking: {type:"enabled", …}` 换成
///    `{type:"adaptive"}` 并**丢掉 `budget_tokens`**（adaptive 下它无意义，
///    留着会被当成未知字段）；
/// 3. 把思考等级写进 `output_config.effort`（合并进已有对象，保留 `format`
///    等其它键）；
/// 4. 客户端明确写了 `thinking: {type:"disabled"}` 时**不写 effort** ——
///    用户主动关了思考，网关不该再把它打开。
pub fn normalize(object: &mut Map<String, Value>, level: &str, model: &str) {
    // 1) OpenAI 字段名一律移除（不论模型：Anthropic 端点没有这个字段）
    object.remove("reasoning_effort");

    // 2) 旧格式 → adaptive（仅限只吃 adaptive 的世代）
    let disabled_by_client = object
        .get("thinking")
        .and_then(|thinking| thinking.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("disabled"));
    if adaptive_only(model) {
        let needs_rewrite = match object.get("thinking") {
            None => true,
            Some(thinking) => thinking
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("enabled")),
        };
        if needs_rewrite {
            object.insert("thinking".to_string(), serde_json::json!({ "type": "adaptive" }));
        } else if let Some(thinking) = object.get_mut("thinking").and_then(Value::as_object_mut) {
            // 已经是 adaptive（或别的类型）：只把 adaptive 下无意义的 budget_tokens 摘掉
            if thinking
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("adaptive"))
            {
                thinking.remove("budget_tokens");
            }
        }
    }

    // 3) 等级写进 output_config.effort；客户端已关闭思考时不写
    if disabled_by_client {
        return;
    }
    let level = level.trim();
    if level.is_empty() {
        return;
    }
    match object.get_mut("output_config") {
        Some(Value::Object(config)) => {
            config.insert("effort".to_string(), Value::String(level.to_string()));
        }
        // 存在但不是对象（畸形请求体）：整个替换，别在错误形态上做手术
        Some(other) => {
            *other = serde_json::json!({ "effort": level });
        }
        None => {
            object.insert(
                "output_config".to_string(),
                serde_json::json!({ "effort": level }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("测试入参是对象")
    }

    fn run(value: Value, level: &str, model: &str) -> Value {
        let mut object = map(value);
        normalize(&mut object, level, model);
        Value::Object(object)
    }

    /// issue #526 的核心场景：旧格式 + Opus 5 → adaptive + effort，且不再有
    /// `reasoning_effort`。
    #[test]
    fn a_legacy_enabled_block_becomes_adaptive_on_the_five_generation() {
        let out = run(
            json!({
                "model": "claude-opus-5-5",
                "thinking": { "type": "enabled", "budget_tokens": 1024 },
                "reasoning_effort": "medium",
                "messages": [{ "role": "user", "content": "ok" }]
            }),
            "medium",
            "claude-opus-5-5",
        );
        assert_eq!(out["thinking"], json!({ "type": "adaptive" }));
        assert!(out["thinking"].get("budget_tokens").is_none(), "adaptive 下不该留 budget_tokens");
        assert_eq!(out["output_config"], json!({ "effort": "medium" }));
        assert!(out.get("reasoning_effort").is_none(), "Anthropic 端点不该收到 reasoning_effort");
    }

    /// 客户端已经发的就是 adaptive：保留，但也要把 effort 补上（否则档位丢失）。
    #[test]
    fn an_adaptive_block_keeps_its_type_and_gains_the_effort() {
        let out = run(
            json!({ "thinking": { "type": "adaptive" } }),
            "high",
            "claude-sonnet-5",
        );
        assert_eq!(out["thinking"], json!({ "type": "adaptive" }));
        assert_eq!(out["output_config"]["effort"], json!("high"));
    }

    /// 旧的 4.x 世代**接受** enabled，不能被改写 —— 否则「修好 A 砸坏 B」。
    #[test]
    fn an_older_generation_keeps_its_enabled_block() {
        let out = run(
            json!({ "thinking": { "type": "enabled", "budget_tokens": 2048 } }),
            "medium",
            "claude-sonnet-4-20250514",
        );
        assert_eq!(
            out["thinking"],
            json!({ "type": "enabled", "budget_tokens": 2048 }),
            "4.x 接受 enabled，一个字段都不该动"
        );
        // 但 OpenAI 字段名仍然要清掉（那是所有 Anthropic 端点都不认的）
        assert_eq!(out["output_config"]["effort"], json!("medium"));
    }

    /// 客户端主动关掉思考时，网关不该再写 effort 把它打开。
    #[test]
    fn a_client_disabled_thinking_is_respected() {
        let out = run(
            json!({ "thinking": { "type": "disabled" } }),
            "medium",
            "claude-opus-5-5",
        );
        assert_eq!(out["thinking"], json!({ "type": "disabled" }));
        assert!(out.get("output_config").is_none(), "客户端关了思考，就不该注入 effort");
    }

    /// `output_config` 已有的其它键必须保留（例如结构化输出的 format）。
    #[test]
    fn other_output_config_keys_survive() {
        let out = run(
            json!({ "output_config": { "format": { "type": "json_schema" } } }),
            "max",
            "claude-opus-5-5",
        );
        assert_eq!(out["output_config"]["effort"], json!("max"));
        assert_eq!(out["output_config"]["format"], json!({ "type": "json_schema" }));
    }

    /// `messages` 一个字节都不动 —— 历史 thinking 块带签名，剥掉会换来另一个 400。
    #[test]
    fn messages_are_never_touched() {
        let messages = json!([{ "role": "assistant", "content": [
            { "type": "thinking", "thinking": "…", "signature": "sig-abc" }
        ]}]);
        let out = run(
            json!({ "thinking": { "type": "enabled", "budget_tokens": 1024 }, "messages": messages.clone() }),
            "medium",
            "claude-opus-5-5",
        );
        assert_eq!(out["messages"], messages, "历史块与签名必须原样保留");
    }

    /// 名单判断：带版本后缀的各种写法都要认，4.x 不认。
    #[test]
    fn the_generation_gate_covers_suffixes_and_excludes_older_ones() {
        for model in ["claude-opus-5-5", "claude-opus-5.5", "claude-sonnet-5-20260101", "claude-fable-5.1"] {
            assert!(adaptive_only(model), "{model} 应当只吃 adaptive");
        }
        for model in ["claude-sonnet-4-20250514", "claude-opus-4-5", "gpt-5", ""] {
            assert!(!adaptive_only(model), "{model} 不该被改写");
        }
    }

    /// 没有思考字段、模型也不在名单里时：只清 OpenAI 字段 + 写 effort。
    #[test]
    fn a_plain_body_only_gets_the_openai_field_removed() {
        let out = run(
            json!({ "reasoning_effort": "low", "max_tokens": 16 }),
            "medium",
            "claude-sonnet-4-20250514",
        );
        assert!(out.get("reasoning_effort").is_none());
        assert_eq!(out["output_config"]["effort"], json!("medium"));
        assert!(out.get("thinking").is_none(), "没有思考字段时不该凭空造一个");
        assert_eq!(out["max_tokens"], json!(16));
    }
}
