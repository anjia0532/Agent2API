//! 聚合路由的**名字解析与模型族匹配**（纯函数，零 IO）。
//!
//! ── 它解决什么问题 ─────────────────────────────────────────
//! 客户端（Claude Code / Codex / Cherry Studio…）只能填一个模型名，而「同一个
//! 模型在多家渠道都有账号」这件事是网关侧的知识。聚合路由让用户填一个**虚拟
//! 名字**，由网关把它展开成一组真实候选再按 [`super::aggregate_core`] 的规则
//! 排序选一个：
//!
//! | 用户填的 | 含义 |
//! |---|---|
//! | `auto` | 任意可用模型，跨所有渠道 |
//! | `auto/deepseek` | 只在 DeepSeek 系列里挑 |
//! | `auto/qwen` | 只在 Qwen 系列里挑 |
//! | `auto/kimi` | 只在 Kimi / Moonshot 系列里挑 |
//!
//! ── 为什么族匹配要「数据驱动」──────────────────────────────
//! 上游的模型命名一直在变（`qwen3.7-max`、`Qwen/Qwen3-Max`、`qwen-turbo`…）。
//! 把规则写成一串 `if` 之后每加一家都要改函数体，容易漏；这里做成一张表
//! （[`FAMILIES`]），加族就是加一行，且能被用例逐条覆盖。
//!
//! ── 安全边界：**绝不把真实模型名误判成聚合名** ─────────────────
//! 判错的后果不对称：把真实模型名当成聚合名 ⇒ 请求被路由到完全不同的模型，
//! 用户拿到答非所问的结果且很难发现；把聚合名当成真实模型名 ⇒ 上游回一个
//! 「模型不存在」，用户立刻知道。
//! 所以：只有**恰好等于** `auto`、`auto/<族>`、`aggregate/<族>`，或**恰好等于**
//! 某个族键（`deepseek` 这种裸名）时才算聚合名；带任何后缀（`auto-x`、
//! `deepseek-v4`）一律不当聚合。

/// 一个模型族。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Family {
    /// 用户填的键（同时是裸名匹配的唯一依据）
    pub key: &'static str,
    /// 界面展示名
    pub label: &'static str,
    /// 模型 id 的**前缀**匹配表（大小写不敏感；比对前先剥掉 `namespace/`）
    pub prefixes: &'static [&'static str],
}

/// 支持的模型族。
///
/// 加一族的成本：这里加一行 + README 支持矩阵加一句。用例
/// `every_family_key_resolves_and_matches_its_own_models` 会自动覆盖新行。
pub const FAMILIES: &[Family] = &[
    Family { key: "deepseek", label: "DeepSeek", prefixes: &["deepseek"] },
    Family { key: "qwen", label: "Qwen / 通义千问", prefixes: &["qwen", "tongyi"] },
    Family { key: "kimi", label: "Kimi / Moonshot", prefixes: &["kimi", "moonshot"] },
    Family { key: "glm", label: "GLM / 智谱", prefixes: &["glm", "z-ai", "chatglm"] },
    Family { key: "minimax", label: "MiniMax", prefixes: &["minimax", "abab"] },
    Family { key: "claude", label: "Claude", prefixes: &["claude"] },
    Family { key: "gpt", label: "GPT / OpenAI", prefixes: &["gpt", "o1", "o3", "o4"] },
    Family { key: "gemini", label: "Gemini", prefixes: &["gemini"] },
    Family { key: "doubao", label: "豆包 / Doubao", prefixes: &["doubao", "seed"] },
];

/// 聚合名的解析结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregateSpec {
    /// `auto`：任意可用模型
    Auto,
    /// `auto/<族>`：限定在某一族
    Family(&'static Family),
}

impl AggregateSpec {
    /// 这个虚拟名是否接受该真实模型 id。
    pub fn accepts(&self, model_id: &str) -> bool {
        match self {
            Self::Auto => true,
            Self::Family(family) => family.matches(model_id),
        }
    }
}

/// 剥掉 `namespace/` 前缀并转小写（`Qwen/Qwen3-Max` → `qwen3-max`）。
///
/// 一次性做两件事：上游有的下发 `vendor/model`（OpenRouter 风格）、有的下发裸名，
/// 而大小写也不统一。先归一再比前缀，规则表就只需要写一遍。
fn bare_lower(model_id: &str) -> String {
    let trimmed = model_id.trim();
    let bare = trimmed.rsplit('/').next().unwrap_or(trimmed);
    bare.to_ascii_lowercase()
}

