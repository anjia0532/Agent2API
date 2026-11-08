//! 跨渠道聚合路由的**选型内核**（纯函数，零 IO、零网络）。
//!
//! ── 它解决什么问题 ────────────────────────────────────────
//! 同一个模型在多家渠道都有账号（`deepseek-v4.1-flash` 在 workbuddy、zcode、
//! trae… 上都能跑），其中一个渠道的积分明天到期、另一个的是永久积分。
//! **应该先烧明天到期的那个**，否则它作废了就白扔。本模块就是这条排序口径的
//! 唯一权威实现。
//!
//! ── 与参考实现的关系 ──────────────────────────────────────
//! 口径照搬 `dsh-providers-hub` 的 `src/aggregate-core.ts`（MIT / AGPL 同族项目），
//! 但**把哨兵换成了枚举**，理由见 [`Expiry`] 的注释 —— 那边用
//! `Infinity` / `-1` 两个魔法值区分「永久」「不可用」，历史上真出过一次
//! 「全部条目都是永久积分 ⇒ 循环一次都没赋值 ⇒ 返回 undefined ⇒ 用户看到
//! 『没有任何已登录且可用的账号』」的缺陷。枚举让这种状态在类型上不可能出现。
//!
//! ── 本模块**不含**什么 ────────────────────────────────────
//! 不含「怎么探某个渠道的临期时刻」（那是各家 provider 的知识，要发网络请求），
//! 也不含「怎么把候选拼成一次真实转发」。这里只回答**排序**这一个问题，
//! 好让它能被完整单测覆盖（无网络、无时钟依赖）。

/// 一个候选渠道的额度状态。
///
/// ── 为什么是枚举而不是 `f64` 哨兵 ────────────────────────────
/// 参考实现用 `NEVER = Infinity` 表示「可用但没有临期额度」、`UNUSABLE = -1`
/// 表示「本次不可用」。两者都是**合法数字**，于是「初值」与「真实值」撞在同一个
/// 取值上：`bestExpiry` 初值写成 `Infinity` 时，全部候选都是永久积分就会
/// **一次都不赋值**，最终返回 `undefined`，被上层渲染成「没有任何可用账号」——
/// 而账号其实全都正常。枚举把三种状态分开，初值为 `None` 就是「还没选」，
/// 与「选了但它是永久积分」互不混淆。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expiry {
    /// 可用，但**没有临期额度**（永久 / 长期积分）。
    ///
    /// 它**参与排序且能当选**，只是排在所有有具体到期时刻的候选之后 ——
    /// 「用不掉就浪费」的只有会过期的那部分。
    Never,
    /// 具体的失效时刻（毫秒时间戳，越小越先烧）。
    At(i64),
    /// 本次不可用（无启用账号 / 探测失败 / 渠道被停用）。
    ///
    /// **从候选池里剔除**，不是排在最后。
    Unusable,
}

impl Expiry {
    /// 排序键：`None` = 这个候选不参与（`Unusable`）。
    ///
    /// `At(t)` 得到具体值、`Never` 得到 `i64::MAX`（排在所有具体时刻之后），
    /// 两者都不会与「未选中」混淆。
    fn sort_key(self) -> Option<i64> {
        match self {
            Expiry::Unusable => None,
            Expiry::At(at) => Some(at),
            Expiry::Never => Some(i64::MAX),
        }
    }
}

/// 一个候选是不是参与本轮轮换（用户可逐渠道开关）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateGate {
    /// 参与候选池
    Allowed,
    /// 用户**显式拒绝**
    Rejected,
    /// 该渠道没接入临期折算、用户也没显式允许 ⇒ **默认不参与**
    DefaultRejected,
}

/// 门控判据的入参（刻意拆成三个布尔，便于逐格测）。
#[derive(Clone, Copy, Debug)]
pub struct GateInput {
    /// 在**显式拒绝**表里
    pub rejected: bool,
    /// 在**白名单**里
    pub allowed: bool,
    /// 该渠道是否已接入临期折算
    pub expiry_wired: bool,
}