impl Family {
    /// 该模型 id 是否属于这一族。
    pub fn matches(&self, model_id: &str) -> bool {
        let bare = bare_lower(model_id);
        if bare.is_empty() {
            return false;
        }
        self.prefixes.iter().any(|prefix| {
            if bare.starts_with(prefix) {
                return true;
            }
            // `namespace/vendor-model` 这种形态：剥前缀后第一段可能才是厂牌
            let full = model_id.trim().to_ascii_lowercase();
            full.starts_with(prefix)
        })
    }
}

fn family_by_key(key: &str) -> Option<&'static Family> {
    FAMILIES.iter().find(|family| family.key == key)
}

/// 解析用户填的模型名是不是聚合名。
///
/// 返回 `None` 表示「这是个普通模型名，不要当聚合处理」。
pub fn parse(requested: &str) -> Option<AggregateSpec> {
    let name = requested.trim();
    if name.is_empty() {
        return None;
    }
    // 精确匹配 auto：带后缀（`auto-x`）不是聚合名
    if name.eq_ignore_ascii_case("auto") {
        return Some(AggregateSpec::Auto);
    }
    // `auto/<族>` 或 `aggregate/<族>`：只有这两段形态才算
    for namespace in ["auto/", "aggregate/"] {
        if name.len() > namespace.len() {
            let (head, tail) = name.split_at(namespace.len());
            if head.eq_ignore_ascii_case(namespace) && !tail.contains('/') {
                // 族名不认识时**不是**聚合名，交回普通路径 ——
                // 否则 `auto/some-real-model` 会被当成聚合的空白名单，用户拿到
                // 「没有任何可用账号」而不是「模型不存在」，更难排查
                return family_by_key(&tail.to_ascii_lowercase()).map(AggregateSpec::Family);
            }
        }
    }
    // 裸族键（`deepseek` / `qwen` …）：只有**恰好等于**族键才算
    family_by_key(&name.to_ascii_lowercase()).map(AggregateSpec::Family)
}

/// 从一组 `(提供商标识, 真实模型 id)` 里挑出该聚合名接受的候选。
///
/// 「提供商标识」只透传不动 —— 本模块不关心它是什么（provider id / 账号 /
/// 渠道名都行），调用方原样拿回去做转发。
pub fn expand<'a, I>(spec: AggregateSpec, candidates: I) -> Vec<(&'a str, &'a str)>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut out: Vec<(&str, &str)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (provider, model) in candidates {
        if !spec.accepts(model) {
            continue;
        }
        // 同一个 (provider, model) 可能因为多个账号出现多次：留一份就够，
        // 账号维度的选择是排序层的事，不是展开层的事
        if !seen.insert((provider, model)) {
            continue;
        }
        out.push((provider, model));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_parses_case_insensitively_and_exactly() {
        assert_eq!(parse("auto"), Some(AggregateSpec::Auto));
        assert_eq!(parse("AUTO"), Some(AggregateSpec::Auto));
        assert_eq!(parse("  auto  "), Some(AggregateSpec::Auto));
        // 带后缀的不是聚合名：`auto-x` 可能真是某个模型
        assert_eq!(parse("auto-x"), None);
        assert_eq!(parse("autopilot"), None);
    }

    #[test]
    fn a_family_can_be_written_with_or_without_a_namespace() {
        for text in ["auto/deepseek", "aggregate/deepseek", "deepseek", "DeepSeek"] {
            let spec = parse(text).unwrap_or_else(|| panic!("{text} 应当解析成聚合名"));
            assert_eq!(spec, AggregateSpec::Family(&FAMILIES[0]), "{text}");
        }
    }

    /// 不认识的族名**不能**当成聚合名 —— 否则用户会看到「没有任何可用账号」，
    /// 而不是「模型不存在」，排查方向完全不同。
    #[test]
    fn an_unknown_family_falls_back_to_a_normal_model_name() {
        assert_eq!(parse("auto/not-a-family"), None);
        assert_eq!(parse("aggregate/whatever"), None);
        assert_eq!(parse("deepseek-v4.1-flash"), None);
        assert_eq!(parse("qwen3.7-max"), None);
    }

    #[test]
    fn a_family_matches_namespaced_and_bare_ids_in_any_case() {
        let deepseek = &FAMILIES[0];
        for model in ["deepseek-chat", "DeepSeek-V4-Flash", "deepseek/deepseek-r1"] {
            assert!(deepseek.matches(model), "{model} 应当属于 DeepSeek 族");
        }
        assert!(!deepseek.matches("qwen3-max"));
        assert!(!deepseek.matches(""));
    }

    /// `namespace/vendor-model` 形态也要认：剥掉 `namespace/` 之后前缀能命中。
    #[test]
    fn a_vendored_id_is_matched_on_its_bare_tail() {
        let qwen = FAMILIES.iter().find(|f| f.key == "qwen").unwrap();
        assert!(qwen.matches("Qwen/Qwen3-Max"));
        assert!(qwen.matches("qwen3.7-plus"));
        assert!(!qwen.matches("claude-sonnet-5"));
    }

    /// 表里每一族都必须：能用自己的键解析、能命中至少一个自家模型、不误吞别家。
    #[test]
    fn every_family_key_resolves_and_matches_its_own_models() {
        for family in FAMILIES {
            assert_eq!(
                parse(family.key),
                Some(AggregateSpec::Family(family)),
                "族键 {} 解析不回来",
                family.key
            );
            let sample = format!("{}-sample-model", family.prefixes[0]);
            assert!(family.matches(&sample), "{} 应当命中自家样例 {sample}", family.key);
            // 不能吞掉一个明显不属于自己的名字
            assert!(
                !family.matches("totally-unrelated-model-xyz"),
                "{} 不该命中无关模型",
                family.key
            );
        }
    }

    /// `auto` 接受一切；族只接受自家。
    #[test]
    fn expand_filters_and_dedups_by_provider_and_model() {
        let candidates = vec![
            ("workbuddy", "deepseek-v4.1-flash"),
            ("code", "deepseek-v4.1-flash"),
            ("workbuddy", "deepseek-v4.1-flash"), // 重复：多个账号
            ("workbuddy", "qwen3.7-max"),
            ("trae", "kimi-k3.1-pro"),
        ];
        let all = expand(AggregateSpec::Auto, candidates.clone());
        assert_eq!(all.len(), 4, "auto 接受全部，但同 (provider, model) 只留一份");

        let deepseek = expand(parse("auto/deepseek").unwrap(), candidates.clone());
        assert_eq!(
            deepseek,
            vec![("workbuddy", "deepseek-v4.1-flash"), ("code", "deepseek-v4.1-flash")]
        );

        let kimi = expand(parse("auto/kimi").unwrap(), candidates);
        assert_eq!(kimi, vec![("trae", "kimi-k3.1-pro")]);
    }

    /// 一族一个候选都没有时给空表（调用方据此回「该族没有可用账号」，
    /// 而不是静默换到别的族）。
    #[test]
    fn an_empty_result_is_an_empty_list_not_a_fallback() {
        let candidates = vec![("workbuddy", "deepseek-v4.1-flash")];
        assert!(expand(parse("auto/gemini").unwrap(), candidates).is_empty());
    }
}

// ── 组合层：族匹配 → 排序 ──────────────────────────────────────

use super::aggregate_core::{rank_candidates, Expiry, RankedCandidate};

/// 一条**可参与聚合选型**的真实候选。
///
/// `provider` / `real_id` 之外的两项（`expiry` / `price`）由调用方从各渠道的
/// 目录与额度状态里采出来 —— 本模块不做任何探测，理由见模块头。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Available {
    /// 渠道 id（转发时用它选账号）
    pub provider: String,
    /// 该渠道下的真实 model id（转发时用它替换虚拟名）
    pub real_id: String,
    /// 该渠道这张卡/这份额度的最早失效时刻
    pub expiry: Expiry,
    /// 倍率（千分位整数，见 `aggregate_core::price_factor_from_name`）
    pub price: Option<u32>,
}

impl Available {
    /// 便于调用方就地构造。
    pub fn new(provider: impl Into<String>, real_id: impl Into<String>, expiry: Expiry, price: Option<u32>) -> Self {
        Self { provider: provider.into(), real_id: real_id.into(), expiry, price }
    }
}