/// 优先级：**显式拒绝 > 显式允许 > 默认**。
///
/// ── 为什么拒绝必须最优先 ────────────────────────────────────
/// 两张表的键域本来由 `expiry_wired` 划分（已接入的只进拒绝表、未接入的只进白名单），
/// 正常情况不会双命中。但跨域残留（渠道后来接入了折算、白名单旧条目还在）或外部
/// 直接改表时，双命中是可能的。此时若判「允许优先」，用户点「拒绝」会**没反应**
/// （界面上按钮像坏了）；判「拒绝优先」的最坏后果只是「拒绝生效、白名单留一条
/// 无作用的记录」，用户再点一次开关就回到自洽态。
pub fn resolve_gate(input: GateInput) -> CandidateGate {
    if input.rejected {
        return CandidateGate::Rejected;
    }
    if input.allowed {
        return CandidateGate::Allowed;
    }
    if input.expiry_wired {
        CandidateGate::Allowed
    } else {
        CandidateGate::DefaultRejected
    }
}

/// 从一批「渠道 → 额度状态」里挑出**最该先烧**的那个。
///
/// 规则：剔除 `Unusable`；其余按到期时刻升序；并列时**保留传入顺序**
/// （即候选表的声明顺序），不重排。
///
/// 返回 `None` **只在全部候选都 `Unusable` 时**发生 —— 全部是 `Never` 时
/// 仍会选出第一个（这正是参考实现踩过的那条缺陷）。
pub fn pick_soonest<'a, I>(entries: I) -> Option<&'a str>
where
    I: IntoIterator<Item = (&'a str, Expiry)>,
{
    let mut best: Option<(&str, i64)> = None;
    for (provider, expiry) in entries {
        let Some(key) = expiry.sort_key() else { continue };
        match best {
            // 严格小于：并列时保留先出现的
            Some((_, best_key)) if key >= best_key => {}
            _ => best = Some((provider, key)),
        }
    }
    best.map(|(provider, _)| provider)
}

/// 一条参与排序的候选。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedCandidate {
    /// 渠道 id
    pub provider: String,
    /// 该渠道下的**真实** model id（转发时用它）
    pub real_id: String,
    /// 该渠道的最早失效时刻
    pub expiry: Expiry,
    /// 倍率（`price_factor_from_name` 的结果，`None` = 目录名没标注）
    pub price: Option<u32>,
}

/// 从目录展示名里解析「倍率」标注。
///
/// ── 真实形态 ──────────────────────────────────────────────
/// 目录名里的倍率只有两种写法（见参考实现的 `formatCreditsRate` /
/// `loomyDisplayName`）：`… · x0.8` 与 `… ×2`，分隔符是**间隔号**
/// `U+00B7` 或乘号；促销期是 `· x0.17→x0.50`，取**箭头右侧**（实际计费倍率）。
/// 名字里带「免费」按 0。
///
/// ── 为什么必须要求分隔符 ───────────────────────────────────
/// 参考实现初版的正则只要求「`x` + 数字 + 行尾」，于是模型名里的 `x` 被当成倍率：
/// `Spark X2.5` → 2.5 倍、`GPT-X5` → 5 倍、`Grok x4` → 4 倍，全是**错**的。
/// 后果是「挑最便宜」反而挑到最贵的。所以这里**只在间隔号之后**认倍率。
///
/// 返回值是**千分位整数**（`0.8` → `Some(800)`）：`f64` 不能做 `Ord`，
/// 而候选排序要一个可全序比较的键。目录名里的倍率最多两位小数，放大 1000 倍
/// 不丢精度。
pub fn price_factor_from_name(name: &str) -> Option<u32> {
    if name.contains("免费") {
        return Some(0);
    }
    let trimmed = name.trim_end();
    // 分隔符必须是间隔号（U+00B7）；`×` 也可以直接出现，但它前面通常也有间隔号
    let separator = trimmed.rfind('\u{00B7}')?;
    let mut rest = trimmed[separator + '\u{00B7}'.len_utf8()..].trim();
    // 可选的乘号前缀：`×0.8` / `x0.8`
    for prefix in ['\u{00D7}', 'x', 'X'] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped.trim_start();
            break;
        }
    }
    let (first, after_first) = take_decimal(rest)?;
    // 促销箭头：取箭头右侧（实际计费倍率）
    for arrow in ['\u{2192}', '>'] {
        if let Some(tail) = after_first.strip_prefix(arrow) {
            let tail = tail.trim_start();
            let tail = ['\u{00D7}', 'x', 'X']
                .iter()
                .find_map(|prefix| tail.strip_prefix(*prefix))
                .unwrap_or(tail)
                .trim_start();
            let (second, _) = take_decimal(tail)?;
            return Some(scale(second));
        }
    }
    Some(scale(first))
}