/// 聚合选型的**最终决策**：给定虚拟名与全部真实候选，返回按优先级排好的目标表。
///
/// ## 顺序即优先级
/// 返回的第一条就是**该用**的候选；后续几条是失败后依次回退的顺序。
/// 排序规则完全交给 [`rank_candidates`]（临期升序 → 同档倍率升序 → 手工位次
/// 严格优先），本函数只负责「按族筛掉不该参与的」。
///
/// ## 为什么不用 `expand` + 再排
/// `expand` 返回 `(&str, &str)` 对，丢掉了 `expiry` / `price` —— 那正是排序仅有的
/// 两个依据。所以排序这条路上直接用 [`Available`]，`expand` 留给只关心成员集合
/// 的场景（例如界面上「这一族有哪些模型」）。
///
/// ## 空表
/// 一族一个可用候选都没有时返回空表，**不回退到别的族**：用户写了
/// `auto/kimi` 却拿到 DeepSeek 的回答，比收到一个明确的「Kimi 没有可用账号」糟得多。
pub fn plan(
    spec: AggregateSpec,
    available: &[Available],
    manual_rank: Option<&dyn Fn(&RankedCandidate) -> Option<u32>>,
) -> Vec<RankedCandidate> {
    let matched: Vec<RankedCandidate> = available
        .iter()
        .filter(|item| spec.accepts(&item.real_id))
        .map(|item| RankedCandidate {
            provider: item.provider.clone(),
            real_id: item.real_id.clone(),
            expiry: item.expiry,
            price: item.price,
        })
        .collect();
    rank_candidates(&matched, manual_rank)
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn available(provider: &str, real_id: &str, expiry: Expiry, price: Option<u32>) -> Available {
        Available::new(provider, real_id, expiry, price)
    }

    /// 族过滤与临期排序**同时**生效：这是组合层存在的全部理由。
    #[test]
    fn a_family_plan_filters_then_orders_by_expiry() {
        let candidates = vec![
            available("workbuddy", "deepseek-v4.1-flash", Expiry::Never, None),
            available("code", "deepseek-v4.1-flash", Expiry::At(1_000), None),
            available("trae", "deepseek-v4.1-flash", Expiry::At(2_000), None),
            // 不属这一族，必须被筛掉
            available("workbuddy", "qwen3.7-max", Expiry::At(500), None),
        ];
        let plan = plan(parse("auto/deepseek").unwrap(), &candidates, None);
        let order: Vec<&str> = plan.iter().map(|row| row.provider.as_str()).collect();
        assert_eq!(order, vec!["code", "trae", "workbuddy"], "早失效的先烧，永久的最后、且没有 qwen");
    }

    /// 同族同临期时按倍率升序（挑便宜的先烧）。
    #[test]
    fn a_plan_breaks_expiry_ties_by_price() {
        let candidates = vec![
            available("dear", "deepseek-v4.1-flash", Expiry::At(1_000), Some(1_500)),
            available("cheap", "deepseek-v4.1-flash", Expiry::At(1_000), Some(500)),
        ];
        let plan = plan(parse("auto/deepseek").unwrap(), &candidates, None);
        assert_eq!(plan[0].provider, "cheap");
    }

    /// `auto` 不筛族，全部参与排序。
    #[test]
    fn auto_includes_every_family() {
        let candidates = vec![
            available("a", "deepseek-v4.1-flash", Expiry::At(2_000), None),
            available("b", "qwen3.7-max", Expiry::At(1_000), None),
            available("c", "kimi-k3.1-pro", Expiry::At(3_000), None),
        ];
        let plan = plan(AggregateSpec::Auto, &candidates, None);
        let order: Vec<&str> = plan.iter().map(|row| row.provider.as_str()).collect();
        assert_eq!(order, vec!["b", "a", "c"]);
    }

    /// `Unusable` 的候选不进结果（那是「本次不可用」，不是「排在最后」）。
    #[test]
    fn unusable_candidates_never_reach_the_plan() {
        let candidates = vec![
            available("dead", "deepseek-v4.1-flash", Expiry::Unusable, None),
            available("live", "deepseek-v4.1-flash", Expiry::Never, None),
        ];
        let plan = plan(parse("auto/deepseek").unwrap(), &candidates, None);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].provider, "live");
    }

    /// 手工位次严格优先于临期：用户拖过的渠道必须排在最前，哪怕它临期更晚。
    #[test]
    fn a_manual_rank_outranks_expiry() {
        let candidates = vec![
            available("soon", "deepseek-v4.1-flash", Expiry::At(1_000), None),
            available("dragged", "deepseek-v4.1-flash", Expiry::At(9_999), None),
        ];
        let manual = |row: &RankedCandidate| -> Option<u32> {
            (row.provider == "dragged").then_some(0)
        };
        let plan = plan(parse("auto/deepseek").unwrap(), &candidates, Some(&manual));
        assert_eq!(plan[0].provider, "dragged");
    }

    /// 一族全空 ⇒ 空表，**不回退到别的族**。
    #[test]
    fn an_empty_family_returns_an_empty_plan_without_falling_back() {
        let candidates = vec![available("a", "deepseek-v4.1-flash", Expiry::Never, None)];
        let plan = plan(parse("auto/kimi").unwrap(), &candidates, None);
        assert!(plan.is_empty(), "写了 kimi 就不该拿到 deepseek 的候选");
    }
}

/// 把一个聚合虚拟名落成**一个真实模型名**。
///
/// 输入是 `(渠道, 真实模型 id)` 的全部对；返回挑中的模型名。
///
/// ## 挑选规则
/// 1. 先按族过滤（`auto` 不过滤）；
/// 2. 按「能承载它的渠道数」降序 —— 渠道越多，越不容易因为某一家没账号或被
///    停用而失败；
/// 3. 渠道数相同时按模型名升序，保证同一份目录**每次挑到同一个**。
///
/// ## 为什么必须确定（不能「随便挑一个」）
/// 目录刷新是会变的，但两次刷新之间同一请求应该打到同一个模型。若按哈希或
/// 遍历顺序挑，会出现「上一秒答得好、下一秒答得差」这种无法复现的抖动。
///
/// 返回 `None` 表示该聚合名当前没有任何候选（调用方据此回「没有可用模型」，
/// 而不是静默回退到别的族）。
pub fn pick_model<'a, I>(spec: AggregateSpec, pairs: I) -> Option<String>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut counts: std::collections::BTreeMap<String, std::collections::BTreeSet<&str>> =
        std::collections::BTreeMap::new();
    for (provider, model) in pairs {
        if !spec.accepts(model) {
            continue;
        }
        counts.entry(model.to_string()).or_default().insert(provider);
    }
    // BTreeMap 已按模型名升序；再按渠道数降序取最大即可得到确定结果
    counts
        .into_iter()
        .max_by(|left, right| {
            left.1
                .len()
                .cmp(&right.1.len())
                // 家数相同时**名字小的赢**：`max_by` 取后者更大，所以反向比较
                .then_with(|| right.0.cmp(&left.0))
        })
        .map(|(model, _)| model)
}

#[cfg(test)]
mod pick_model_tests {
    use super::*;

    /// 家数多的赢。
    #[test]
    fn the_model_offered_by_more_providers_wins() {
        let pairs = vec![
            ("a", "deepseek-v4.1-flash"),
            ("b", "deepseek-v4.1-flash"),
            ("c", "deepseek-v4-flash"),
            ("d", "qwen3.7-max"),
        ];
        assert_eq!(
            pick_model(parse("auto/deepseek").unwrap(), pairs).as_deref(),
            Some("deepseek-v4.1-flash")
        );
    }

    /// 家数相同时按名字升序，结果确定（同一份目录每次同一个答案）。
    #[test]
    fn ties_are_broken_by_name_so_the_choice_is_stable() {
        let pairs = vec![("a", "deepseek-v4-flash"), ("b", "deepseek-v4.1-flash")];
        let first = pick_model(parse("auto/deepseek").unwrap(), pairs.clone());
        let second = pick_model(parse("auto/deepseek").unwrap(), pairs);
        assert_eq!(first, second, "两次调用必须一致");
        assert_eq!(first.as_deref(), Some("deepseek-v4-flash"), "字典序靠前的赢");
    }

    /// 同一渠道报了两遍不该让它显得「有更多家」。
    #[test]
    fn the_same_provider_counted_twice_does_not_inflate_its_weight() {
        let pairs = vec![
            ("a", "deepseek-old"),
            ("a", "deepseek-old"),
            ("a", "deepseek-old"),
            ("b", "deepseek-new"),
            ("c", "deepseek-new"),
        ];
        assert_eq!(
            pick_model(parse("auto/deepseek").unwrap(), pairs).as_deref(),
            Some("deepseek-new"),
            "new 有两家、old 只有一家（重复三次仍是一家）"
        );
    }

    /// 一族全空 ⇒ None（调用方必须回「没有可用模型」，不能回退到别的族）。
    #[test]
    fn an_empty_family_yields_none() {
        let pairs = vec![("a", "deepseek-v4.1-flash")];
        assert_eq!(pick_model(parse("auto/kimi").unwrap(), pairs), None);
    }

    /// `auto` 不限族。
    #[test]
    fn auto_considers_every_family() {
        let pairs = vec![("a", "kimi-k3.1-pro"), ("b", "kimi-k3.1-pro"), ("c", "qwen3.7-max")];
        assert_eq!(pick_model(AggregateSpec::Auto, pairs).as_deref(), Some("kimi-k3.1-pro"));
    }
}