/// 读一个十进制小数，返回 `(值, 剩余串)`。只吃 `123` / `1.5` 形态。
fn take_decimal(text: &str) -> Option<(f64, &str)> {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut seen_dot = false;
    while index < bytes.len() {
        match bytes[index] {
            b'0'..=b'9' => index += 1,
            b'.' if !seen_dot => {
                seen_dot = true;
                index += 1;
            }
            _ => break,
        }
    }
    if index == 0 || text[..index].ends_with('.') {
        return None;
    }
    let value: f64 = text[..index].parse().ok()?;
    let rest = text[index..].trim_end();
    // 必须停在行尾或箭头处，否则说明这不是一个「结尾的倍率标注」
    if !rest.is_empty() && !rest.starts_with('\u{2192}') && !rest.starts_with('>') {
        return None;
    }
    Some((value, rest))
}

fn scale(value: f64) -> u32 {
    let scaled = (value * 1000.0).round();
    if scaled <= 0.0 {
        0
    } else if scaled >= u32::MAX as f64 {
        u32::MAX
    } else {
        scaled as u32
    }
}

/// 候选排序：**手工位次严格优先 → 临期升序 → 同档倍率升序 → 声明顺序兜底**。
///
/// ── 手工位次为什么是「严格序」而不是「临期为主、手工为辅」──
/// 用户拖动候选时表达的是「我要这个渠道先被用」。若临期仍优先，拖动会被临期值
/// **静默覆盖** —— 面板显示的顺序与真实轮换顺序分叉，是最难查的一类不一致。
/// 所以排过的一律排在**全部**没排过的之前，即使它临期更晚。
///
/// ── 并列怎么处理 ─────────────────────────────────────────
/// `sort_by_key` 是**稳定**排序，完全并列时保持传入顺序，不需要显式兜底字段。
///
/// ── 为什么不原地排序 ─────────────────────────────────────
/// 适配器会对同一份候选表**反复排序**（每次请求一次）。原地排序会让顺序随调用
/// 次数漂移，所以先 `to_vec()`。
pub fn rank_candidates(
    candidates: &[RankedCandidate],
    manual_rank: Option<&dyn Fn(&RankedCandidate) -> Option<u32>>,
) -> Vec<RankedCandidate> {
    let mut ranked: Vec<RankedCandidate> = candidates
        .iter()
        .filter(|candidate| candidate.expiry != Expiry::Unusable)
        .cloned()
        .collect();
    ranked.sort_by_key(|candidate| {
        let manual = manual_rank.and_then(|rank| rank(candidate)).unwrap_or(u32::MAX);
        // `Unusable` 已被过滤掉，这里不会出现 None
        let expiry = candidate.expiry.sort_key().unwrap_or(i64::MAX);
        let price = candidate.price.unwrap_or(u32::MAX);
        (manual, expiry, price)
    });
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(provider: &str, expiry: Expiry, price: Option<u32>) -> RankedCandidate {
        RankedCandidate {
            provider: provider.to_string(),
            real_id: format!("{provider}-model"),
            expiry,
            price,
        }
    }

    /// 越早作废的先烧：这是整个功能的全部价值。
    #[test]
    fn the_soonest_expiring_candidate_comes_first() {
        let ranked = rank_candidates(
            &[
                candidate("permanent", Expiry::Never, None),
                candidate("next-week", Expiry::At(2_000), None),
                candidate("tomorrow", Expiry::At(1_000), None),
            ],
            None,
        );
        let order: Vec<&str> = ranked.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(order, vec!["tomorrow", "next-week", "permanent"]);
    }

    /// 永久积分是「可用但没用到期概念」，**能当选**，只是排最后。
    /// 参考实现曾因为把它和「不可用」混成一个哨兵，导致全部永久积分时
    /// 返回 `undefined`、被渲染成「没有任何可用账号」。
    #[test]
    fn permanent_credit_is_selectable_not_unusable() {
        let picked = pick_soonest(vec![("a", Expiry::Never), ("b", Expiry::Never)]);
        assert_eq!(picked, Some("a"), "全部永久积分时必须选出第一个，而不是 None");
    }

    /// 全部不可用才返回 None。
    #[test]
    fn only_all_unusable_yields_none() {
        assert_eq!(pick_soonest(vec![("a", Expiry::Unusable)]), None);
        assert_eq!(
            pick_soonest(vec![("a", Expiry::Unusable), ("b", Expiry::At(5))]),
            Some("b")
        );
    }

    /// 到期时刻并列时保留传入顺序，不重排。
    #[test]
    fn ties_keep_the_declaration_order() {
        let picked = pick_soonest(vec![("first", Expiry::At(7)), ("second", Expiry::At(7))]);
        assert_eq!(picked, Some("first"));
    }

    /// 同一临期档内挑便宜的。
    #[test]
    fn the_cheaper_multiplier_wins_within_one_expiry() {
        let ranked = rank_candidates(
            &[
                candidate("dear", Expiry::At(1_000), Some(1_500)),
                candidate("cheap", Expiry::At(1_000), Some(800)),
            ],
            None,
        );
        assert_eq!(ranked[0].provider, "cheap");
    }

    /// 手工位次是严格序：排过的排在全部没排过的之前，即使临期更晚。
    #[test]
    fn a_manual_rank_outranks_a_sooner_expiry() {
        let manual = |c: &RankedCandidate| -> Option<u32> {
            if c.provider == "manual" {
                Some(0)
            } else {
                None
            }
        };
        let ranked = rank_candidates(
            &[
                candidate("tomorrow", Expiry::At(1_000), None),
                candidate("manual", Expiry::At(9_999), None),
            ],
            Some(&manual),
        );
        assert_eq!(ranked[0].provider, "manual");
    }

    /// 不可用的候选直接从池里剔除，不是排最后。
    #[test]
    fn unusable_candidates_are_dropped_not_deprioritized() {
        let ranked = rank_candidates(
            &[
                candidate("dead", Expiry::Unusable, None),
                candidate("alive", Expiry::At(1_000), None),
            ],
            None,
        );
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].provider, "alive");
    }

    /// 倍率只认间隔号之后的标注 —— 这是参考实现踩过的真实缺陷。
    #[test]
    fn a_multiplier_requires_the_separator() {
        // 没有分隔符的「X2.5」「GPT-X5」「x4」都不是倍率
        assert_eq!(price_factor_from_name("Spark X2.5"), None);
        assert_eq!(price_factor_from_name("GPT-X5"), None);
        assert_eq!(price_factor_from_name("Grok x4"), None);
        // 有分隔号的才认
        assert_eq!(price_factor_from_name("DeepSeek V4 Flash 0731 \u{00B7} x3.0"), Some(3_000));
        assert_eq!(price_factor_from_name("GLM-5.3 \u{00B7} \u{00D7}0.8"), Some(800));
        // 促销取箭头右侧（实际计费倍率）
        assert_eq!(
            price_factor_from_name("Kimi \u{00B7} x0.17\u{2192}x0.50"),
            Some(500)
        );
        // 免费按 0
        assert_eq!(price_factor_from_name("Qwen 免费额度"), Some(0));
    }

    /// 门控优先级：显式拒绝 > 显式允许 > 默认。
    #[test]
    fn an_explicit_rejection_beats_an_explicit_allow() {
        assert_eq!(
            resolve_gate(GateInput { rejected: true, allowed: true, expiry_wired: true }),
            CandidateGate::Rejected
        );
        assert_eq!(
            resolve_gate(GateInput { rejected: false, allowed: true, expiry_wired: false }),
            CandidateGate::Allowed
        );
        // 已接入折算 + 用户没表态 ⇒ 默认参与
        assert_eq!(
            resolve_gate(GateInput { rejected: false, allowed: false, expiry_wired: true }),
            CandidateGate::Allowed
        );
        // 未接入折算 + 用户没表态 ⇒ 默认不参与
        assert_eq!(
            resolve_gate(GateInput { rejected: false, allowed: false, expiry_wired: false }),
            CandidateGate::DefaultRejected
        );
    }
}
